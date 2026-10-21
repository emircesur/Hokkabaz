//! QuickJS-backed [`ExpressionEvaluator`].
//!
//! `rquickjs` runtimes are not `Send`, and QuickJS measures stack depth from the thread
//! it was created on, so one dedicated worker thread owns every runtime. Callers send
//! requests over a channel and block for the reply. The worker keeps one JS realm
//! ("session") per execution id: the shim is evaluated once and compiled expressions are
//! cached inside it, so repeated evaluations over many items stay cheap.
//!
//! The shim reaches Rust through one string-in/string-out dispatcher (`host_op`) that is
//! only visible to the shim closure; it is deleted from the global object before any
//! expression runs.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use rquickjs::function::Opt;
use rquickjs::{CatchResultExt, CaughtError, Context, Function, Persistent, Promise, Runtime};
use serde_json::Value;

use super::template::{Part, parse_template};
use super::{ExprError, ExprLimits, ExprScope, ExpressionEvaluator, ScriptMode, ScriptOutput};
use crate::item::PairedItem;

/// Concatenated so the three files share one closure scope.
const SHIM: &str = concat!(
    "(function(H){\n'use strict';\n",
    include_str!("shim/datetime.js"),
    "\n",
    include_str!("shim/ext.js"),
    "\n",
    include_str!("shim/core.js"),
    "\n})"
);

/// Session key for evaluations outside an execution.
const NO_EXECUTION: &str = "";

static DEFAULT: OnceLock<Arc<QuickJsEvaluator>> = OnceLock::new();

/// Process-wide evaluator used when `ExprEnv::evaluator` is unset.
pub fn default_evaluator() -> Arc<dyn ExpressionEvaluator> {
    Arc::clone(DEFAULT.get_or_init(|| Arc::new(QuickJsEvaluator::new(ExprLimits::default()))))
        as Arc<dyn ExpressionEvaluator>
}

/// Drop the default evaluator's session for `execution_id`. A no-op (and no worker
/// thread start) when the default evaluator was never used.
pub fn release_default(execution_id: &str) {
    if let Some(d) = DEFAULT.get() {
        d.release(execution_id);
    }
}

enum Req {
    Eval {
        key: String,
        template: String,
        scope: ExprScope,
        reply: SyncSender<Result<Value, ExprError>>,
    },
    Script {
        key: String,
        source: String,
        mode: ScriptMode,
        scope: ExprScope,
        reply: SyncSender<Result<ScriptOutput, ExprError>>,
    },
    Release(String),
}

pub struct QuickJsEvaluator {
    tx: Mutex<Sender<Req>>,
}

impl std::fmt::Debug for QuickJsEvaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("QuickJsEvaluator")
    }
}

impl QuickJsEvaluator {
    pub fn new(limits: ExprLimits) -> Self {
        let (tx, rx) = mpsc::channel::<Req>();
        // A failed spawn drops `rx`; every later send then reports `Unavailable`.
        let _ = std::thread::Builder::new()
            .name("hk-expr".into())
            .stack_size(limits.stack_bytes + 3 * 1024 * 1024)
            .spawn(move || worker(rx, limits));
        Self { tx: Mutex::new(tx) }
    }

    fn send(&self, r: Req) -> bool {
        self.tx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(r)
            .is_ok()
    }
}

impl ExpressionEvaluator for QuickJsEvaluator {
    fn evaluate(&self, template: &str, scope: &ExprScope) -> Result<Value, ExprError> {
        let (reply, rx) = mpsc::sync_channel(1);
        let key = scope
            .base
            .execution_id
            .clone()
            .unwrap_or_else(|| NO_EXECUTION.to_owned());
        let sent = self.send(Req::Eval {
            key,
            template: template.to_owned(),
            scope: scope.clone(),
            reply,
        });
        if !sent {
            return Err(ExprError::Unavailable("worker stopped".into()));
        }
        rx.recv()
            .map_err(|_| ExprError::Unavailable("worker stopped".into()))?
    }

