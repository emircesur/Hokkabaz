//! Agent tools over the flow engine: `flow_author` (draft workflows) and `flow_run` (test run).
//!
//! Lives here, not in hk-tools: hk-tools stays free of hk-flow/hk-store (android bloat, no
//! dep cycle), while hk-server's `server` tier already links both. The runtime that owns the
//! executor (the daemon) plugs in through [`FlowToolHost`]; [`flow_tools`] hands back the
//! tools for `AgentRuntime::add_tools`.
// port: none (Hokkabaz-native; see DESIGN §3.9)

use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use hk_flow::triggers::{TriggerKind, find_trigger};
use hk_flow::{
    ExecutionResult, IoDecl, Item, Node, NodeProperty, NodeRegistry, RunStatus, Workflow,
};
use hk_store::Store;
use hk_tools::{Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};
use serde_json::{Map, Value, json};

use crate::n8n::{new_id, new_version_id, prepare_workflow, workflow_json};

const AUTHOR: &str = "flow_author";
const RUN: &str = "flow_run";
const NS_BASE: &str = "n8n-nodes-base.";
const PREFIXES: [&str; 3] = [NS_BASE, "@n8n/n8n-nodes-langchain.", "hokkabaz."];
const MAX_ITEMS_IN: usize = 100;
const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_TIMEOUT_SECS: u64 = 300;
const PREVIEW_ITEMS: usize = 5;
const PREVIEW_ITEM_CHARS: usize = 1500;
const PREVIEW_TOTAL_CHARS: usize = 6000;
const SCAN_LIMIT: u32 = 1000;

/// Node types (all `n8n-nodes-base.*`) that only reshape items in memory. A workflow made of
/// these runs without asking; anything else (HTTP, sub-workflows, agents, integrations) may
/// reach outside the process.
const PURE: [&str; 24] = [
    "manualTrigger",
    "scheduleTrigger",
    "webhook",
    "formTrigger",
    "executeWorkflowTrigger",
    "errorTrigger",
    "set",
    "if",
    "switch",
    "filter",
    "merge",
    "splitInBatches",
    "noOp",
    "code",
    "dateTime",
    "aggregate",
    "splitOut",
    "sort",
    "limit",
    "removeDuplicates",
    "wait",
    "respondToWebhook",
    "stickyNote",
    "stopAndError",
];

/// What the tools need from the runtime that owns the executor.
#[async_trait]
pub trait FlowToolHost: Send + Sync + fmt::Debug {
    /// Node implementations; drives validation and type shorthand.
    fn nodes(&self) -> &NodeRegistry;
    /// Run `req` in test mode: manual execution mode, pinned data honoured, no activation, no
    /// error workflow. The host records an execution row when `req.workflow_id` is set.
    async fn run_test(&self, req: TestRun) -> Result<ExecutionResult, String>;
    /// Blank credential-bearing values before run data is shown to the model.
    fn scrub(&self, _data: &mut Value) {}
}

#[derive(Debug, Clone)]
pub struct TestRun {
    pub workflow: Workflow,
    /// Stored workflow id; `None` = inline workflow, nothing recorded.
    pub workflow_id: Option<String>,
    /// Trigger node that emits `items` (not executed itself).
    pub start_node: String,
    pub items: Vec<Item>,
}

struct Shared {
    host: Arc<dyn FlowToolHost>,
    store: Store,
    /// Stored workflows last seen free of side effects. `Tool::risk` is sync and cannot read the
    /// store, so it consults this; `flow_run` re-checks the row before running (see there).
    pure: Mutex<HashSet<String>>,
}

