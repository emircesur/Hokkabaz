//! Talking to a user's own n8n instance: pull / push workflows, and run node types this engine
//! lacks there ([`N8nRemote`], a [`RemoteNodeRunner`]).
//!
//! n8n's public API has no "run this node" call, so a delegated node runs inside a throwaway
//! workflow `Webhook -> Code (items) -> <node> -> Respond to Webhook`, created, activated and
//! called over its production webhook, then deleted. Limits: one input port, first output
//! only, no binary data, and expressions that read other nodes cannot resolve remotely.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use hk_flow::{FlowError, Item, RemoteNodeRunner, workflow::Node};
use serde_json::{Map, Value, json};

const TIMEOUT: Duration = Duration::from_secs(120);
const MAX_PAGES: usize = 50;

#[derive(Clone)]
pub struct N8nClient {
    http: reqwest::Client,
    /// Ends with `/api/v1`.
    base: String,
    key: String,
}

impl fmt::Debug for N8nClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("N8nClient")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// Error text without the key or the full URL.
fn http_err(what: &str, e: &reqwest::Error) -> String {
    if e.is_timeout() {
        format!("{what}: timed out")
    } else if e.is_connect() {
        format!("{what}: cannot connect to the n8n instance")
    } else {
        format!("{what}: request failed")
    }
}

impl N8nClient {
    /// `base_url` may be the instance root or the API base; `/api/v1` is added when missing.
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, String> {
        let mut base = base_url.trim().trim_end_matches('/').to_owned();
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return Err("n8n URL must start with http:// or https://".into());
        }
        if !base.ends_with("/api/v1") {
            base.push_str("/api/v1");
        }
        if api_key.trim().is_empty() {
            return Err("n8n API key is empty".into());
        }
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            http,
            base,
            key: api_key.trim().to_owned(),
        })
    }

    fn root(&self) -> &str {
        self.base.trim_end_matches("/api/v1")
    }

    async fn call(
        &self,
        what: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        let mut r = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("X-N8N-API-KEY", &self.key);
        if let Some(b) = body {
            r = r
                .header("content-type", "application/json")
                .body(b.to_string());
        }
        let resp = r.send().await.map_err(|e| http_err(what, &e))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| http_err(what, &e))?;
        if !status.is_success() {
            let msg = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["message"].as_str().map(str::to_owned))
                .unwrap_or_default();
            return Err(match status.as_u16() {
                401 | 403 => format!("{what}: the n8n API key was rejected"),
                404 => format!("{what}: not found on the n8n instance"),
                code => format!("{what}: n8n answered {code} {msg}")
                    .trim()
                    .to_owned(),
            });
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|_| {
            format!("{what}: n8n returned something that is not JSON (is the URL the API base?)")
        })
    }

    pub async fn ping(&self) -> Result<(), String> {
        self.call("connect", reqwest::Method::GET, "/workflows?limit=1", None)
            .await
            .map(|_| ())
    }

    /// Every workflow (id, name, active, updatedAt, ... without need for node bodies).
    pub async fn list_workflows(&self) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let path = match &cursor {
                Some(c) => format!("/workflows?limit=100&cursor={c}"),
                None => "/workflows?limit=100".to_owned(),
            };
            let page = self
                .call("list workflows", reqwest::Method::GET, &path, None)
                .await?;
            if let Some(d) = page["data"].as_array() {
                out.extend(d.iter().cloned());
            }
            match page["nextCursor"].as_str().filter(|c| !c.is_empty()) {
                Some(c) => cursor = Some(c.to_owned()),
                None => break,
            }
        }
        Ok(out)
    }

    pub async fn get_workflow(&self, id: &str) -> Result<Value, String> {
        self.call(
            "get workflow",
            reqwest::Method::GET,
            &format!("/workflows/{}", urlenc(id)),
            None,
        )
        .await
    }

    pub async fn create_workflow(&self, wf: &Value) -> Result<Value, String> {
        self.call(
            "create workflow",
            reqwest::Method::POST,
            "/workflows",
            Some(&push_body(wf)),
        )
        .await
    }

    pub async fn update_workflow(&self, id: &str, wf: &Value) -> Result<Value, String> {
        self.call(
            "update workflow",
            reqwest::Method::PUT,
            &format!("/workflows/{}", urlenc(id)),
            Some(&push_body(wf)),
        )
        .await
    }

    pub async fn set_active(&self, id: &str, active: bool) -> Result<(), String> {
        let action = if active { "activate" } else { "deactivate" };
        self.call(
            action,
            reqwest::Method::POST,
            &format!("/workflows/{}/{action}", urlenc(id)),
            None,
        )
        .await
        .map(|_| ())
    }

    pub async fn delete_workflow(&self, id: &str) -> Result<(), String> {
        self.call(
            "delete workflow",
            reqwest::Method::DELETE,
            &format!("/workflows/{}", urlenc(id)),
            None,
        )
        .await
        .map(|_| ())
    }
}