    fn run_script(
        &self,
        source: &str,
        mode: ScriptMode,
        scope: &ExprScope,
    ) -> Result<ScriptOutput, ExprError> {
        let (reply, rx) = mpsc::sync_channel(1);
        let key = scope
            .base
            .execution_id
            .clone()
            .unwrap_or_else(|| NO_EXECUTION.to_owned());
        let sent = self.send(Req::Script {
            key,
            source: source.to_owned(),
            mode,
            scope: scope.clone(),
            reply,
        });
        if !sent {
            return Err(ExprError::Unavailable("worker stopped".into()));
        }
        rx.recv()
            .map_err(|_| ExprError::Unavailable("worker stopped".into()))?
    }

    fn release(&self, execution_id: &str) {
        self.send(Req::Release(execution_id.to_owned()));
    }
}

// ------------------------------------------------------------------ worker

fn worker(rx: mpsc::Receiver<Req>, limits: ExprLimits) {
    // Oldest first; evicting the front keeps memory bounded when executions leak.
    let mut sessions: VecDeque<(String, Session)> = VecDeque::new();
    while let Ok(req) = rx.recv() {
        match req {
            Req::Release(key) => sessions.retain(|(k, _)| *k != key),
            Req::Eval {
                key,
                template,
                scope,
                reply,
            } => {
                let r = with_session(&mut sessions, &limits, &key, |s| {
                    run_in(s, &template, &scope)
                });
                let _ = reply.send(r);
            }
            Req::Script {
                key,
                source,
                mode,
                scope,
                reply,
            } => {
                let r = with_session(&mut sessions, &limits, &key, |s| {
                    run_script_in(s, &source, mode, &scope)
                });
                let _ = reply.send(r);
            }
        }
    }
}

fn with_session<R>(
    sessions: &mut VecDeque<(String, Session)>,
    limits: &ExprLimits,
    key: &str,
    f: impl FnOnce(&mut Session) -> Result<R, ExprError>,
) -> Result<R, ExprError> {
    let pos = sessions.iter().position(|(k, _)| k == key);
    let result = match pos {
        Some(p) => f(&mut sessions[p].1),
        None => match Session::new(limits) {
            Ok(mut s) => {
                let r = f(&mut s);
                if sessions.len() >= limits.max_sessions.max(1) {
                    sessions.pop_front();
                }
                sessions.push_back((key.to_owned(), s));
                r
            }
            Err(e) => Err(e),
        },
    };
    // A blown memory limit or interrupt can leave the realm half-torn: rebuild.
    if matches!(result, Err(ExprError::Memory | ExprError::Timeout)) {
        sessions.retain(|(k, _)| k != key);
    }
    result
}

fn run_script_in(
    s: &mut Session,
    source: &str,
    mode: ScriptMode,
    scope: &ExprScope,
) -> Result<ScriptOutput, ExprError> {
    s.bind(scope);
    let raw = s.call_script(
        source,
        match mode {
            ScriptMode::AllItems => MODE_SCRIPT_ALL,
            ScriptMode::EachItem => MODE_SCRIPT_EACH,
        },
    )?;
    #[derive(serde::Deserialize)]
    struct Out {
        #[serde(default)]
        u: Value,
        #[serde(default)]
        r: Value,
        #[serde(default)]
        logs: Vec<String>,
    }
    let o: Out = serde_json::from_str(&raw)
        .map_err(|e| ExprError::Js(format!("script result is not JSON: {e}")))?;
    let undefined = match (&o.u, mode) {
        (Value::Bool(true), ScriptMode::AllItems) => vec![0],
        (Value::Array(a), ScriptMode::EachItem) => a
            .iter()
            .filter_map(|x| x.as_u64().map(|n| n as usize))
            .collect(),
        _ => Vec::new(),
    };
    Ok(ScriptOutput {
        value: o.r,
        undefined,
        logs: o.logs,
    })
}