impl Shared {
    fn pure(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.pure.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `flow_author` + `flow_run`, ready for `AgentRuntime::add_tools`.
pub fn flow_tools(host: Arc<dyn FlowToolHost>, store: Store) -> Vec<Arc<dyn Tool>> {
    let sh = Arc::new(Shared {
        host,
        store,
        pure: Mutex::default(),
    });
    vec![Arc::new(FlowAuthor(sh.clone())), Arc::new(FlowRun(sh))]
}

// ------------------------------------------------------------------ normalize

fn ver_json(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 1e9 {
        #[allow(clippy::cast_possible_truncation)]
        return json!(v as i64);
    }
    json!(v)
}

fn resolve_type(nodes: &NodeRegistry, t: &str) -> String {
    if t.contains('.') {
        return t.to_owned();
    }
    PREFIXES
        .iter()
        .map(|p| format!("{p}{t}"))
        .find(|full| nodes.latest(full).is_some())
        .unwrap_or_else(|| format!("{NS_BASE}{t}"))
}

fn latest_version(nodes: &NodeRegistry, ty: &str) -> Option<Value> {
    let v = nodes.latest(ty)?.description().latest_version()?;
    Some(ver_json(v.get()))
}

fn default_pos(i: usize) -> Value {
    json!([250 + 220 * i64::try_from(i).unwrap_or(0), 300])
}

/// n8n JSON (has `connections`) gets missing node boilerplate filled in; anything else is read
/// as the compact spec and expanded. Returns n8n workflow JSON plus problems found while
/// expanding (reported together with validation errors).
fn normalize(input: &Value, nodes: &NodeRegistry) -> (Value, Vec<String>) {
    let Some(obj) = input.as_object() else {
        return (Value::Null, vec!["`workflow` must be a JSON object".into()]);
    };
    let mut errs = Vec::new();
    let mut out = obj.clone();
    let compact = !obj.contains_key("connections");
    let node_list = match obj.get("nodes") {
        Some(Value::Array(a)) => a.clone(),
        _ => {
            errs.push("`nodes` must be an array of node objects".into());
            return (Value::Object(out), errs);
        }
    };

    let mut fixed = Vec::with_capacity(node_list.len());
    for (i, n) in node_list.into_iter().enumerate() {
        let Value::Object(mut m) = n else {
            errs.push(format!("nodes[{i}] must be an object"));
            continue;
        };
        match m
            .get("type")
            .and_then(Value::as_str)
            .map(|t| resolve_type(nodes, t))
        {
            Some(t) => {
                if !m.contains_key("typeVersion") {
                    // Unknown types still need a version to parse; validation reports the type.
                    m.insert(
                        "typeVersion".into(),
                        latest_version(nodes, &t).unwrap_or_else(|| json!(1)),
                    );
                }
                m.insert("type".into(), json!(t));
            }
            None => errs.push(format!(
                "nodes[{i}] needs a `type` (e.g. `set`, `n8n-nodes-base.httpRequest`)"
            )),
        }
        if !m.contains_key("name")
            && let Some(Value::String(t)) = m.get("type")
        {
            // Last path segment is a readable default; duplicates are caught by validation.
            m.insert("name".into(), json!(t.rsplit('.').next().unwrap_or(t)));
        }
        m.entry("position").or_insert_with(|| default_pos(i));
        m.entry("parameters").or_insert_with(|| json!({}));
        fixed.push(Value::Object(m));
    }

    if compact {
        let conns = expand_links(obj.get("links"), &fixed, &mut errs);
        out.remove("links");
        out.insert("connections".into(), conns);
    }
    out.insert("nodes".into(), Value::Array(fixed));
    (Value::Object(out), errs)
}

fn node_name(n: &Value) -> Option<&str> {
    n.get("name").and_then(Value::as_str)
}

/// `links`: `["A","B"]`, `["A","B",out]`, `["A","B",out,in]` or `{from,to,output?,input?}`.
/// Omitted = chain the nodes in the order listed (notes excluded).
fn expand_links(links: Option<&Value>, nodes: &[Value], errs: &mut Vec<String>) -> Value {
    let names: Vec<&str> = nodes.iter().filter_map(node_name).collect();
    let mut edges: Vec<(String, String, usize, usize)> = Vec::new();
    match links {
        None | Some(Value::Null) => {
            let chain: Vec<&str> = nodes
                .iter()
                .filter(|n| {
                    n.get("type").and_then(Value::as_str) != Some("n8n-nodes-base.stickyNote")
                })
                .filter_map(node_name)
                .collect();
            for w in chain.windows(2) {
                edges.push((w[0].into(), w[1].into(), 0, 0));
            }
        }
        Some(Value::Array(ls)) => {
            for (i, l) in ls.iter().enumerate() {
                let parsed = match l {
                    Value::Array(a) if a.len() >= 2 && a.len() <= 4 => {
                        let idx = |k: usize| {
                            a.get(k)
                                .and_then(Value::as_u64)
                                .and_then(|n| usize::try_from(n).ok())
                        };
                        a[0].as_str()
                            .zip(a[1].as_str())
                            .map(|(f, t)| (f, t, idx(2).unwrap_or(0), idx(3).unwrap_or(0)))
                    }
                    Value::Object(o) => {
                        let idx = |k: &str| {
                            o.get(k)
                                .and_then(Value::as_u64)
                                .and_then(|n| usize::try_from(n).ok())
                        };
                        o.get("from")
                            .and_then(Value::as_str)
                            .zip(o.get("to").and_then(Value::as_str))
                            .map(|(f, t)| {
                                (f, t, idx("output").unwrap_or(0), idx("input").unwrap_or(0))
                            })
                    }
                    _ => None,
                };
                match parsed {
                    Some((f, t, o, n)) => edges.push((f.into(), t.into(), o, n)),
                    None => errs.push(format!(
                        "links[{i}] must be [from, to, output?, input?] or {{from, to, output?, input?}}"
                    )),
                }
            }
        }
        Some(_) => errs.push("`links` must be an array".into()),
    }

    let mut by_src: Map<String, Value> = Map::new();
    for (f, t, o, n) in edges {
        for (what, name) in [("from", &f), ("to", &t)] {
            if !names.contains(&name.as_str()) {
                errs.push(format!(
                    "link {what} unknown node `{name}`; nodes: {}",
                    names.join(", ")
                ));
            }
        }
        let slot = by_src.entry(f).or_insert_with(|| json!({ "main": [] }));
        let Some(Value::Array(outs)) = slot.get_mut("main") else {
            continue;
        };
        while outs.len() <= o {
            outs.push(json!([]));
        }
        if let Some(Value::Array(targets)) = outs.get_mut(o) {
            targets.push(json!({ "node": t, "type": "main", "index": n }));
        }
    }
    Value::Object(by_src)
}

// ------------------------------------------------------------------ validation

#[derive(Debug, Default)]
struct Report {
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl Report {
    fn to_json(&self) -> Value {
        json!({ "ok": self.errors.is_empty(), "errors": self.errors, "warnings": self.warnings })
    }
}

fn lev(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push(
                (prev[j] + usize::from(ca != *cb))
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1),
            );
        }
        prev = cur;
    }
    prev[b.len()]
}

fn suggest(nodes: &NodeRegistry, ty: &str) -> Vec<String> {
    let short = ty.rsplit('.').next().unwrap_or(ty).to_lowercase();
    let mut scored: Vec<(usize, String)> = nodes
        .types()
        .map(|t| {
            let s = t.rsplit('.').next().unwrap_or(t).to_lowercase();
            let d = if s.contains(&short) || short.contains(&s) {
                0
            } else {
                lev(&s, &short)
            };
            (d, t.to_owned())
        })
        .filter(|(d, _)| *d <= 2)
        .collect();
    scored.sort();
    scored.into_iter().take(5).map(|(_, t)| t).collect()
}

#[derive(PartialEq)]
enum Shown {
    Yes,
    No,
    Unknown,
}