fn urlenc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The subset of a workflow n8n's create/update accepts (extra properties are rejected).
pub fn push_body(wf: &Value) -> Value {
    let mut settings = wf["settings"].as_object().cloned().unwrap_or_default();
    // Only settings the public API knows; anything else makes n8n answer 400.
    const KNOWN: &[&str] = &[
        "saveExecutionProgress",
        "saveManualExecutions",
        "saveDataErrorExecution",
        "saveDataSuccessExecution",
        "executionTimeout",
        "errorWorkflow",
        "timezone",
        "executionOrder",
    ];
    settings.retain(|k, _| KNOWN.contains(&k.as_str()));
    json!({
        "name": wf["name"].as_str().filter(|n| !n.is_empty()).unwrap_or("Untitled"),
        "nodes": wf["nodes"].clone(),
        "connections": wf["connections"].clone(),
        "settings": settings,
    })
}

#[derive(Debug, Clone)]
pub struct LocalCred {
    pub id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmapped {
    pub node: String,
    pub kind: String,
    pub name: String,
}

/// Re-point a pulled workflow's credential references at local credentials: same type and the
/// same name. Anything without a match keeps its remote reference and is reported, so the user
/// can create the credential and pick it in the node panel.
pub fn map_credentials(wf: &mut Value, local: &[LocalCred]) -> Vec<Unmapped> {
    let mut missing = Vec::new();
    let Some(nodes) = wf["nodes"].as_array_mut() else {
        return missing;
    };
    for n in nodes {
        let node_name = n["name"].as_str().unwrap_or_default().to_owned();
        let Some(creds) = n.get_mut("credentials").and_then(Value::as_object_mut) else {
            continue;
        };
        for (kind, r) in creds.iter_mut() {
            let name = r["name"].as_str().unwrap_or_default().to_owned();
            match local.iter().find(|c| &c.kind == kind && c.name == name) {
                Some(c) => *r = json!({"id": c.id, "name": c.name}),
                None => missing.push(Unmapped {
                    node: node_name.clone(),
                    kind: kind.clone(),
                    name,
                }),
            }
        }
    }
    missing
}

// ---------------------------------------------------------------- remote nodes

#[derive(Debug, Clone)]
pub struct N8nRemote {
    client: N8nClient,
}

impl N8nRemote {
    pub fn new(client: N8nClient) -> Self {
        Self { client }
    }
}

fn token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("hk-{ns:x}-{:x}", N.fetch_add(1, Ordering::Relaxed))
}

fn shim_workflow(node: &Node, path: &str) -> Result<Value, String> {
    let mut target = serde_json::to_value(node).map_err(|e| e.to_string())?;
    let t = target.as_object_mut().ok_or("node is not an object")?;
    t.insert("name".into(), json!("Target"));
    t.insert("id".into(), json!("hk-target"));
    t.insert("position".into(), json!([460, 0]));
    t.remove("disabled");
    t.remove("webhookId");
    let code = "const body = $input.first().json.body || {};\nreturn (body.items || []).map(j => ({ json: j }));";
    Ok(json!({
        "name": format!("hokkabaz-delegate-{path}"),
        "nodes": [
            {"id": "hk-hook", "name": "Webhook", "type": "n8n-nodes-base.webhook", "typeVersion": 2, "position": [0, 0],
             "webhookId": path,
             "parameters": {"httpMethod": "POST", "path": path, "responseMode": "responseNode", "options": {}}},
            {"id": "hk-code", "name": "Items", "type": "n8n-nodes-base.code", "typeVersion": 2, "position": [230, 0],
             "parameters": {"jsCode": code}},
            target,
            {"id": "hk-resp", "name": "Respond", "type": "n8n-nodes-base.respondToWebhook", "typeVersion": 1.1, "position": [690, 0],
             "parameters": {"respondWith": "allIncomingItems", "options": {}}}
        ],
        "connections": {
            "Webhook": {"main": [[{"node": "Items", "type": "main", "index": 0}]]},
            "Items": {"main": [[{"node": "Target", "type": "main", "index": 0}]]},
            "Target": {"main": [[{"node": "Respond", "type": "main", "index": 0}]]}
        },
        "settings": {"executionOrder": "v1"}
    }))
}