fn run_in(s: &mut Session, template: &str, scope: &ExprScope) -> Result<Value, ExprError> {
    s.bind(scope);
    let parts = parse_template(template);
    match parts.as_slice() {
        [] => Ok(Value::String(String::new())),
        [Part::Text(t)] => Ok(Value::String(t.clone())),
        [Part::Expr(src)] => {
            let json = s.call(src, MODE_JSON)?;
            if json.is_empty() {
                Ok(Value::Null)
            } else {
                serde_json::from_str(&json)
                    .map_err(|e| ExprError::Js(format!("expression result is not JSON: {e}")))
            }
        }
        many => {
            let mut out = String::new();
            for p in many {
                match p {
                    Part::Text(t) => out.push_str(t),
                    Part::Expr(src) => out.push_str(&s.call(src, MODE_TEXT)?),
                }
            }
            Ok(Value::String(out))
        }
    }
}

const MODE_JSON: u8 = 0;
const MODE_TEXT: u8 = 1;
const MODE_SCRIPT_ALL: u8 = 2;
const MODE_SCRIPT_EACH: u8 = 3;

/// State the host dispatcher reads; shared with the JS closures.
#[derive(Default)]
struct Shared {
    scope: RefCell<Option<ExprScope>>,
    /// Bumped when the shared [`ScopeBase`](super::ScopeBase) changes so the shim drops
    /// its parsed-input caches; items of one node run keep them.
    gen_: Cell<u64>,
    deadline: Cell<Option<Instant>>,
}

struct Session {
    // Field order = drop order: realm handles before the runtime.
    api: Persistent<Function<'static>>,
    ctx: Context,
    _rt: Runtime,
    shared: Rc<Shared>,
    timeout: Duration,
}

impl Session {
    fn new(limits: &ExprLimits) -> Result<Self, ExprError> {
        let unavailable = |e: rquickjs::Error| ExprError::Unavailable(e.to_string());
        let rt = Runtime::new().map_err(unavailable)?;
        rt.set_memory_limit(limits.memory_bytes);
        rt.set_max_stack_size(limits.stack_bytes);
        let shared = Rc::new(Shared::default());
        let sh = Rc::clone(&shared);
        rt.set_interrupt_handler(Some(Box::new(move || {
            sh.deadline.get().is_some_and(|d| Instant::now() >= d)
        })));
        let ctx = Context::full(&rt).map_err(unavailable)?;
        let sh = Rc::clone(&shared);
        let api = ctx.with(|ctx| -> Result<_, ExprError> {
            let host = Function::new(
                ctx.clone(),
                move |op: String, a: Opt<String>, b: Opt<String>, c: Opt<String>| -> String {
                    match host_op(&sh, &op, a.0.as_deref(), b.0.as_deref(), c.0.as_deref()) {
                        Ok(s) => s,
                        // \u{1} prefix: the shim rethrows as a JS Error.
                        Err(e) => format!("\u{1}{e}"),
                    }
                },
            )
            .map_err(unavailable)?;
            let install: Function = ctx
                .eval(SHIM)
                .catch(&ctx)
                .map_err(|e| ExprError::Unavailable(format!("shim: {e}")))?;
            let api: Function = install
                .call((host,))
                .catch(&ctx)
                .map_err(|e| ExprError::Unavailable(format!("shim init: {e}")))?;
            Ok(Persistent::save(&ctx, api))
        })?;
        Ok(Self {
            api,
            ctx,
            _rt: rt,
            shared,
            timeout: limits.timeout,
        })
    }

    fn bind(&self, scope: &ExprScope) {
        let mut cur = self.shared.scope.borrow_mut();
        let same = cur
            .as_ref()
            .is_some_and(|c| Arc::ptr_eq(&c.base, &scope.base));
        if !same {
            self.shared.gen_.set(self.shared.gen_.get() + 1);
        }
        *cur = Some(scope.clone());
    }