/// Evaluates `displayOptions` against the node parameters. Conditions the engine cannot decide
/// (`_cnd`, root paths, `@version`) come out `Unknown` so no false "missing" error is raised.
fn shown(p: &NodeProperty, params: &Map<String, Value>, desc_props: &[NodeProperty]) -> Shown {
    let Some(d) = &p.display_options else {
        return Shown::Yes;
    };
    let current = |k: &str| -> Option<Value> {
        params.get(k).cloned().or_else(|| {
            desc_props
                .iter()
                .find(|q| q.name == k)
                .and_then(|q| q.default.clone())
        })
    };
    let mut verdict = Shown::Yes;
    for (map, show) in [(&d.show, true), (&d.hide, false)] {
        let Some(map) = map else { continue };
        let mut all = true;
        for (k, want) in map {
            if k.starts_with('/') || k.starts_with('@') {
                return Shown::Unknown;
            }
            let Value::Array(opts) = want else {
                return Shown::Unknown;
            };
            if opts.iter().any(Value::is_object) {
                return Shown::Unknown;
            }
            let cur = current(k);
            all &= cur.as_ref().is_some_and(|c| opts.contains(c));
        }
        if show && !all {
            verdict = Shown::No;
        }
        if !show && all && !map.is_empty() {
            verdict = Shown::No;
        }
    }
    verdict
}

fn empty_value(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        _ => false,
    }
}

fn check_node(n: &Node, nodes: &NodeRegistry, rep: &mut Report) {
    let label = format!("node `{}` ({})", n.name, n.node_type);
    let Ok(imp) = nodes.resolve(n) else {
        match nodes.latest(&n.node_type) {
            Some(i) => {
                let vs: Vec<String> = i
                    .description()
                    .version
                    .to_vec()
                    .iter()
                    .map(|v| v.to_string())
                    .collect();
                rep.errors.push(format!(
                    "{label}: typeVersion {} is not supported; use one of [{}]",
                    n.type_version,
                    vs.join(", ")
                ));
            }
            None => {
                let s = suggest(nodes, &n.node_type);
                let hint = if s.is_empty() {
                    "see the node registry for available types".to_owned()
                } else {
                    format!("did you mean: {}", s.join(", "))
                };
                rep.errors
                    .push(format!("{label}: unknown node type; {hint}"));
            }
        }
        return;
    };
    let d = imp.description();
    for p in &d.properties {
        if p.required != Some(true) || shown(p, &n.parameters, &d.properties) != Shown::Yes {
            continue;
        }
        let missing = match n.parameters.get(&p.name) {
            Some(v) => empty_value(v),
            None => p.default.as_ref().is_none_or(empty_value),
        };
        if missing {
            rep.errors.push(format!(
                "{label}: missing required parameter `{}` ({})",
                p.name, p.display_name
            ));
        }
    }
    for c in &d.credentials {
        let needed = c.required == Some(true) && c.display_options.is_none();
        if needed
            && !n
                .credentials
                .as_ref()
                .is_some_and(|m| m.contains_key(&c.name))
        {
            rep.warnings.push(format!(
                "{label}: needs credential `{}`; attach it in the canvas before running",
                c.name
            ));
        }
    }
}

fn io_count(decl: &IoDecl, conn_type: &str) -> Option<usize> {
    match decl {
        IoDecl::List(l) => Some(l.iter().filter(|s| s.conn_type() == conn_type).count()),
        IoDecl::Expression(_) => None,
    }
}

fn validate(
    wf_json: &Value,
    expand_errs: Vec<String>,
    nodes: &NodeRegistry,
) -> (Option<Workflow>, Report) {
    let mut rep = Report {
        errors: expand_errs,
        warnings: Vec::new(),
    };
    let wf = match Workflow::from_json_value(wf_json.clone()) {
        Ok(w) => w,
        Err(e) => {
            rep.errors.push(format!(
                "cannot read workflow: {e}; each node needs name, type, typeVersion, position [x, y]"
            ));
            return (None, rep);
        }
    };
    if let Err(e) = wf.validate() {
        let names: Vec<&str> = wf.nodes.iter().map(|n| n.name.as_str()).collect();
        rep.errors
            .push(format!("{e}; node names: [{}]", names.join(", ")));
    }
    for n in &wf.nodes {
        check_node(n, nodes, &mut rep);
    }

    // Port-level checks: only where the node declares a static port list.
    for (src, conns) in &wf.connections.0 {
        let Some(sn) = wf.node(src) else { continue };
        for (ct, outs) in &conns.0 {
            for (oi, targets) in outs.iter().enumerate() {
                let Some(ts) = targets.as_ref().filter(|t| !t.is_empty()) else {
                    continue;
                };
                if let Some(imp) = nodes.latest(&sn.node_type)
                    && let Some(n) = io_count(&imp.description().outputs, ct)
                    && oi >= n
                {
                    rep.errors.push(format!(
                        "connection from `{src}`: output {oi} does not exist ({n} `{ct}` output(s))"
                    ));
                }
                for t in ts {
                    let Some(tn) = wf.node(&t.node) else { continue };
                    if let Some(imp) = nodes.latest(&tn.node_type)
                        && let Some(n) = io_count(&imp.description().inputs, &t.conn_type)
                        && (t.index as usize) >= n
                    {
                        rep.errors.push(format!(
                            "connection `{src}` -> `{}`: input {} does not exist ({n} `{}` input(s)){}",
                            t.node,
                            t.index,
                            t.conn_type,
                            if n == 0 { "; triggers take no input" } else { "" }
                        ));
                    }
                }
            }
        }
    }

    // Reachability warnings.
    let mut fed: BTreeSet<&str> = BTreeSet::new();
    for (src, conns) in &wf.connections.0 {
        for (ct, outs) in &conns.0 {
            if ct != "main" && outs.iter().flatten().any(|t| !t.is_empty()) {
                fed.insert(src.as_str()); // sub-node feeding a root node
            }
            for t in outs.iter().flatten().flatten() {
                fed.insert(t.node.as_str());
            }
        }
    }
    let is_trigger = |n: &Node| {
        nodes
            .latest(&n.node_type)
            .is_some_and(|i| i.description().is_trigger())
    };
    if !wf.nodes.iter().any(|n| is_trigger(n) && !n.is_disabled()) {
        rep.warnings.push(
            "no trigger node; add `manualTrigger` so flow_run (and the canvas Test button) has a start".into(),
        );
    }
    for n in &wf.nodes {
        let note = n.node_type.ends_with(".stickyNote");
        if !note && !is_trigger(n) && !fed.contains(n.name.as_str()) {
            rep.warnings.push(format!(
                "node `{}` has no incoming connection and is not a trigger; it never runs",
                n.name
            ));
        }
        if n.is_disabled() {
            rep.warnings.push(format!("node `{}` is disabled", n.name));
        }
    }
    (Some(wf), rep)
}

