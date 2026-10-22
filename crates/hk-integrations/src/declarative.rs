//! Data-driven REST integrations.
//!
//! Most SaaS nodes are a table: resource, operation, HTTP method, path, which parameter goes
//! where. Each lives in `specs/<name>.json` (embedded at build time) and is expanded here into the
//! n8n node description plus a [`RestSpec`], then runs on [`RestNode`] like the hand-written
//! `ntfy`/`gitea`. Parameter and operation names follow n8n so imported workflows run unchanged.
//!
//! Spec shape (see `specs/notion.json`): `cred` = credential fields + how they authenticate
//! (`header`/`query`/`basic` templates over `$credentials.*`, or `oauth` for refresh-token
//! credentials); `resources[].ops[]` = one request each, `path` with `{param}` placeholders
//! (percent-encoded), `params` shown for that operation, `extra` = optional "Additional Fields",
//! `list` = adds Return All / Limit, `page` = pagination, `out` = response property holding the
//! items.

use std::collections::BTreeMap;
use std::sync::Arc;

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, NodeRegistry};

use crate::credentials::{
    Authenticate, CredentialTest, CredentialType, CredentialTypes, GenericAuth,
};
use crate::describe::Prop;
use crate::pathparam;
use crate::rest::{RestBody, RestNode, RestRequest, RestSpec};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    /// Short id (file name), used in messages.
    pub name: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub display: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    /// Supported `typeVersion`s, ascending; the last is the default.
    #[serde(default)]
    pub versions: Vec<f64>,
    /// Base URL template (`$credentials.*` allowed).
    pub base: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub cred: CredSpec,
    pub resources: Vec<ResourceSpec>,
    #[serde(default)]
    pub error_paths: Vec<String>,
    /// `json` (default) | `form` (`application/x-www-form-urlencoded`, nested keys as `a[b]`).
    #[serde(default)]
    pub body_format: Option<String>,
    /// Named code hook for requests a table cannot express (see [`hook`]).
    #[serde(default)]
    pub hook: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredSpec {
    pub name: String,
    pub display: String,
    #[serde(default)]
    pub docs: Option<String>,
    #[serde(default)]
    pub fields: Vec<FieldSpec>,
    #[serde(default)]
    pub auth: AuthSpec,
    /// Cheap authenticated GET (path under `base`) that proves the credential.
    #[serde(default)]
    pub test: Option<String>,
    /// Refresh-token credential: adds clientId / clientSecret / refreshToken / tokenUrl fields and
    /// authenticates with the minted bearer token.
    #[serde(default)]
    pub oauth: Option<OauthSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OauthSpec {
    pub token_url: String,
    #[serde(default)]
    pub scope: Option<String>,
    /// Extra non-secret field, e.g. Microsoft `tenant`.
    #[serde(default)]
    pub tenant: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldSpec {
    pub name: String,
    pub display: String,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub desc: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSpec {
    #[serde(default)]
    pub header: IndexMap<String, String>,
    #[serde(default)]
    pub query: IndexMap<String, String>,
    #[serde(default)]
    pub basic: Option<(String, String)>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSpec {
    pub name: String,
    pub display: String,
    pub ops: Vec<OpSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpSpec {
    pub name: String,
    pub display: String,
    #[serde(default)]
    pub action: Option<String>,
    pub method: String,
    /// `{param}` placeholders are filled (percent-encoded) from node parameters.
    pub path: String,
    #[serde(default)]
    pub params: Vec<ParamSpec>,
    /// "Additional Fields" collection.
    #[serde(default)]
    pub extra: Vec<ParamSpec>,
    #[serde(default)]
    pub qs: Map<String, Value>,
    #[serde(default)]
    pub body: Map<String, Value>,
    #[serde(default)]
    pub headers: Map<String, Value>,
    /// Response property holding the result list / object.
    #[serde(default)]
    pub out: Option<String>,
    /// Adds Return All / Limit.
    #[serde(default)]
    pub list: bool,
    /// `out` is a table (array of arrays); the first row names the columns.
    #[serde(default)]
    pub rows: bool,
    #[serde(default)]
    pub page: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamSpec {
    pub name: String,
    pub display: String,
    /// `string` (default) | `number` | `boolean` | `json` | `options` | `dateTime`
    #[serde(default, rename = "type")]
    pub ty: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub desc: Option<String>,
    /// `[label, value]` pairs for `options`.
    #[serde(default)]
    pub options: Vec<(String, Value)>,
    /// `body` | `query` | `header` | `queryString` (a `a=b&c=d` string appended to the query);
    /// absent = only used in the path / expressions.
    #[serde(default)]
    pub send: Option<String>,
    /// Request field name when it differs from `name` (dotted for nested body objects).
    #[serde(default)]
    pub prop: Option<String>,
}

fn param_json(p: &ParamSpec, show: &Value) -> Value {
    let ty = p.ty.as_deref().unwrap_or("string");
    let default = p.default.clone().unwrap_or_else(|| match ty {
        "number" => json!(0),
        "boolean" => json!(false),
        "json" => json!("{}"),
        _ => json!(""),
    });
    let mut v = json!({"displayName": p.display, "name": p.name, "type": ty, "default": default});
    if p.required {
        v["required"] = json!(true);
    }
    if let Some(d) = &p.desc {
        v["description"] = json!(d);
    }
    if ty == "options" {
        v["options"] = p
            .options
            .iter()
            .map(|(n, val)| json!({"name": n, "value": val}))
            .collect();
    }
    if !show.is_null() {
        v["displayOptions"] = json!({"show": show});
    }
    if let Some(kind) = p.send.as_deref().filter(|k| *k != "queryString") {
        let mut send =
            json!({"type": kind, "property": p.prop.clone().unwrap_or_else(|| p.name.clone())});
        if ty == "json" {
            send["json"] = json!(true);
        }
        v["routing"] = json!({"send": send});
    }
    v
}

impl Spec {
    pub fn parse(raw: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(raw)
    }

    fn credential(&self) -> CredentialType {
        let c = &self.cred;
        let mut t = CredentialType::new(&c.name, &c.display);
        if let Some(d) = &c.docs {
            t = t.documentation(d);
        }
        for f in &c.fields {
            let mut p = Prop::string(&f.name, &f.display);
            if f.secret {
                p = p.password();
            }
            if f.required {
                p = p.required();
            }
            if let Some(d) = &f.default {
                p = p.default(d.clone());
            }
            if let Some(d) = &f.desc {
                p = p.description(d);
            }
            t = t.property(p);
        }
        let mut auth = GenericAuth {
            headers: c.auth.header.clone(),
            qs: c.auth.query.clone(),
            basic: c.auth.basic.clone(),
            ..GenericAuth::default()
        };
        if let Some(o) = &c.oauth {
            if o.tenant {
                t = t.property(
                    Prop::string("tenant", "Tenant")
                        .default("common")
                        .description("Directory (tenant) id, or `common`"),
                );
            }
            t = t
                .property(Prop::string("clientId", "Client ID").required())
                .property(Prop::string("clientSecret", "Client Secret").password())
                .property(
                    Prop::string("refreshToken", "Refresh Token")
                        .password()
                        .required()
                        .description("Long-lived token from the provider's consent screen; access tokens are minted from it"),
                )
                .property(Prop::string("tokenUrl", "Token URL").default(o.token_url.clone()));
            if let Some(s) = &o.scope {
                t = t.property(Prop::string("scope", "Scope").default(s.clone()));
            }
            auth.headers.insert(
                "Authorization".into(),
                "=Bearer {{$credentials.accessToken}}".into(),
            );
        }
        if !auth.headers.is_empty() || !auth.qs.is_empty() || auth.basic.is_some() {
            t = t.authenticate(Authenticate::Generic(Box::new(auth)));
        }
        if let Some(path) = &c.test
            && c.oauth.is_none()
        {
            t = t.test(CredentialTest::get(&self.base, path));
        }
        t
    }

    fn description(&self) -> NodeTypeDescription {
        let versions = if self.versions.is_empty() {
            vec![1.0]
        } else {
            self.versions.clone()
        };
        let default_version = versions.last().copied().unwrap_or(1.0);
        let mut props: Vec<Value> = Vec::new();
        let first = &self.resources[0];
        props.push(json!({
            "displayName": "Resource", "name": "resource", "type": "options", "default": first.name,
            "options": self.resources.iter().map(|r| json!({"name": r.display, "value": r.name})).collect::<Vec<_>>()
        }));
        for r in &self.resources {
            let ops: Vec<Value> = r.ops.iter().map(|o| self.op_choice(o)).collect();
            props.push(json!({
                "displayName": "Operation", "name": "operation", "type": "options", "default": r.ops[0].name,
                "displayOptions": {"show": {"resource": [r.name]}},
                "options": ops
            }));
            for o in &r.ops {
                let show = json!({"resource": [r.name], "operation": [o.name]});
                for p in &o.params {
                    props.push(param_json(p, &show));
                }
                if o.list {
                    props.push(
                        json!({"displayName": "Return All", "name": "returnAll", "type": "boolean",
                        "default": false, "displayOptions": {"show": show}}),
                    );
                    props.push(json!({"displayName": "Limit", "name": "limit", "type": "number", "default": 50,
                        "typeOptions": {"minValue": 1},
                        "displayOptions": {"show": {"resource": [r.name], "operation": [o.name], "returnAll": [false]}}}));
                }
                if !o.extra.is_empty() {
                    props.push(json!({
                        "displayName": "Additional Fields", "name": "additionalFields", "type": "collection",
                        "default": {}, "placeholder": "Add Field", "displayOptions": {"show": show},
                        "options": o.extra.iter().map(|p| param_json(p, &Value::Null)).collect::<Vec<_>>()
                    }));
                }
            }
        }
        let mut d = json!({
            "displayName": self.display,
            "name": self.node_type,
            "version": versions,
            "defaultVersion": default_version,
            "description": self.description,
            "subtitle": "={{ $parameter[\"operation\"] + \": \" + $parameter[\"resource\"] }}",
            "group": ["output"],
            "defaults": {"name": self.display, "color": self.color.clone().unwrap_or_else(|| "#555555".into())},
            "inputs": ["main"],
            "outputs": ["main"],
            "credentials": [{"name": self.cred.name, "required": true}],
            "properties": props,
        });
        if let Some(i) = &self.icon {
            d["icon"] = json!(i);
        }
        serde_json::from_value(d)
            .unwrap_or_else(|e| panic!("spec {}: bad description: {e}", self.name))
    }

    fn op_choice(&self, o: &OpSpec) -> Value {
        let mut request = json!({"method": o.method, "url": o.path});
        if !o.qs.is_empty() {
            request["qs"] = Value::Object(o.qs.clone());
        }
        if !o.body.is_empty() {
            request["body"] = Value::Object(o.body.clone());
        }
        if !o.headers.is_empty() {
            request["headers"] = Value::Object(o.headers.clone());
        }
        let mut routing = json!({"request": request});
        if let Some(out) = &o.out {
            routing["output"] = json!({"property": out, "rows": o.rows});
        }
        if let Some(p) = &o.page {
            routing["pagination"] = p.clone();
        }
        let mut c = json!({"name": o.display, "value": o.name, "routing": routing});
        if let Some(a) = &o.action {
            c["action"] = json!(a);
        }
        c
    }

    /// Node over `base` (a test points it at a local server).
    pub fn node(&self, creds: &CredentialTypes, base: Option<&str>) -> RestNode {
        let desc = self.description();
        let defaults = pathparam::defaults_of(&desc);
        let form = self.body_format.as_deref() == Some("form");
        let hook_fn = self.hook.as_deref().and_then(hook);
        let raw_query: Vec<String> = self
            .resources
            .iter()
            .flat_map(|r| &r.ops)
            .flat_map(|o| o.params.iter().chain(&o.extra))
            .filter(|p| p.send.as_deref() == Some("queryString"))
            .map(|p| p.name.clone())
            .collect();
        let mut spec = RestSpec::new(base.unwrap_or(&self.base))
            .credential(&self.cred.name, true)
            .pre_send(move |req, ctx, i| {
                pathparam::expand(req, ctx, &defaults, i)?;
                for name in &raw_query {
                    if let Some(v) = ctx.eval_param_at(name, i)? {
                        for (k, val) in crate::template::display(&v)
                            .split('&')
                            .filter(|s| !s.is_empty())
                            .map(|kv| kv.split_once('=').unwrap_or((kv, "")))
                        {
                            req.query.push((k.to_owned(), val.to_owned()));
                        }
                    }
                }
                if let Some(h) = hook_fn {
                    h(req, ctx, i)?;
                }
                if form && let RestBody::Json(Value::Object(o)) = &req.body {
                    let mut pairs = Vec::new();
                    flatten_form("", &Value::Object(o.clone()), &mut pairs);
                    req.body = RestBody::Form(pairs);
                }
                Ok(())
            });
        for (k, v) in &self.headers {
            spec = spec.header(k, v);
        }
        for p in &self.error_paths {
            spec = spec.error_path(p);
        }
        RestNode::new(desc, spec, creds)
    }
}

/// Stripe/Twilio style form encoding: `a[b]=1`, `a[0]=x`.
fn flatten_form(prefix: &str, v: &Value, out: &mut Vec<(String, String)>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}[{k}]")
                };
                flatten_form(&key, x, out);
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                flatten_form(&format!("{prefix}[{i}]"), x, out);
            }
        }
        Value::Null => {}
        other => out.push((prefix.to_owned(), crate::template::display(other))),
    }
}

type Hook = fn(&mut RestRequest, &hk_flow::NodeContext, usize) -> Result<(), FlowError>;

/// Code hooks named by `spec.hook`: they run after path expansion, with the planned request.
fn hook(name: &str) -> Option<Hook> {
    match name {
        "gmail" => Some(gmail),
        _ => None,
    }
}

/// Gmail `messages/send` takes the whole RFC 2822 message base64url-encoded in `raw`.
fn gmail(req: &mut RestRequest, ctx: &hk_flow::NodeContext, i: usize) -> Result<(), FlowError> {
    use base64::Engine as _;
    let draft = req.url.ends_with("/drafts");
    if req.method != "POST" || (!draft && !req.url.ends_with("/messages/send")) {
        return Ok(());
    }
    let get = |p: &str| -> Result<String, FlowError> {
        Ok(ctx
            .eval_param_at(p, i)?
            .map(|v| crate::template::display(&v))
            .unwrap_or_default())
    };
    // header values must not break out of their line
    let clean = |s: String| s.replace(['\r', '\n'], " ");
    let to = clean(get("sendTo")?);
    if to.is_empty() {
        return Err(FlowError::Node {
            node: ctx.node.name.clone(),
            message: "Parameter `To` is required".into(),
        });
    }
    let html = get("emailType")? == "html";
    let mut msg = format!("To: {to}\r\n");
    for (opt, header) in [
        ("options.ccList", "Cc"),
        ("options.bccList", "Bcc"),
        ("options.replyTo", "Reply-To"),
    ] {
        let v = clean(get(opt)?);
        if !v.is_empty() {
            msg.push_str(&format!("{header}: {v}\r\n"));
        }
    }
    let subject = clean(get("subject")?);
    let subject_b64 = base64::engine::general_purpose::STANDARD.encode(subject.as_bytes());
    msg.push_str(&format!(
        "Subject: =?UTF-8?B?{subject_b64}?=\r\nMIME-Version: 1.0\r\nContent-Type: text/{}; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n",
        if html { "html" } else { "plain" }
    ));
    let body = base64::engine::general_purpose::STANDARD.encode(get("message")?.as_bytes());
    for chunk in body.as_bytes().chunks(76) {
        msg.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        msg.push_str("\r\n");
    }
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(msg.as_bytes());
    req.body = RestBody::Json(if draft {
        json!({ "message": { "raw": raw } })
    } else {
        json!({ "raw": raw })
    });
    Ok(())
}

/// Every embedded spec.
pub fn specs() -> Vec<Spec> {
    crate::specs::ALL
        .iter()
        .map(|(file, raw)| Spec::parse(raw).unwrap_or_else(|e| panic!("specs/{file}: {e}")))
        .collect()
}

/// Register every spec'd node and its credential type.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    for s in specs() {
        creds.register_if_absent(s.credential());
        nodes.register(Arc::new(s.node(creds, None)))?;
    }
    Ok(())
}

/// Register one spec with a different base URL and return its credential type name (tests).
pub fn register_one(
    nodes: &mut NodeRegistry,
    creds: &CredentialTypes,
    spec: &Spec,
    base: &str,
) -> Result<(), FlowError> {
    creds.register_if_absent(spec.credential());
    nodes.register(Arc::new(spec.node(creds, Some(base))))
}