impl N8nRemote {
    async fn call_webhook(&self, path: &str, items: &[Item]) -> Result<Value, String> {
        let url = format!("{}/webhook/{path}", self.client.root());
        let body = json!({"items": items.iter().map(|i| Value::Object(i.json.clone())).collect::<Vec<_>>()});
        // The production webhook registers a moment after activation.
        let mut last = String::new();
        for attempt in 0..6u64 {
            let resp = self
                .client
                .http
                .post(&url)
                .header("content-type", "application/json")
                .body(body.to_string())
                .send()
                .await
                .map_err(|e| http_err("run on n8n", &e))?;
            let status = resp.status();
            let text = resp.text().await.map_err(|e| http_err("run on n8n", &e))?;
            if status.is_success() {
                return Ok(serde_json::from_str(&text).unwrap_or(Value::Null));
            }
            let msg = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["message"].as_str().map(str::to_owned))
                .unwrap_or_default();
            last = format!("n8n answered {} {msg}", status.as_u16());
            if status.as_u16() != 404 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250 * (attempt + 1))).await;
        }
        Err(last.trim().to_owned())
    }
}

#[async_trait]
impl RemoteNodeRunner for N8nRemote {
    async fn run_node(&self, node: &Node, input: &[Item]) -> Result<Vec<Vec<Item>>, FlowError> {
        let fail = |m: String| FlowError::Node {
            node: node.name.clone(),
            message: format!(
                "{m} (node type {} is not built in; delegated to n8n)",
                node.node_type
            ),
        };
        let path = token();
        let wf = shim_workflow(node, &path).map_err(&fail)?;
        let created = self.client.create_workflow(&wf).await.map_err(&fail)?;
        let id = created["id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| fail("n8n did not return a workflow id".into()))?;
        let result = async {
            self.client.set_active(&id, true).await?;
            self.call_webhook(&path, input).await
        }
        .await;
        // Clean up whatever happened; a leftover workflow is named `hokkabaz-delegate-*`.
        let _ = self.client.set_active(&id, false).await;
        let _ = self.client.delete_workflow(&id).await;
        let value = result.map_err(&fail)?;
        let items: Vec<Item> = match value {
            Value::Array(a) => a
                .into_iter()
                .map(|v| match v {
                    Value::Object(m) => Item::new(m),
                    other => Item::new(Map::from_iter([("data".to_owned(), other)])),
                })
                .collect(),
            Value::Object(m) => vec![Item::new(m)],
            _ => Vec::new(),
        };
        Ok(vec![items])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_is_normalised_and_key_required() {
        assert_eq!(
            N8nClient::new("https://n.example.com/", "k").unwrap().base,
            "https://n.example.com/api/v1"
        );
        assert_eq!(
            N8nClient::new("https://n.example.com/api/v1", "k")
                .unwrap()
                .root(),
            "https://n.example.com"
        );
        assert!(N8nClient::new("n.example.com", "k").is_err());
        assert!(N8nClient::new("https://n.example.com", " ").is_err());
    }

    #[test]
    fn push_body_keeps_only_accepted_fields() {
        let wf = json!({"id": "1", "active": true, "pinData": {}, "name": "", "nodes": [], "connections": {},
                        "settings": {"executionOrder": "v1", "binaryMode": "x"}});
        let b = push_body(&wf);
        assert_eq!(b["name"], "Untitled");
        assert_eq!(b["settings"], json!({"executionOrder": "v1"}));
        assert!(b.get("pinData").is_none() && b.get("active").is_none() && b.get("id").is_none());
    }

    #[test]
    fn credentials_map_by_type_and_name() {
        let mut wf = json!({"nodes": [
            {"name": "A", "credentials": {"slackApi": {"id": "r1", "name": "Team Slack"}, "notionApi": {"id": "r2", "name": "Wiki"}}},
            {"name": "B"}]});
        let local = [LocalCred {
            id: "c9".into(),
            name: "Team Slack".into(),
            kind: "slackApi".into(),
        }];
        let missing = map_credentials(&mut wf, &local);
        assert_eq!(
            wf["nodes"][0]["credentials"]["slackApi"],
            json!({"id": "c9", "name": "Team Slack"})
        );
        assert_eq!(wf["nodes"][0]["credentials"]["notionApi"]["id"], "r2");
        assert_eq!(
            missing,
            vec![Unmapped {
                node: "A".into(),
                kind: "notionApi".into(),
                name: "Wiki".into()
            }]
        );
    }

    #[test]
    fn shim_wraps_the_node_between_webhook_and_respond() {
        let node: Node = serde_json::from_value(json!({"name": "Orig", "type": "n8n-nodes-base.slack", "typeVersion": 2.2, "position": [1, 2],
            "parameters": {"text": "x"}, "credentials": {"slackApi": {"id": "r1", "name": "S"}}})).unwrap();
        let wf = shim_workflow(&node, "p1").unwrap();
        assert_eq!(wf["nodes"][2]["name"], "Target");
        assert_eq!(wf["nodes"][2]["type"], "n8n-nodes-base.slack");
        assert_eq!(wf["nodes"][2]["credentials"]["slackApi"]["id"], "r1");
        assert_eq!(wf["nodes"][0]["parameters"]["path"], "p1");
        assert_eq!(wf["connections"]["Target"]["main"][0][0]["node"], "Respond");
    }
}