/// Descriptions of enabled nodes that may reach beyond in-memory item shaping.
fn side_effects(wf: &Workflow) -> Vec<String> {
    wf.nodes
        .iter()
        .filter(|n| !n.is_disabled())
        .filter(|n| {
            !n.node_type
                .strip_prefix(NS_BASE)
                .is_some_and(|t| PURE.contains(&t))
        })
        .map(|n| format!("`{}` ({})", n.name, n.node_type))
        .collect()
}

// ------------------------------------------------------------------ helpers

fn args_obj<'a>(tool: &str, args: &'a Value) -> Result<&'a Map<String, Value>, ToolError> {
    args.as_object().ok_or_else(|| ToolError::InvalidArgs {
        tool: tool.into(),
        reason: "arguments must be an object".into(),
    })
}

fn bad(tool: &str, reason: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs {
        tool: tool.into(),
        reason: reason.into(),
    }
}

fn failed(tool: &str, reason: impl fmt::Display) -> ToolError {
    ToolError::Failed {
        tool: tool.into(),
        reason: reason.to_string(),
    }
}

fn json_out(v: &Value) -> ToolOutput {
    ToolOutput::ok(v.to_string())
}

fn report_err(rep: &Report) -> ToolOutput {
    ToolOutput::error(rep.to_json().to_string())
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}...", &s[..i]),
        None => s.to_owned(),
    }
}

/// `meta.hokkabaz.draft`, the flag marking workflows an agent wrote and no human has saved.
fn is_draft(data: &Value) -> bool {
    data.pointer("/meta/hokkabaz/draft")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn mark_draft(data: &mut Map<String, Value>) {
    let meta = data.entry("meta").or_insert_with(|| json!({}));
    if !meta.is_object() {
        *meta = json!({});
    }
    if let Value::Object(m) = meta {
        m.insert(
            "hokkabaz".into(),
            json!({ "draft": true, "author": "agent" }),
        );
    }
}

// ------------------------------------------------------------------ flow_author

struct FlowAuthor(Arc<Shared>);

impl fmt::Debug for FlowAuthor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FlowAuthor")
    }
}

const AUTHOR_DESC: &str = "Create, update, validate, list and read workflow drafts (n8n-compatible JSON). \
Drafts are stored inactive with draft=true; a human reviews and activates them in the canvas. \
`workflow` is either n8n workflow JSON ({name, nodes, connections}) or a compact spec: \
{name, nodes:[{name, type, parameters?}], links?:[[from,to,output?,input?]]}. In the compact form `type` may be a \
short name (`set`, `httpRequest`), typeVersion/position are filled in, and without `links` the nodes are chained in \
listed order. `update` replaces the whole draft and only works on drafts (never on human-saved or active workflows). \
Validation errors name the node and the fix; always run `validate` or check the create/update result before telling the user a flow is ready.";

