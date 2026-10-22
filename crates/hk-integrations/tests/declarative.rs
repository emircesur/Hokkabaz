//! Every declarative spec, every operation, against a local server: the request line, auth,
//! query and body that each operation sends must match what its spec declares. Catches typos in
//! the spec corpus (placeholder without a parameter, wrong send target, missing auth) without
//! calling any real service.
#![cfg(feature = "int-saas")]

mod common;

use std::sync::Arc;

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::declarative::{OpSpec, ParamSpec, Spec, register_one, specs};
use hk_integrations::{CredentialTypes, register_integrations};
use serde_json::{Map, Value, json};

use common::{Creds, http_resp, serve};

fn sample(p: &ParamSpec) -> Value {
    match p.ty.as_deref().unwrap_or("string") {
        "number" => json!(3),
        "boolean" => json!(true),
        "json" => json!(r#"{"a":1}"#),
        "options" => p.options.first().map(|(_, v)| v.clone()).unwrap_or(json!("x")),
        "dateTime" => json!("2024-01-02T03:04:05Z"),
        _ => json!(format!("v_{}", p.name)),
    }
}

fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |c, k| c.get(k))
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len() + 0
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn cred_data(s: &Spec) -> Value {
    let mut m = Map::new();
    for f in &s.cred.fields {
        m.insert(f.name.clone(), json!(format!("cv_{}", f.name)));
    }
    // oauth credentials: the minted token
    m.insert("accessToken".into(), json!("cv_accessToken"));
    Value::Object(m)
}

#[test]
fn every_spec_parses_and_registers_once() {
    let all = specs();
    assert!(!all.is_empty());
    let mut nodes = NodeRegistry::new();
    let creds = CredentialTypes::new();
    register_integrations(&mut nodes, &creds).unwrap();
    let mut seen = std::collections::HashSet::new();
    for s in &all {
        assert!(seen.insert(s.node_type.clone()), "duplicate type {}", s.node_type);
        assert!(nodes.latest(&s.node_type).is_some(), "{} not registered", s.name);
        assert!(creds.get(&s.cred.name).is_some(), "{} credential", s.name);
        for r in &s.resources {
            let mut ops = std::collections::HashSet::new();
            for o in &r.ops {
                assert!(ops.insert(&o.name), "{}: duplicate op {}.{}", s.name, r.name, o.name);
                // every {placeholder} has a parameter of that name
                let mut rest = o.path.as_str();
                while let Some(i) = rest.find('{') {
                    let j = rest[i..].find('}').unwrap() + i;
                    let name = rest[i + 1..j].trim_start_matches('+');
                    assert!(
                        o.params.iter().any(|p| p.name == name),
                        "{}: {}.{} path uses {{{name}}} without a param",
                        s.name,
                        r.name,
                        o.name
                    );
                    rest = &rest[j + 1..];
                }
            }
        }
    }
}

