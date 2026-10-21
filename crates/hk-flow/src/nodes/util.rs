//! Helpers shared by the core nodes.

use std::cmp::Ordering;

use serde_json::{Map, Number, Value};

use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::item::{Item, PairedItem};
use crate::node::NodeContext;

/// Build a static description. The JSON is a literal in this crate; the registration
/// test parses every one, so a typo fails CI, not a user.
pub(crate) fn describe(v: Value) -> NodeTypeDescription {
    serde_json::from_value(v).expect("static node description")
}

pub(crate) fn node_err(ctx: &NodeContext, message: impl Into<String>) -> FlowError {
    FlowError::Node {
        node: ctx.node.name.clone(),
        message: message.into(),
    }
}

/// Items on input port `idx` (empty when the port has none).
pub(crate) fn port(ctx: &NodeContext, idx: usize) -> &[Item] {
    ctx.input.get(idx).map(Vec::as_slice).unwrap_or(&[])
}

pub(crate) fn paired(i: usize) -> Option<PairedItem> {
    Some(PairedItem::Index(i))
}

// ----------------------------------------------------------------- parameters

/// Evaluated parameter at a dotted path; `None` when absent.
pub(crate) fn param(ctx: &NodeContext, path: &str, i: usize) -> Result<Option<Value>, FlowError> {
    ctx.eval_param_at(path, i)
}

pub(crate) fn param_or(
    ctx: &NodeContext,
    path: &str,
    i: usize,
    default: Value,
) -> Result<Value, FlowError> {
    Ok(param(ctx, path, i)?.unwrap_or(default))
}

pub(crate) fn param_str(
    ctx: &NodeContext,
    path: &str,
    i: usize,
    default: &str,
) -> Result<String, FlowError> {
    Ok(match param(ctx, path, i)? {
        Some(Value::String(s)) => s,
        Some(Value::Null) | None => default.to_owned(),
        Some(v) => to_display(&v),
    })
}

pub(crate) fn param_bool(
    ctx: &NodeContext,
    path: &str,
    i: usize,
    default: bool,
) -> Result<bool, FlowError> {
    Ok(match param(ctx, path, i)? {
        Some(Value::Bool(b)) => b,
        Some(Value::String(s)) if s == "true" => true,
        Some(Value::String(s)) if s == "false" => false,
        _ => default,
    })
}

pub(crate) fn param_f64(
    ctx: &NodeContext,
    path: &str,
    i: usize,
    default: f64,
) -> Result<f64, FlowError> {
    Ok(match param(ctx, path, i)? {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
        _ => default,
    })
}

pub(crate) fn param_usize(
    ctx: &NodeContext,
    path: &str,
    i: usize,
    default: usize,
) -> Result<usize, FlowError> {
    let f = param_f64(ctx, path, i, default as f64)?;
    Ok(if f.is_finite() && f >= 0.0 { f as usize } else { default })
}

/// Comma separated list parameter (`"a, b"`), trimmed, empties dropped.
pub(crate) fn param_list(ctx: &NodeContext, path: &str, i: usize) -> Result<Vec<String>, FlowError> {
    Ok(match param(ctx, path, i)? {
        Some(Value::String(s)) => split_list(&s),
        Some(Value::Array(a)) => a.iter().map(to_display).collect(),
        _ => Vec::new(),
    })
}

pub(crate) fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Raw (unevaluated) parameter at a dotted path.
pub(crate) fn raw_param<'a>(ctx: &'a NodeContext, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.');
    let mut cur = ctx.node.parameters.get(parts.next()?)?;
    for p in parts {
        cur = cur.get(p)?;
    }
    Some(cur)
}

/// Number of entries of a raw array parameter (`rules.values`, ...).
pub(crate) fn raw_array<'a>(ctx: &'a NodeContext, path: &str) -> &'a [Value] {
    raw_param(ctx, path)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

// ------------------------------------------------------------- value helpers