#[async_trait]
impl Tool for FlowAuthor {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: AUTHOR.into(),
            description: AUTHOR_DESC.into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["create", "update", "validate", "list", "get"] },
                    "id": { "type": "string", "description": "Workflow id (update, get, validate a stored workflow)." },
                    "name": { "type": "string", "description": "Workflow name; overrides `workflow.name`." },
                    "workflow": { "type": "object", "description": "n8n workflow JSON or compact spec." },
                    "drafts_only": { "type": "boolean", "description": "list: only agent drafts (default false)." },
                    "query": { "type": "string", "description": "list: case-insensitive name filter." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "list: max rows (default 50)." }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let a = args_obj(AUTHOR, &args)?;
        let sh = &self.0;
        let id = a.get("id").and_then(Value::as_str);
        let name = a.get("name").and_then(Value::as_str);
        match a.get("action").and_then(Value::as_str) {
            Some("list") => self.list(a).await,
            Some("get") => {
                let id = id.ok_or_else(|| bad(AUTHOR, "`id` is required for get"))?;
                let Some(row) = sh
                    .store
                    .get_workflow(id)
                    .await
                    .map_err(|e| failed(AUTHOR, e))?
                else {
                    return Ok(ToolOutput::error(format!(
                        "workflow `{id}` not found; use action=list"
                    )));
                };
                let data = serde_json::from_str::<Value>(&row.data).unwrap_or(Value::Null);
                let wf = workflow_json(&row);
                let (n, errs) = normalize(&wf, sh.host.nodes());
                let (_, rep) = validate(&n, errs, sh.host.nodes());
                Ok(json_out(&json!({
                    "id": row.id, "name": row.name, "active": row.active, "draft": is_draft(&data),
                    "version_id": row.version_id, "validation": rep.to_json(), "workflow": wf,
                })))
            }
            Some("validate") => {
                let raw = match (a.get("workflow"), id) {
                    (Some(w), _) => w.clone(),
                    (None, Some(id)) => match sh
                        .store
                        .get_workflow(id)
                        .await
                        .map_err(|e| failed(AUTHOR, e))?
                    {
                        Some(row) => workflow_json(&row),
                        None => return Ok(ToolOutput::error(format!("workflow `{id}` not found"))),
                    },
                    (None, None) => return Err(bad(AUTHOR, "validate needs `workflow` or `id`")),
                };
                let (n, errs) = normalize(&raw, sh.host.nodes());
                let (_, rep) = validate(&n, errs, sh.host.nodes());
                Ok(json_out(&rep.to_json()))
            }
            Some(act @ ("create" | "update")) => {
                let raw = a
                    .get("workflow")
                    .ok_or_else(|| bad(AUTHOR, format!("`workflow` is required for {act}")))?;
                let mut raw = raw.clone();
                let keep = match (act, id) {
                    ("update", Some(id)) => sh
                        .store
                        .get_workflow(id)
                        .await
                        .map_err(|e| failed(AUTHOR, e))?
                        .map(|r| r.name),
                    _ => None,
                };
                if let Some(o) = raw.as_object_mut() {
                    // Explicit `name` wins, then the workflow's own, then the stored one on update.
                    if let Some(n) = name
                        .map(str::to_owned)
                        .or(keep)
                        .filter(|_| name.is_some() || !o.contains_key("name"))
                    {
                        o.insert("name".into(), json!(n));
                    }
                }
                let (n, errs) = normalize(&raw, sh.host.nodes());
                let (_, rep) = validate(&n, errs, sh.host.nodes());
                if !rep.errors.is_empty() {
                    return Ok(report_err(&rep));
                }
                let mut prepared = match prepare_workflow(n.clone()) {
                    Ok(p) => p,
                    Err(e) => return Ok(ToolOutput::error(e.to_string())),
                };
                mark_draft(&mut prepared.data);
                let data = Value::Object(prepared.data).to_string();
                let row = if act == "create" {
                    let (wid, vid) = (
                        new_id().map_err(|e| failed(AUTHOR, e))?,
                        new_version_id().map_err(|e| failed(AUTHOR, e))?,
                    );
                    sh.store
                        .insert_workflow(&wid, &prepared.name, &vid, &data)
                        .await
                        .map_err(|e| failed(AUTHOR, e))?
                } else {
                    let id = id.ok_or_else(|| bad(AUTHOR, "`id` is required for update"))?;
                    let Some(old) = sh
                        .store
                        .get_workflow(id)
                        .await
                        .map_err(|e| failed(AUTHOR, e))?
                    else {
                        return Ok(ToolOutput::error(format!(
                            "workflow `{id}` not found; use action=create"
                        )));
                    };
                    let old_data = serde_json::from_str::<Value>(&old.data).unwrap_or(Value::Null);
                    if old.active || !is_draft(&old_data) {
                        return Ok(ToolOutput::error(format!(
                            "workflow `{id}` is {}; only inactive agent drafts can be updated. Create a new draft instead.",
                            if old.active {
                                "active"
                            } else {
                                "not a draft (saved by a person)"
                            }
                        )));
                    }
                    let vid = new_version_id().map_err(|e| failed(AUTHOR, e))?;
                    let upd = sh
                        .store
                        .update_workflow(id, &prepared.name, &vid, &data)
                        .await
                        .map_err(|e| failed(AUTHOR, e))?;
                    match upd {
                        Some(r) => r,
                        None => {
                            return Ok(ToolOutput::error(format!(
                                "workflow `{id}` vanished during update"
                            )));
                        }
                    }
                };
                sh.pure().remove(&row.id);
                Ok(json_out(&json!({
                    "id": row.id, "name": row.name, "draft": true, "active": false,
                    "nodes": n.get("nodes").and_then(Value::as_array).map_or(0, Vec::len),
                    "warnings": rep.warnings,
                    "next": "saved as an inactive draft; the user opens it in the canvas to review and activate",
                })))
            }
            _ => Err(bad(
                AUTHOR,
                "`action` must be one of create, update, validate, list, get",
            )),
        }
    }
}

impl FlowAuthor {
    async fn list(&self, a: &Map<String, Value>) -> Result<ToolOutput, ToolError> {
        let limit = a
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(50, |l| l.clamp(1, 200)) as usize;
        let drafts_only = a
            .get("drafts_only")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let query = a.get("query").and_then(Value::as_str);
        let (mut out, mut offset) = (Vec::new(), 0u32);
        while out.len() < limit && offset < SCAN_LIMIT {
            let rows = self
                .0
                .store
                .list_workflows(None, query, 100, offset)
                .await
                .map_err(|e| failed(AUTHOR, e))?;
            if rows.is_empty() {
                break;
            }
            offset += u32::try_from(rows.len()).unwrap_or(u32::MAX);
            for r in rows {
                let data = serde_json::from_str::<Value>(&r.data).unwrap_or(Value::Null);
                let draft = is_draft(&data);
                if drafts_only && !draft {
                    continue;
                }
                let nodes = data
                    .get("nodes")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                out.push(json!({ "id": r.id, "name": r.name, "active": r.active, "draft": draft, "nodes": nodes, "updated_ms": r.updated_ms }));
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(json_out(&json!({ "workflows": out })))
    }
}

// ------------------------------------------------------------------ flow_run

struct FlowRun(Arc<Shared>);

impl fmt::Debug for FlowRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FlowRun")
    }
}

const RUN_DESC: &str = "Test-run a workflow once from its trigger with optional input items (manual mode; nothing is \
activated, error workflows and schedules do not fire). Give `workflow_id` (stored) or `workflow` (inline n8n JSON or \
compact spec, see flow_author). Returns status, per-node item counts, a truncated preview of the last node's output and \
the first error. Workflows with nodes that can reach outside the process (HTTP, sub-workflows, agents, integrations) need \
approval; pure data-shaping workflows run directly. Pinned node data is used instead of executing pinned nodes.";