async fn run_op(s: &Spec, resource: &str, o: &OpSpec) {
    // answer with the shape the operation reads: a list at `out`, else a plain object
    let reply = match &o.out {
        Some(path) => path.split('.').rev().fold(json!([{"id": "1"}]), |acc, k| json!({ k: acc })),
        None if o.list => json!([{"id": "1"}]),
        None => json!({"id": "1", "ok": true}),
    };
    let (base, seen) = serve(vec![http_resp("200 OK", &reply.to_string())]).await;
    let mut nodes = NodeRegistry::new();
    let creds = CredentialTypes::new();
    register_one(&mut nodes, &creds, s, &base).unwrap();

    let mut params = Map::new();
    params.insert("resource".into(), json!(resource));
    params.insert("operation".into(), json!(o.name));
    for p in &o.params {
        params.insert(p.name.clone(), sample(p));
    }
    if o.list {
        params.insert("returnAll".into(), json!(false));
        params.insert("limit".into(), json!(2));
    }
    if !o.extra.is_empty() {
        let extra: Map<String, Value> = o.extra.iter().map(|p| (p.name.clone(), sample(p))).collect();
        params.insert("additionalFields".into(), Value::Object(extra));
    }
    let node: Node = serde_json::from_value(json!({
        "name": "N", "type": s.node_type, "typeVersion": s.versions.last().copied().unwrap_or(1.0),
        "position": [0, 0], "parameters": params,
        "credentials": {s.cred.name.clone(): {"id": "c1", "name": "c"}}
    }))
    .unwrap();
    let mut ctx = NodeContext::new(node, vec![vec![Item::from_json(json!({}))]], 0);
    ctx.hooks.credentials = Some(Arc::new(Creds(cred_data(s))));
    let label = format!("{}.{}.{}", s.name, resource, o.name);
    let out = nodes
        .latest(&s.node_type)
        .unwrap()
        .execute(&ctx)
        .await
        .unwrap_or_else(|e| panic!("{label}: {e}"));
    assert!(!out[0].is_empty(), "{label}: no items");

    let req = seen.lock().unwrap()[0].clone();
    let (head, body) = req.split_once("\r\n\r\n").unwrap();
    let line = head.lines().next().unwrap();
    assert!(
        line.starts_with(&format!("{} ", o.method.to_uppercase())),
        "{label}: {line}"
    );
    let target = line.split(' ').nth(1).unwrap();
    assert!(!target.contains('{') && !target.contains("%7B"), "{label}: unresolved placeholder in {target}");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = &pct_decode(query);
    // the path template with each {x} replaced by its sample
    let mut want = o.path.clone();
    for p in &o.params {
        want = want.replace(&format!("{{{}}}", p.name), &format!("v_{}", p.name));
        want = want.replace(&format!("{{+{}}}", p.name), &format!("v_{}", p.name));
    }
    assert!(path.ends_with(&want), "{label}: {path} vs {want}");

    let lower = head.to_lowercase();
    for k in s.cred.auth.header.keys() {
        assert!(lower.contains(&format!("{}:", k.to_lowercase())), "{label}: auth header {k} missing");
    }
    if s.cred.oauth.is_some() {
        assert!(lower.contains("authorization: bearer cv_accesstoken"), "{label}: bearer");
    }
    if let Some((u, _)) = &s.cred.auth.basic {
        let _ = u;
        assert!(lower.contains("authorization: basic"), "{label}: basic auth");
    }
    for k in s.cred.auth.query.keys() {
        assert!(query.contains(&format!("{k}=")), "{label}: auth query {k} in {query}");
    }
    for (k, v) in &s.headers {
        assert!(lower.contains(&format!("{}: {}", k.to_lowercase(), v.to_lowercase())), "{label}: header {k}");
    }

    let json_body: Option<Value> = serde_json::from_str(body).ok();
    for p in o.params.iter().chain(o.extra.iter()) {
        let Some(kind) = &p.send else { continue };
        let field = p.prop.clone().unwrap_or_else(|| p.name.clone());
        match kind.as_str() {
            "query" => assert!(query.contains(&format!("{field}=")), "{label}: query {field} in {query}"),
            "queryString" => assert!(query.contains(&format!("v_{}", p.name)), "{label}: raw query {query}"),
            "header" => assert!(lower.contains(&format!("{}:", field.to_lowercase())), "{label}: header {field}"),
            _ if s.body_format.as_deref() == Some("form") => {
                // `a.b` becomes `a[b]` in form encoding
                let mut it = field.split('.');
                let key = it.next().unwrap().to_owned() + &it.map(|k| format!("[{k}]")).collect::<String>();
                let form = pct_decode(&body.replace('+', " "));
                assert!(
                    form.split('&').any(|kv| kv.starts_with(&format!("{key}="))),
                    "{label}: form field {key} in {form}"
                );
            }
            _ => {
                let b = json_body.as_ref().unwrap_or_else(|| panic!("{label}: no JSON body"));
                if field == "$" {
                    // the parameter *is* the body: its fields sit at the root
                    assert!(b.get("a").is_some() || b.is_array(), "{label}: body should be the parsed parameter, got {b}");
                    continue;
                }
                let got = get_path(b, &field).unwrap_or_else(|| panic!("{label}: body field {field} in {b}"));
                if p.ty.as_deref() == Some("json") {
                    assert!(got.is_object() || got.is_array(), "{label}: {field} should be parsed JSON, got {got}");
                }
            }
        }
    }
}

#[tokio::test]
async fn every_operation_sends_what_its_spec_declares() {
    let mut n = 0;
    for s in specs() {
        for r in &s.resources {
            for o in &r.ops {
                run_op(&s, &r.name, o).await;
                n += 1;
            }
        }
    }
    assert!(n >= 1, "only {n} operations exercised");
}