    fn call(&self, src: &str, mode: u8) -> Result<String, ExprError> {
        self.shared
            .deadline
            .set(Some(Instant::now() + self.timeout));
        let out = self.ctx.with(|ctx| {
            let f = match self.api.clone().restore(&ctx) {
                Ok(f) => f,
                Err(e) => return Err(ExprError::Unavailable(e.to_string())),
            };
            f.call::<_, String>((src, mode as i32))
                .catch(&ctx)
                .map_err(js_error)
        });
        self.shared.deadline.set(None);
        out
    }

    /// Script modes return a promise (async body); drive the job queue until it settles.
    fn call_script(&self, src: &str, mode: u8) -> Result<String, ExprError> {
        let deadline = Instant::now() + self.timeout;
        self.shared.deadline.set(Some(deadline));
        let out = self.ctx.with(|ctx| {
            let f = match self.api.clone().restore(&ctx) {
                Ok(f) => f,
                Err(e) => return Err(ExprError::Unavailable(e.to_string())),
            };
            let p: Promise = f
                .call((src, mode as i32))
                .catch(&ctx)
                .map_err(js_error)?;
            p.finish::<String>().catch(&ctx).map_err(js_error)
        });
        self.shared.deadline.set(None);
        // An interrupt inside a promise job is swallowed by the job queue: the promise just
        // never settles. Past the deadline that can only mean the timeout fired.
        match out {
            Err(ExprError::Js(_)) if Instant::now() >= deadline => Err(ExprError::Timeout),
            other => other,
        }
    }
}

fn js_error(e: CaughtError<'_>) -> ExprError {
    let msg = match &e {
        CaughtError::Exception(x) => x.message().unwrap_or_default(),
        CaughtError::Value(v) => format!("{v:?}"),
        CaughtError::Error(rquickjs::Error::WouldBlock) => {
            "script never finished: it awaits something that can't resolve in the sandbox".into()
        }
        CaughtError::Error(x) => x.to_string(),
    };
    if let Some(err) = script_error(&msg) {
        return err;
    }
    let lower = msg.to_ascii_lowercase();
    if lower.contains("interrupted") {
        ExprError::Timeout
    } else if lower.contains("out of memory") {
        ExprError::Memory
    } else {
        ExprError::Js(msg)
    }
}

/// Decode the envelope the shim wraps user-code errors in (`\u{1}S{json}`).
fn script_error(msg: &str) -> Option<ExprError> {
    #[derive(serde::Deserialize)]
    struct Env {
        m: String,
        #[serde(default)]
        n: String,
        l: Option<u32>,
        i: Option<usize>,
    }
    let env: Env = serde_json::from_str(msg.strip_prefix("\u{1}S")?).ok()?;
    // The user script may catch and rethrow resource errors; keep them classified.
    let lower = env.m.to_ascii_lowercase();
    if env.n == "InternalError" && lower.contains("out of memory") {
        return Some(ExprError::Memory);
    }
    if env.n == "InternalError" && lower.contains("interrupted") {
        return Some(ExprError::Timeout);
    }
    Some(ExprError::Script {
        message: if env.n.is_empty() || env.n == "Error" {
            env.m
        } else {
            format!("{}: {}", env.n, env.m)
        },
        line: env.l,
        item_index: env.i,
    })
}

// ----------------------------------------------------------------- host ops