#[async_trait]
impl Tool for FlowRun {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: RUN.into(),
            description: RUN_DESC.into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "workflow_id": { "type": "string", "description": "Stored workflow to run." },
                    "workflow": { "type": "object", "description": "Inline workflow (n8n JSON or compact spec); not stored." },
                    "items": { "type": "array", "items": { "type": "object" }, "description": "Input items (json objects) emitted by the trigger. Default: one empty item." },
                    "start_node": { "type": "string", "description": "Trigger node to start from. Default: the Manual Trigger." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECS, "description": "Default 60." }
                },
                "additionalProperties": false
            }),
        }
    }

    /// Inline: classified from the args. Stored: only a workflow last seen pure is auto-allowed,
    /// and `execute` re-checks the row, so an edit after classification cannot slip a
    /// side-effecting node past the approval gate.
    fn risk(&self, args: &Value) -> Risk {
        let sh = &self.0;
        if let Some(id) = args.get("workflow_id").and_then(Value::as_str) {
            return if sh.pure().contains(id) {
                Risk::Safe
            } else {
                Risk::Ask
            };
        }
        let Some(w) = args.get("workflow") else {
            return Risk::Ask;
        };
        let (n, _) = normalize(w, sh.host.nodes());
        match Workflow::from_json_value(n) {
            Ok(wf) if side_effects(&wf).is_empty() => Risk::Safe,
            _ => Risk::Ask,
        }
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let a = args_obj(RUN, &args)?;
        let sh = &self.0;
        let nodes = sh.host.nodes();

        let (raw, stored_id) = match (
            a.get("workflow_id").and_then(Value::as_str),
            a.get("workflow"),
        ) {
            (Some(_), Some(_)) => {
                return Err(bad(RUN, "give `workflow_id` or `workflow`, not both"));
            }
            (Some(id), None) => match sh
                .store
                .get_workflow(id)
                .await
                .map_err(|e| failed(RUN, e))?
            {
                Some(row) => (workflow_json(&row), Some(row.id)),
                None => {
                    return Ok(ToolOutput::error(format!(
                        "workflow `{id}` not found; use flow_author list"
                    )));
                }
            },
            (None, Some(w)) => (w.clone(), None),
            (None, None) => return Err(bad(RUN, "`workflow_id` or `workflow` is required")),
        };
        let (n, errs) = normalize(&raw, nodes);
        let (wf, rep) = validate(&n, errs, nodes);
        let Some(wf) = wf.filter(|_| rep.errors.is_empty()) else {
            return Ok(report_err(&rep));
        };

        let effects = side_effects(&wf);
        if let Some(id) = &stored_id {
            let was_pure = if effects.is_empty() {
                !sh.pure().insert(id.clone())
            } else {
                sh.pure().remove(id)
            };
            // Auto-allowed on stale knowledge but the row now has side effects: refuse, the next
            // call is classified Ask and goes through approval.
            if was_pure && !effects.is_empty() {
                return Ok(ToolOutput::error(format!(
                    "workflow `{id}` changed and now contains nodes with side effects ({}); call flow_run again to get approval",
                    effects.join(", ")
                )));
            }
        }

        let start = match a.get("start_node").and_then(Value::as_str) {
            Some(s) => match wf.node(s) {
                Some(node) if !node.is_disabled() => node.name.clone(),
                _ => {
                    return Ok(ToolOutput::error(format!(
                        "start_node `{s}` is not an enabled node"
                    )));
                }
            },
            None => match find_trigger(&wf, TriggerKind::Manual) {
                Some(t) => t.name.clone(),
                None => {
                    let others: Vec<&str> = wf
                        .nodes
                        .iter()
                        .filter(|n| {
                            nodes
                                .latest(&n.node_type)
                                .is_some_and(|i| i.description().is_trigger())
                        })
                        .map(|n| n.name.as_str())
                        .collect();
                    return Ok(ToolOutput::error(format!(
                        "workflow has no Manual Trigger; add `manualTrigger` or pass start_node (triggers: [{}])",
                        others.join(", ")
                    )));
                }
            },
        };
        let items = match a.get("items") {
            None | Some(Value::Null) => vec![Item::default()],
            Some(Value::Array(v)) if v.len() <= MAX_ITEMS_IN && v.iter().all(Value::is_object) => {
                v.iter().cloned().map(Item::from_json).collect()
            }
            Some(_) => {
                return Err(bad(
                    RUN,
                    format!("`items` must be an array of up to {MAX_ITEMS_IN} objects"),
                ));
            }
        };
        let secs = a
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS);

        let run = sh.host.run_test(TestRun {
            workflow: wf.clone(),
            workflow_id: stored_id.clone(),
            start_node: start,
            items,
        });
        let res = match tokio::time::timeout(Duration::from_secs(secs), run).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Ok(ToolOutput::error(format!("run failed: {e}"))),
            Err(_) => {
                return Ok(ToolOutput::error(format!(
                    "run exceeded {secs}s and was abandoned"
                )));
            }
        };
        let mut summary = summarize(&res, stored_id.as_deref(), &rep.warnings);
        sh.host.scrub(&mut summary);
        let text = summary.to_string();
        Ok(if res.status == RunStatus::Error {
            ToolOutput::error(text)
        } else {
            ToolOutput::ok(text)
        })
    }
}