pub(crate) fn num(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9.0e15 {
        Value::Number(Number::from(f as i64))
    } else {
        Number::from_f64(f).map_or(Value::Null, Value::Number)
    }
}

pub(crate) fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// JS-ish string conversion: strings bare, scalars printed, containers as JSON.
pub(crate) fn to_display(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// Canonical text of a value with object keys sorted: equal JSON gives equal keys.
pub(crate) fn canonical(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                if let Some(x) = m.get(k) {
                    write_canonical(x, out);
                }
            }
            out.push('}');
        }
        Value::Number(n) => match n.as_f64() {
            // 1 and 1.0 are the same JS number
            Some(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => out.push_str(&(f as i64).to_string()),
            _ => out.push_str(&n.to_string()),
        },
        other => out.push_str(&other.to_string()),
    }
}

/// Total order for sorting mixed JSON: null < bool < number < string < array < object.
pub(crate) fn cmp_values(a: &Value, b: &Value) -> Ordering {
    fn rank(v: &Value) -> u8 {
        match v {
            Value::Null => 0,
            Value::Bool(_) => 1,
            Value::Number(_) => 2,
            Value::String(_) => 3,
            Value::Array(_) => 4,
            Value::Object(_) => 5,
        }
    }
    match (a, b) {
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Number(x), Value::Number(y)) => x
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&y.as_f64().unwrap_or(0.0)),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        _ if rank(a) != rank(b) => rank(a).cmp(&rank(b)),
        _ => canonical(a).cmp(&canonical(b)),
    }
}

// ------------------------------------------------------------- field paths

/// Read `a.b.c` (array indexes allowed: `a.0.b`); with `dot == false` the whole string is
/// one literal key.
pub(crate) fn get_path<'a>(m: &'a Map<String, Value>, path: &str, dot: bool) -> Option<&'a Value> {
    if !dot || !path.contains('.') {
        return m.get(path);
    }
    // a literal dotted key wins over nesting, like n8n's lookup
    if let Some(v) = m.get(path) {
        return Some(v);
    }
    let mut parts = path.split('.');
    let mut cur = m.get(parts.next()?)?;
    for p in parts {
        cur = match cur {
            Value::Object(o) => o.get(p)?,
            Value::Array(a) => a.get(p.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// Write `a.b.c = v`, creating (or replacing non-object) intermediates.
pub(crate) fn set_path(m: &mut Map<String, Value>, path: &str, v: Value, dot: bool) {
    if !dot || !path.contains('.') {
        m.insert(path.to_owned(), v);
        return;
    }
    let mut parts: Vec<&str> = path.split('.').collect();
    let Some(last) = parts.pop() else { return };
    let mut cur = m;
    for p in parts {
        let slot = cur
            .entry(p.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if !slot.is_object() {
            *slot = Value::Object(Map::new());
        }
        cur = match slot {
            Value::Object(o) => o,
            _ => return,
        };
    }
    cur.insert(last.to_owned(), v);
}

pub(crate) fn remove_path(m: &mut Map<String, Value>, path: &str, dot: bool) {
    if !dot || !path.contains('.') || m.contains_key(path) {
        m.shift_remove(path);
        return;
    }
    let mut parts: Vec<&str> = path.split('.').collect();
    let Some(last) = parts.pop() else { return };
    let mut cur = m;
    for p in parts {
        cur = match cur.get_mut(p) {
            Some(Value::Object(o)) => o,
            _ => return,
        };
    }
    cur.shift_remove(last);
}

/// Recursive object merge; `b` wins on scalar/array clashes.
pub(crate) fn deep_merge(a: &mut Map<String, Value>, b: &Map<String, Value>) {
    for (k, bv) in b {
        match (a.get_mut(k), bv) {
            (Some(Value::Object(ao)), Value::Object(bo)) => deep_merge(ao, bo),
            _ => {
                a.insert(k.clone(), bv.clone());
            }
        }
    }
}