fn host_op(
    sh: &Shared,
    op: &str,
    a: Option<&str>,
    b: Option<&str>,
    c: Option<&str>,
) -> Result<String, String> {
    // Pure helpers need no scope.
    match op {
        "tzValid" => {
            return Ok(if a.is_some_and(|z| z.parse::<chrono_tz::Tz>().is_ok()) {
                "1".into()
            } else {
                "0".into()
            });
        }
        "tzOffset" => return tz_offset(a, b),
        "hash" => return hash(a, b, c),
        "jmespath" => return jmespath(a, b),
        _ => {}
    }
    let guard = sh.scope.borrow();
    let scope = guard.as_ref().ok_or("no active expression scope")?;
    let base = &*scope.base;
    let arg = |x: Option<&str>| x.unwrap_or_default().to_owned();
    let num = |x: Option<&str>| x.and_then(|s| s.parse::<i64>().ok());
    match op {
        "meta" => Ok(serde_json::json!({
            "gen": sh.gen_.get(),
            "itemIndex": scope.item_index,
            "runIndex": base.run_index,
            "nodeName": base.node_name,
            "timezone": base.timezone,
            "now": base.now_ms.unwrap_or_else(now_ms),
            "mode": base.mode,
        })
        .to_string()),
        "params" => Ok(base.node_params.to_string()),
        "workflow" => Ok(serde_json::json!({
            "id": base.workflow_id, "name": base.workflow_name, "active": base.workflow_active
        })
        .to_string()),
        "execution" => Ok(serde_json::json!({
            "id": base.execution_id, "mode": base.mode, "resumeUrl": base.resume_url
        })
        .to_string()),
        "env" => to_json(&*base.env),
        "vars" => to_json(&*base.vars),
        "input" => {
            let port = num(a).unwrap_or(0) as usize;
            to_json(base.input.get(port).map_or(&[][..], Vec::as_slice))
        }
        "prevNode" => Ok(match base.source.first().and_then(Option::as_ref) {
            Some(s) => serde_json::json!({
                "name": s.previous_node,
                "outputIndex": s.previous_node_output.unwrap_or(0),
                "runIndex": s.previous_node_run.unwrap_or(0),
            })
            .to_string(),
            None => String::new(),
        }),
        "nodeParams" => {
            let name = arg(a);
            Ok(base
                .node_params_all
                .get(&name)
                .map(Value::to_string)
                .unwrap_or_default())
        }
        "runCount" => Ok(base.run_data.run_count(&arg(a)).to_string()),
        // items of (node, branch, run); run -1 = last
        "run" => {
            let name = arg(a);
            let branch = num(b).unwrap_or(0).max(0) as usize;
            let run = num(c).unwrap_or(-1);
            node_items(base, &name, branch, run)
        }
        // (run, output, item) the current item derives from in `node`
        "paired" => {
            let name = arg(a);
            let idx = num(b).map_or(scope.item_index, |i| i.max(0) as usize);
            let (run, out, item) = trace_paired(base, &name, idx)?;
            Ok(format!("{run},{out},{item}"))
        }
        _ => Err(format!("unknown host op {op}")),
    }
}