/// Per-node counts, a bounded preview of the last node's first output, and the error.
fn summarize(res: &ExecutionResult, workflow_id: Option<&str>, warnings: &[String]) -> Value {
    let mut per_node: Vec<(usize, Value)> = res
        .run_data
        .iter()
        .map(|(name, runs)| {
            let width = runs.iter().map(|r| r.data.main.len()).max().unwrap_or(0);
            let mut counts = vec![0usize; width];
            for r in runs {
                for (i, c) in counts.iter_mut().enumerate() {
                    *c += r.output(i).len();
                }
            }
            let first = runs.iter().map(|r| r.execution_index).min().unwrap_or(0);
            let err = runs
                .iter()
                .find_map(|r| r.error.as_ref())
                .map(|e| e.message.clone());
            let mut v = json!({ "node": name, "runs": runs.len(), "items_out": counts });
            if let Some(e) = err {
                v["error"] = json!(e);
            }
            (first, v)
        })
        .collect();
    per_node.sort_by_key(|(i, _)| *i);

    let last = res
        .last_node_executed
        .as_deref()
        .and_then(|n| res.run_data.get(n).map(|r| (n, r)));
    let last_out = last.map(|(name, runs)| {
        let items = runs.last().map_or(&[][..], |r| r.output(0));
        let (mut shown, mut budget) = (Vec::new(), PREVIEW_TOTAL_CHARS);
        for it in items.iter().take(PREVIEW_ITEMS) {
            let s = Value::Object(it.json.clone()).to_string();
            let cost = s.chars().count().min(PREVIEW_ITEM_CHARS);
            if cost > budget {
                break;
            }
            budget -= cost;
            shown.push(if s.chars().count() > PREVIEW_ITEM_CHARS {
                json!({ "_truncated": truncate_chars(&s, PREVIEW_ITEM_CHARS) })
            } else {
                Value::Object(it.json.clone())
            });
        }
        json!({ "node": name, "item_count": items.len(), "items_shown": shown.len(), "items": shown })
    });

    let status = match res.status {
        RunStatus::Success => "success",
        RunStatus::Error => "error",
        RunStatus::Waiting => "waiting",
        RunStatus::Running => "running",
    };
    let mut out = json!({ "status": status, "execution_id": res.execution_id, "nodes": per_node.into_iter().map(|(_, v)| v).collect::<Vec<_>>() });
    if let Some(id) = workflow_id {
        out["workflow_id"] = json!(id);
    }
    if let Some(l) = last_out {
        out["last_node"] = l;
    }
    if let Some(e) = &res.error {
        out["error"] = json!({ "message": e.message, "node": e.node });
    }
    if !warnings.is_empty() {
        out["warnings"] = json!(warnings);
    }
    out
}

#[cfg(test)]
mod tests {
    use hk_flow::{ExecuteInput, node::NodeOutput};
    use hk_flow::{
        Executor, ExecutorConfig, FlowError, FlowNode, NodeContext, NodeTypeDescription,
        register_core_nodes,
    };

    use super::*;

    /// Registered node outside `PURE` with a required parameter.
    #[derive(Debug)]
    struct NeedsUrl(NodeTypeDescription);

