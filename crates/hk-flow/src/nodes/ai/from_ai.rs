//! `$fromAI('key', 'description', 'type', default)`: values the model fills in when it calls
//! a tool. The expression engine does not know the helper, so tool nodes scan their
//! parameters for it, derive the tool's argument schema, and rewrite each call to a
//! lookup in `$vars.__fromAI` before evaluating.

use serde_json::{Map, Value, json};

/// Scope variable the rewritten calls read.
pub(crate) const FROM_AI_VAR: &str = "__fromAI";

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FromAiCall {
    pub key: String,
    pub description: String,
    pub ty: String,
    pub default: Option<Value>,
}

/// One call found in `src`: byte span of `$fromAI(...)` and its parsed arguments.
fn find_calls(src: &str) -> Vec<(usize, usize, FromAiCall)> {
    let mut out = Vec::new();
    let b = src.as_bytes();
    let mut from = 0;
    while let Some(rel) = src[from..].find("$fromAI") {
        let start = from + rel;
        let mut i = start + "$fromAI".len();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b.get(i) != Some(&b'(') {
            from = start + 1;
            continue;
        }
        let args_start = i + 1;
        let mut depth = 1usize;
        let mut j = args_start;
        while j < b.len() && depth > 0 {
            match b[j] {
                q @ (b'\'' | b'"' | b'`') => {
                    j += 1;
                    while j < b.len() && b[j] != q {
                        if b[j] == b'\\' {
                            j += 1;
                        }
                        j += 1;
                    }
                }
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            j += 1;
        }
        if depth != 0 {
            break;
        }
        let end = j; // one past ')'
        let args = split_args(&src[args_start..end - 1]);
        if let Some(call) = parse_call(&args) {
            out.push((start, end, call));
        }
        from = end;
    }
    out
}

fn split_args(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let (mut out, mut depth, mut last, mut i) = (Vec::new(), 0i32, 0, 0);
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"' | b'`') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push(s[last..i].trim().to_owned());
                last = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let tail = s[last..].trim();
    if !tail.is_empty() {
        out.push(tail.to_owned());
    }
    out
}

fn literal(s: &str) -> Value {
    let t = s.trim();
    let q = t.chars().next();
    if matches!(q, Some('\'' | '"' | '`')) && t.len() >= 2 && t.ends_with(q.unwrap_or('"')) {
        let inner = &t[1..t.len() - 1];
        return Value::String(
            inner
                .replace("\\'", "'")
                .replace("\\\"", "\"")
                .replace("\\n", "\n"),
        );
    }
    serde_json::from_str(t).unwrap_or_else(|_| Value::String(t.to_owned()))
}

fn parse_call(args: &[String]) -> Option<FromAiCall> {
    let key = match literal(args.first()?) {
        Value::String(s) if !s.is_empty() => s,
        _ => return None,
    };
    let text = |i: usize| match args.get(i).map(|a| literal(a)) {
        Some(Value::String(s)) => s,
        _ => String::new(),
    };
    let ty = match text(2) {
        t if t.is_empty() => "string".to_owned(),
        t => t,
    };
    Some(FromAiCall {
        key,
        description: text(1),
        ty,
        default: args.get(3).map(|a| literal(a)),
    })
}

/// Every `$fromAI` call in the string leaves of `v`, first occurrence per key.
pub(crate) fn scan(v: &Value) -> Vec<FromAiCall> {
    fn walk(v: &Value, out: &mut Vec<FromAiCall>) {
        match v {
            Value::String(s) if s.contains("$fromAI") => {
                for (_, _, c) in find_calls(s) {
                    if !out.iter().any(|o| o.key == c.key) {
                        out.push(c);
                    }
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::Object(m) => m.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(v, &mut out);
    out
}

/// JSON Schema for the arguments of the calls found.
pub(crate) fn schema_of(calls: &[FromAiCall]) -> Value {
    let mut props = Map::new();
    let mut required = Vec::new();
    for c in calls {
        let ty = match c.ty.as_str() {
            "number" | "boolean" | "string" => c.ty.as_str(),
            "json" => "object",
            _ => "string",
        };
        props.insert(
            c.key.clone(),
            json!({"type": ty, "description": c.description}),
        );
        if c.default.is_none() {
            required.push(Value::String(c.key.clone()));
        }
    }
    json!({"type": "object", "properties": props, "required": required})
}

/// Same walk as [`scan`] but rewrites each call into `$vars.__fromAI["key"]` (or the
/// default when the model omitted the key).
pub(crate) fn rewrite(v: &Value) -> Value {
    match v {
        Value::String(s) if s.contains("$fromAI") => {
            let mut out = String::new();
            let mut last = 0;
            for (a, b, c) in find_calls(s) {
                out.push_str(&s[last..a]);
                let key = Value::String(c.key).to_string();
                match c.default {
                    Some(d) => out.push_str(&format!("($vars.{FROM_AI_VAR}[{key}] ?? {d})")),
                    None => out.push_str(&format!("$vars.{FROM_AI_VAR}[{key}]")),
                }
                last = b;
            }
            out.push_str(&s[last..]);
            Value::String(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(rewrite).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), rewrite(x))).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_and_rewrites() {
        let v = json!({"a": "={{ $fromAI('orderId', 'the order (number)', 'number') }}",
                       "b": "={{ $fromAI(\"note\", \"\", \"string\", \"none\") + 1 }}", "c": 5});
        let calls = scan(&v);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].key, "orderId");
        assert_eq!(calls[0].description, "the order (number)");
        assert_eq!(calls[0].ty, "number");
        assert_eq!(calls[1].default, Some(json!("none")));
        let s = schema_of(&calls);
        assert_eq!(s["required"], json!(["orderId"]));
        let r = rewrite(&v);
        assert_eq!(r["a"], "={{ $vars.__fromAI[\"orderId\"] }}");
        assert_eq!(r["b"], "={{ ($vars.__fromAI[\"note\"] ?? \"none\") + 1 }}");
        assert_eq!(r["c"], 5);
    }
}