fn to_json<T: serde::Serialize + ?Sized>(v: &T) -> Result<String, String> {
    serde_json::to_string(v).map_err(|e| e.to_string())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn node_items(
    base: &super::ScopeBase,
    name: &str,
    branch: usize,
    run: i64,
) -> Result<String, String> {
    let count = base.run_data.run_count(name);
    if count == 0 {
        return Err(if base.node_params_all.contains_key(name) {
            format!(
                "An expression references the node '{name}', but it hasn't been executed yet. \
                 Either change the expression, or re-wire your workflow to make sure that node executes first."
            )
        } else {
            format!("Referenced node '{name}' doesn't exist")
        });
    }
    let idx = if run < 0 { count - 1 } else { run as usize };
    let r = base
        .run_data
        .run(name, idx)
        .ok_or_else(|| format!("Node '{name}' has no run {idx}"))?;
    to_json(r.output(branch))
}

/// Walk back through `pairedItem` links from the current input item to `target`.
fn trace_paired(
    base: &super::ScopeBase,
    target: &str,
    item_index: usize,
) -> Result<(usize, usize, usize), String> {
    let unavailable = |node: &str| {
        format!(
            "Paired item data for item from node '{node}' is unavailable. \
             Ensure '{node}' is providing the required output."
        )
    };
    let src = base
        .source
        .first()
        .and_then(Option::as_ref)
        .ok_or_else(|| {
            format!(
                "Can't get data for expression under '{}': the node has no input to pair '{target}' with",
                base.node_name
            )
        })?;
    let mut node = src.previous_node.clone();
    let mut out = src.previous_node_output.unwrap_or(0);
    let mut run = src.previous_node_run.unwrap_or(0);
    let mut idx = item_index;
    // Cycle guard: loops revisit nodes, but a chain longer than this is corrupt data.
    for _ in 0..10_000 {
        if node == target {
            return Ok((run, out, idx));
        }
        let r = base
            .run_data
            .run(&node, run)
            .ok_or_else(|| unavailable(&node))?;
        let item = r.output(out).get(idx).ok_or_else(|| unavailable(&node))?;
        let (port, parent) = match &item.paired_item {
            Some(PairedItem::Index(i)) => (0, *i),
            Some(PairedItem::Source(s)) => (s.input.unwrap_or(0), s.item),
            Some(PairedItem::Many(v)) => v
                .first()
                .map(|s| (s.input.unwrap_or(0), s.item))
                .ok_or_else(|| unavailable(&node))?,
            // Only unambiguous when the node saw exactly one input item.
            None => (0, 0),
        };
        if item.paired_item.is_none() {
            let up = r.source.first().and_then(Option::as_ref);
            let single = up.is_some_and(|u| {
                base.run_data
                    .run(&u.previous_node, u.previous_node_run.unwrap_or(0))
                    .is_some_and(|p| p.output(u.previous_node_output.unwrap_or(0)).len() == 1)
            });
            if !single {
                return Err(unavailable(&node));
            }
        }
        let s = r
            .source
            .get(port)
            .and_then(Option::as_ref)
            .ok_or_else(|| unavailable(&node))?;
        node = s.previous_node.clone();
        out = s.previous_node_output.unwrap_or(0);
        run = s.previous_node_run.unwrap_or(0);
        idx = parent;
    }
    Err(unavailable(target))
}

fn tz_offset(zone: Option<&str>, ms: Option<&str>) -> Result<String, String> {
    use chrono::{Offset, TimeZone};
    let tz: chrono_tz::Tz = zone
        .and_then(|z| z.parse().ok())
        .ok_or("invalid time zone")?;
    let ms: i64 = ms.and_then(|m| m.parse().ok()).unwrap_or(0);
    let secs = chrono::DateTime::from_timestamp_millis(ms).map_or(0, |dt| {
        tz.offset_from_utc_datetime(&dt.naive_utc())
            .fix()
            .local_minus_utc()
    });
    Ok((f64::from(secs) / 60.0).to_string())
}

fn hash(algo: Option<&str>, text: Option<&str>, enc: Option<&str>) -> Result<String, String> {
    use md5::Md5;
    use sha1::{Digest, Sha1};
    use sha2::{Sha224, Sha256, Sha384, Sha512};
    fn go<D: Digest>(t: &str) -> Vec<u8> {
        D::digest(t.as_bytes()).to_vec()
    }
    let t = text.unwrap_or_default();
    let bytes = match algo.unwrap_or("md5").to_ascii_lowercase().as_str() {
        "md5" => go::<Md5>(t),
        "sha1" => go::<Sha1>(t),
        "sha224" => go::<Sha224>(t),
        "sha256" => go::<Sha256>(t),
        "sha384" => go::<Sha384>(t),
        "sha512" => go::<Sha512>(t),
        other => return Err(format!("unsupported hash algorithm '{other}'")),
    };
    if enc == Some("base64") {
        return Ok(base64(&bytes));
    }
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for ch in bytes.chunks(3) {
        let n = (u32::from(ch[0]) << 16)
            | (u32::from(*ch.get(1).unwrap_or(&0)) << 8)
            | u32::from(*ch.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if ch.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if ch.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn jmespath(data: Option<&str>, expr: Option<&str>) -> Result<String, String> {
    let compiled = jmespath::compile(expr.unwrap_or_default()).map_err(|e| e.to_string())?;
    let data = jmespath::Variable::from_json(data.unwrap_or("null"))?;
    let found = compiled.search(data).map_err(|e| e.to_string())?;
    serde_json::to_string(&*found).map_err(|e| e.to_string())
}