    #[async_trait]
    impl FlowNode for NeedsUrl {
        fn description(&self) -> &NodeTypeDescription {
            &self.0
        }
        async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
            Ok(ctx.input.clone())
        }
    }

    struct Host {
        nodes: Arc<NodeRegistry>,
        exec: Executor,
    }

    impl fmt::Debug for Host {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("Host")
        }
    }

    #[async_trait]
    impl FlowToolHost for Host {
        fn nodes(&self) -> &NodeRegistry {
            &self.nodes
        }
        async fn run_test(&self, req: TestRun) -> Result<ExecutionResult, String> {
            let exec = self.exec.clone().with_config(ExecutorConfig {
                use_pin_data: true,
                ..ExecutorConfig::default()
            });
            exec.execute(&req.workflow, &ExecuteInput::new(req.start_node, req.items))
                .await
                .map_err(|e| e.to_string())
        }
    }

    struct Rig {
        author: Arc<dyn Tool>,
        run: Arc<dyn Tool>,
        store: Store,
    }

    async fn rig() -> Rig {
        let mut reg = NodeRegistry::new();
        register_core_nodes(&mut reg).unwrap();
        let desc = serde_json::from_value(json!({
            "displayName": "Needs Url", "name": "n8n-nodes-base.needsUrl", "version": 1,
            "properties": [{"displayName": "URL", "name": "url", "type": "string", "default": "", "required": true}]
        }))
        .unwrap();
        reg.register(Arc::new(NeedsUrl(desc))).unwrap();
        let nodes = Arc::new(reg);
        let host = Arc::new(Host {
            exec: Executor::new(nodes.clone()),
            nodes,
        });
        let store = Store::open_in_memory().await.unwrap();
        let mut tools = flow_tools(host, store.clone());
        let run = tools.pop().unwrap();
        let author = tools.pop().unwrap();
        Rig { author, run, store }
    }

    async fn call(t: &Arc<dyn Tool>, args: Value) -> (bool, Value) {
        let out = t.execute(ToolCtx::default(), args).await.unwrap();
        (
            out.is_error,
            serde_json::from_str(&out.content).unwrap_or(Value::String(out.content)),
        )
    }

    fn set_flow() -> Value {
        json!({
            "name": "Pure",
            "nodes": [
                {"name": "Start", "type": "manualTrigger"},
                {"name": "Mark", "type": "set", "parameters": {"assignments": {"assignments": [
                    {"id": "1", "name": "x", "value": 1, "type": "number"}]}}}
            ]
        })
    }

    fn errors(v: &Value) -> String {
        v["errors"].to_string()
    }

    #[tokio::test]
    async fn validate_reports_actionable_errors() {
        let r = rig().await;
        let spec = json!({"nodes": [
            {"name": "Start", "type": "manualTrigger"},
            {"name": "A", "type": "sett"},
            {"name": "B", "type": "needsUrl"}],
            "links": [["Start", "A"], ["A", "Ghost"], ["A", "Start"]]});
        let (err, v) = call(&r.author, json!({"action": "validate", "workflow": spec})).await;
        assert!(!err && v["ok"] == false);
        let e = errors(&v);
        assert!(
            e.contains("unknown node type")
                && e.contains("did you mean")
                && e.contains("n8n-nodes-base.set"),
            "{e}"
        );
        assert!(e.contains("missing required parameter `url`"), "{e}");
        assert!(e.contains("unknown node `Ghost`"), "{e}");
        assert!(e.contains("triggers take no input"), "{e}");
    }

    #[tokio::test]
    async fn invalid_draft_is_not_stored() {
        let r = rig().await;
        let spec = json!({"name": "Bad", "nodes": [{"name": "B", "type": "needsUrl"}]});
        let (err, v) = call(&r.author, json!({"action": "create", "workflow": spec})).await;
        assert!(err, "{v}");
        assert!(
            r.store
                .list_workflows(None, None, 10, 0)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn drafts_create_update_list_get() {
        let r = rig().await;
        let (err, v) = call(
            &r.author,
            json!({"action": "create", "workflow": set_flow()}),
        )
        .await;
        assert!(!err, "{v}");
        let id = v["id"].as_str().unwrap().to_owned();
        assert_eq!(v["draft"], true);

        let row = r.store.get_workflow(&id).await.unwrap().unwrap();
        assert!(!row.active && is_draft(&serde_json::from_str(&row.data).unwrap()));

        let mut next = set_flow();
        next["name"] = json!("Renamed");
        let (err, v) = call(
            &r.author,
            json!({"action": "update", "id": id, "workflow": next}),
        )
        .await;
        assert!(!err && v["name"] == "Renamed", "{v}");

        let (_, v) = call(&r.author, json!({"action": "list", "drafts_only": true})).await;
        assert_eq!(v["workflows"][0]["id"], id.as_str());
        let (_, v) = call(&r.author, json!({"action": "get", "id": id})).await;
        assert_eq!(v["validation"]["ok"], true);
        assert_eq!(v["workflow"]["nodes"].as_array().unwrap().len(), 2);

        // A person saving it (no draft flag) locks it against agent updates.
        r.store
            .update_workflow(&id, "Mine", "v2", r#"{"nodes":[],"connections":{}}"#)
            .await
            .unwrap();
        let (err, v) = call(
            &r.author,
            json!({"action": "update", "id": id, "workflow": set_flow()}),
        )
        .await;
        assert!(
            err && v.as_str().is_some_and(|s| s.contains("not a draft")),
            "{v}"
        );
    }

    #[tokio::test]
    async fn run_summarizes_scripted_set_workflow() {
        let r = rig().await;
        let args = json!({"workflow": set_flow(), "items": [{"a": 1}, {"a": 2}]});
        assert_eq!(r.run.risk(&args), Risk::Safe);
        let (err, v) = call(&r.run, args).await;
        assert!(!err, "{v}");
        assert_eq!(v["status"], "success");
        let mark = v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["node"] == "Mark")
            .unwrap();
        assert_eq!(mark["items_out"], json!([2]));
        assert_eq!(v["last_node"]["node"], "Mark");
        assert_eq!(v["last_node"]["item_count"], 2);
        assert_eq!(v["last_node"]["items"][0], json!({"x": 1}));
    }

    #[tokio::test]
    async fn run_reports_validation_and_missing_trigger() {
        let r = rig().await;
        let (err, v) = call(
            &r.run,
            json!({"workflow": {"nodes": [{"name": "B", "type": "needsUrl"}]}}),
        )
        .await;
        assert!(err && errors(&v).contains("`url`"), "{v}");
        let no_trigger = json!({"nodes": [{"name": "M", "type": "noOp"}]});
        let (err, v) = call(&r.run, json!({"workflow": no_trigger})).await;
        assert!(
            err && v.as_str().is_some_and(|s| s.contains("no Manual Trigger")),
            "{v}"
        );
    }

    #[tokio::test]
    async fn side_effect_nodes_ask_and_stale_classification_is_refused() {
        let r = rig().await;
        let impure = json!({"nodes": [
            {"name": "Start", "type": "manualTrigger"},
            {"name": "Call", "type": "needsUrl", "parameters": {"url": "http://x"}}]});
        assert_eq!(r.run.risk(&json!({"workflow": impure})), Risk::Ask);

        // Stored: unknown to the pure cache -> Ask; after a pure run -> Safe.
        let (_, v) = call(
            &r.author,
            json!({"action": "create", "workflow": set_flow()}),
        )
        .await;
        let id = v["id"].as_str().unwrap().to_owned();
        let args = json!({"workflow_id": id});
        assert_eq!(r.run.risk(&args), Risk::Ask);
        let (err, v) = call(&r.run, args.clone()).await;
        assert!(!err, "{v}");
        assert_eq!(r.run.risk(&args), Risk::Safe);

        // Edited to add a side-effect node: the auto-allowed call must not run.
        // Via the store, like a REST/UI save would: the tool's own update drops the cache entry.
        let edited = json!({"nodes": [
            {"name": "Start", "type": "n8n-nodes-base.manualTrigger", "typeVersion": 1, "position": [0, 0], "parameters": {}},
            {"name": "Call", "type": "n8n-nodes-base.needsUrl", "typeVersion": 1, "position": [0, 0], "parameters": {"url": "http://x"}}],
            "connections": {"Start": {"main": [[{"node": "Call", "type": "main", "index": 0}]]}}});
        r.store
            .update_workflow(&id, "Edited", "v2", &edited.to_string())
            .await
            .unwrap();
        let (err, v) = call(&r.run, args.clone()).await;
        assert!(
            err && v.as_str().is_some_and(|s| s.contains("side effects")),
            "{v}"
        );
        assert_eq!(r.run.risk(&args), Risk::Ask);
    }
}
