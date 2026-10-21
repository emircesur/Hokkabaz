//! JSON helpers shared by the parsers, the extractor and the classifier: lenient JSON
//! extraction from model text, schema inference from an example, and a small JSON Schema
//! validator (type / properties / required / items / enum / anyOf).

use serde_json::{Map, Value, json};

/// JSON value inside model text: plain JSON, a fenced block, or the outermost `{..}` /
/// `[..]` span.
pub(crate) fn extract_json(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Some(v);
    }
    // fenced block: ```json ... ```
    if let Some(start) = t.find("```") {
        let rest = &t[start + 3..];
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        if let Some(end) = rest.find("```")
            && let Ok(v) = serde_json::from_str::<Value>(rest[..end].trim())
        {
            return Some(v);
        }
    }
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let (Some(a), Some(b)) = (t.find(open), t.rfind(close))
            && a < b
            && let Ok(v) = serde_json::from_str::<Value>(&t[a..=b])
        {
            return Some(v);
        }
    }
    None
}

/// Schema describing `example` (types only, nothing required: examples show shape, not
/// which fields a model must produce).
pub(crate) fn schema_from_example(example: &Value) -> Value {
    match example {
        Value::Null => json!({"type": "null"}),
        Value::Bool(_) => json!({"type": "boolean"}),
        Value::Number(n) if n.is_i64() || n.is_u64() => json!({"type": "integer"}),
        Value::Number(_) => json!({"type": "number"}),
        Value::String(_) => json!({"type": "string"}),
        Value::Array(a) => match a.first() {
            Some(first) => json!({"type": "array", "items": schema_from_example(first)}),
            None => json!({"type": "array"}),
        },
        Value::Object(m) => {
            let props: Map<String, Value> = m
                .iter()
                .map(|(k, v)| (k.clone(), schema_from_example(v)))
                .collect();
            json!({"type": "object", "properties": props})
        }
    }
}

fn type_ok(ty: &str, v: &Value) -> bool {
    match ty {
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => {
            v.as_i64().is_some()
                || v.as_u64().is_some()
                || v.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        "boolean" => v.is_boolean(),
        "object" => v.is_object(),
        "array" => v.is_array(),
        "null" => v.is_null(),
        _ => true,
    }
}

/// `Err` carries a JSON-pointer-ish path and the reason.
pub(crate) fn validate(schema: &Value, v: &Value) -> Result<(), String> {
    check(schema, v, "$")
}

fn check(schema: &Value, v: &Value, path: &str) -> Result<(), String> {
    let Some(s) = schema.as_object() else {
        return Ok(());
    };
    if let Some(any) = s
        .get("anyOf")
        .or_else(|| s.get("oneOf"))
        .and_then(Value::as_array)
        && !any.iter().any(|sub| check(sub, v, path).is_ok())
    {
        return Err(format!("{path}: matches none of the allowed shapes"));
    }
    match s.get("type") {
        Some(Value::String(t)) if !type_ok(t, v) => {
            return Err(format!("{path}: expected {t}"));
        }
        Some(Value::Array(ts)) if !ts.iter().filter_map(Value::as_str).any(|t| type_ok(t, v)) => {
            return Err(format!("{path}: unexpected type"));
        }
        _ => {}
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array)
        && !e.contains(v)
    {
        return Err(format!("{path}: not one of the allowed values"));
    }
    if let Value::Object(obj) = v {
        if let Some(req) = s.get("required").and_then(Value::as_array) {
            for k in req.iter().filter_map(Value::as_str) {
                if !obj.contains_key(k) {
                    return Err(format!("{path}: missing required property `{k}`"));
                }
            }
        }
        let props = s.get("properties").and_then(Value::as_object);
        if let Some(props) = props {
            for (k, sub) in props {
                if let Some(x) = obj.get(k) {
                    check(sub, x, &format!("{path}.{k}"))?;
                }
            }
        }
        if s.get("additionalProperties") == Some(&Value::Bool(false))
            && let Some(k) = obj
                .keys()
                .find(|k| props.is_none_or(|p| !p.contains_key(*k)))
        {
            return Err(format!("{path}: unexpected property `{k}`"));
        }
    }
    if let (Value::Array(items), Some(sub)) = (v, s.get("items")) {
        for (i, x) in items.iter().enumerate() {
            check(sub, x, &format!("{path}[{i}]"))?;
        }
    }
    Ok(())
}

/// Prompt block telling a model to answer with JSON matching `schema`.
pub(crate) fn json_instructions(schema: &Value) -> String {
    format!(
        "Respond with a single JSON value that conforms to the JSON Schema below. \
         Output only that JSON, wrapped in a ```json markdown code block, with no other text.\n\
         ```json\n{}\n```",
        serde_json::to_string_pretty(schema).unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_from_fence_and_prose() {
        assert_eq!(
            extract_json("```json\n{\"a\":1}\n```"),
            Some(json!({"a": 1}))
        );
        assert_eq!(
            extract_json("Sure! {\"a\": [1,2]} done"),
            Some(json!({"a": [1, 2]}))
        );
        assert_eq!(extract_json("nothing"), None);
    }

    #[test]
    fn validates_shapes() {
        let s = json!({"type":"object","properties":{"n":{"type":"integer"},"t":{"type":"array","items":{"type":"string"}}},"required":["n"]});
        assert!(validate(&s, &json!({"n": 2, "t": ["a"]})).is_ok());
        assert!(
            validate(&s, &json!({"t": []}))
                .unwrap_err()
                .contains("required")
        );
        assert!(
            validate(&s, &json!({"n": "x"}))
                .unwrap_err()
                .contains("$.n")
        );
        assert!(
            validate(&s, &json!({"n": 1, "t": [1]}))
                .unwrap_err()
                .contains("$.t[0]")
        );
    }

    #[test]
    fn example_schema() {
        let s = schema_from_example(&json!({"a": 1, "b": ["x"], "c": 1.5}));
        assert_eq!(s["properties"]["a"]["type"], "integer");
        assert_eq!(s["properties"]["b"]["items"]["type"], "string");
        assert_eq!(s["properties"]["c"]["type"], "number");
    }
}
