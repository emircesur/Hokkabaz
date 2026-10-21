//! Filter-component conditions shared by If, Switch and Filter.
//!
//! Shape (from the node docs / exported JSON):
//! `{ options: {caseSensitive, leftValue, typeValidation: strict|loose},
//!    conditions: [{leftValue, rightValue, operator: {type, operation, singleValue?}}],
//!    combinator: and|or }`

use chrono_tz::Tz;
use regex::RegexBuilder;
use serde_json::Value;

use super::date::parse_date;
use super::util::{canonical, to_display, type_name};
use crate::error::FlowError;
use crate::node::NodeContext;

/// Node-level switches that tweak how conditions compare (`options.ignoreCase`,
/// `options.looseTypeValidation`).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CondFlags {
    pub ignore_case: bool,
    pub loose: bool,
}

impl CondFlags {
    pub fn from_options(ctx: &NodeContext, i: usize) -> Result<Self, FlowError> {
        Ok(Self {
            ignore_case: super::util::param_bool(ctx, "options.ignoreCase", i, false)?,
            loose: super::util::param_bool(ctx, "options.looseTypeValidation", i, false)?,
        })
    }
}

/// Evaluate a `conditions` object for input item `item` of port 0. `conditions` is the raw
/// parameter value; operands are resolved per item.
pub(crate) fn evaluate(
    ctx: &NodeContext,
    conditions: &Value,
    item: usize,
    flags: CondFlags,
) -> Result<bool, FlowError> {
    let Some(obj) = conditions.as_object() else {
        return Ok(true);
    };
    let opts = obj.get("options");
    let case_sensitive = opts
        .and_then(|o| o.get("caseSensitive"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
        && !flags.ignore_case;
    let loose = flags.loose
        || opts
            .and_then(|o| o.get("typeValidation"))
            .and_then(Value::as_str)
            == Some("loose");
    let or = obj.get("combinator").and_then(Value::as_str) == Some("or");
    let list = obj
        .get("conditions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let tz = zone_of(ctx);

    for (ci, c) in list.iter().enumerate() {
        let left = ctx.eval_param(c.get("leftValue").unwrap_or(&Value::Null), item)?;
        let right = ctx.eval_param(c.get("rightValue").unwrap_or(&Value::Null), item)?;
        let op = c.get("operator");
        let ty = op
            .and_then(|o| o.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("string");
        let name = op
            .and_then(|o| o.get("operation"))
            .and_then(Value::as_str)
            .unwrap_or("equals");
        let res = compare(ty, name, &left, &right, case_sensitive, loose, tz).map_err(|m| {
            FlowError::Node {
                node: ctx.node.name.clone(),
                message: format!("{m} [condition {ci}, item {item}]"),
            }
        })?;
        if or && res {
            return Ok(true);
        }
        if !or && !res {
            return Ok(false);
        }
    }
    // empty list: `and` is vacuously true, `or` is false
    Ok(!or || list.is_empty())
}

fn zone_of(ctx: &NodeContext) -> Tz {
    ctx.expr
        .timezone
        .as_deref()
        .map_or(Tz::UTC, super::date::parse_zone)
}

fn is_missing(v: &Value) -> bool {
    v.is_null()
}

fn is_empty(ty: &str, v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => ty == "array" && a.is_empty(),
        Value::Object(o) => ty == "object" && o.is_empty(),
        _ => false,
    }
}

fn wrong_type(v: &Value, expected: &str) -> String {
    format!(
        "Wrong type: '{}' is {} but was expecting {expected}",
        to_display(v),
        type_name(v)
    )
}

/// One comparison. `Err` = type validation failure (strict mode).
pub(crate) fn compare(
    ty: &str,
    op: &str,
    left: &Value,
    right: &Value,
    case_sensitive: bool,
    loose: bool,
    tz: Tz,
) -> Result<bool, String> {
    match op {
        "exists" => return Ok(!is_missing(left)),
        "notExists" => return Ok(is_missing(left)),
        "empty" => return Ok(is_empty(ty, left)),
        "notEmpty" => return Ok(!is_empty(ty, left)),
        _ => {}
    }
    match ty {
        "string" => string_op(op, left, right, case_sensitive, loose),
        "number" => number_op(op, left, right, loose),
        "boolean" => boolean_op(op, left, right, loose),
        "dateTime" => date_op(op, left, right, tz),
        "array" => array_op(op, left, right),
        "object" => Ok(false),
        // untyped operand: plain JSON equality
        _ => Ok(match op {
            "equals" => canonical(left) == canonical(right),
            "notEquals" => canonical(left) != canonical(right),
            _ => false,
        }),
    }
}

fn as_text(v: &Value, loose: bool) -> Result<Option<String>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        _ if loose => Ok(Some(to_display(v))),
        _ => Err(wrong_type(v, "a string")),
    }
}

fn string_op(op: &str, left: &Value, right: &Value, cs: bool, loose: bool) -> Result<bool, String> {
    let l = as_text(left, loose)?;
    // right operands typed by hand in the UI may be numbers; they are text here
    let r = match right {
        Value::Null => String::new(),
        other => to_display(other),
    };
    let negated = op.starts_with("not");
    let Some(l) = l else {
        // null on the left: positive ops fail, negated ops hold
        return Ok(negated);
    };
    let (l, r) = if cs {
        (l, r)
    } else {
        (l.to_lowercase(), r.to_lowercase())
    };
    Ok(match op {
        "equals" => l == r,
        "notEquals" => l != r,
        "contains" => l.contains(&r),
        "notContains" => !l.contains(&r),
        "startsWith" => l.starts_with(&r),
        "notStartsWith" => !l.starts_with(&r),
        "endsWith" => l.ends_with(&r),
        "notEndsWith" => !l.ends_with(&r),
        "regex" | "notRegex" => {
            let (pat, ci) = split_regex(&to_display(right));
            let re = RegexBuilder::new(&pat)
                .case_insensitive(ci || !cs)
                .build()
                .map_err(|e| format!("Invalid regex: {e}"))?;
            // match on the untouched text, case folding is the regex flag's job
            let hit = re.is_match(&to_display(left));
            if op == "regex" { hit } else { !hit }
        }
        other => return Err(format!("Unknown string operation `{other}`")),
    })
}

/// `/pattern/flags` or bare pattern.
fn split_regex(s: &str) -> (String, bool) {
    if let Some(body) = s.strip_prefix('/')
        && let Some(end) = body.rfind('/')
    {
        let (pat, flags) = body.split_at(end);
        return (pat.to_owned(), flags.contains('i'));
    }
    (s.to_owned(), false)
}

fn as_number(v: &Value, loose: bool, strict_label: bool) -> Result<Option<f64>, String> {
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => Ok(n.as_f64()),
        Value::String(s) if loose || !strict_label => Ok(s.trim().parse().ok()),
        Value::Bool(b) if loose => Ok(Some(f64::from(u8::from(*b)))),
        _ => Err(wrong_type(v, "a number")),
    }
}

fn number_op(op: &str, left: &Value, right: &Value, loose: bool) -> Result<bool, String> {
    let l = as_number(left, loose, true)?;
    // the right operand is typed in the node UI, numeric strings are fine there
    let r = as_number(right, true, false)?;
    let (Some(l), Some(r)) = (l, r) else {
        return Ok(op == "notEquals");
    };
    Ok(match op {
        "equals" => l == r,
        "notEquals" => l != r,
        "gt" => l > r,
        "lt" => l < r,
        "gte" => l >= r,
        "lte" => l <= r,
        other => return Err(format!("Unknown number operation `{other}`")),
    })
}

fn as_bool(v: &Value, loose: bool) -> Result<Option<bool>, String> {
    match v {
        Value::Null => Ok(None),
        Value::Bool(b) => Ok(Some(*b)),
        Value::String(s) if loose || s == "true" || s == "false" => {
            Ok(Some(matches!(s.as_str(), "true" | "1")))
        }
        Value::Number(n) if loose => Ok(Some(n.as_f64().is_some_and(|f| f != 0.0))),
        _ => Err(wrong_type(v, "a boolean")),
    }
}

fn boolean_op(op: &str, left: &Value, right: &Value, loose: bool) -> Result<bool, String> {
    let l = as_bool(left, loose)?;
    Ok(match op {
        "true" => l == Some(true),
        "false" => l == Some(false),
        "equals" => l == as_bool(right, true)?,
        "notEquals" => l != as_bool(right, true)?,
        other => return Err(format!("Unknown boolean operation `{other}`")),
    })
}

fn date_op(op: &str, left: &Value, right: &Value, tz: Tz) -> Result<bool, String> {
    let l = match left {
        Value::Null => return Ok(op == "notEquals"),
        v => parse_date(v, tz).ok_or_else(|| wrong_type(v, "a date"))?,
    };
    let r = match right {
        Value::Null => return Ok(op == "notEquals"),
        v => parse_date(v, tz).ok_or_else(|| wrong_type(v, "a date"))?,
    };
    let (l, r) = (l.timestamp_millis(), r.timestamp_millis());
    Ok(match op {
        "equals" => l == r,
        "notEquals" => l != r,
        "after" => l > r,
        "before" => l < r,
        "afterOrEquals" => l >= r,
        "beforeOrEquals" => l <= r,
        other => return Err(format!("Unknown dateTime operation `{other}`")),
    })
}

fn array_op(op: &str, left: &Value, right: &Value) -> Result<bool, String> {
    let Some(arr) = left.as_array() else {
        return Err(wrong_type(left, "an array"));
    };
    let n = right
        .as_f64()
        .or_else(|| right.as_str().and_then(|s| s.trim().parse().ok()));
    let len = arr.len() as f64;
    Ok(match op {
        "contains" => arr.iter().any(|x| loose_eq(x, right)),
        "notContains" => !arr.iter().any(|x| loose_eq(x, right)),
        "lengthEquals" => n == Some(len),
        "lengthNotEquals" => n != Some(len),
        "lengthGt" => n.is_some_and(|n| len > n),
        "lengthLt" => n.is_some_and(|n| len < n),
        "lengthGte" => n.is_some_and(|n| len >= n),
        "lengthLte" => n.is_some_and(|n| len <= n),
        other => return Err(format!("Unknown array operation `{other}`")),
    })
}

fn loose_eq(a: &Value, b: &Value) -> bool {
    canonical(a) == canonical(b) || to_display(a) == to_display(b)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn cmp(ty: &str, op: &str, l: Value, r: Value) -> Result<bool, String> {
        compare(ty, op, &l, &r, true, false, Tz::UTC)
    }

    #[test]
    fn strict_types_reject_wrong_operand() {
        assert!(cmp("number", "gt", json!("5"), json!(1)).is_err());
        assert_eq!(cmp("number", "gt", json!(5), json!("1")), Ok(true));
        assert_eq!(
            compare("number", "gt", &json!("5"), &json!(1), true, true, Tz::UTC),
            Ok(true)
        );
    }

    #[test]
    fn string_ops_and_case() {
        assert_eq!(
            cmp("string", "endsWith", json!("a@corp.io"), json!("corp.io")),
            Ok(true)
        );
        assert_eq!(
            cmp("string", "notEquals", json!(null), json!("x")),
            Ok(true)
        );
        assert_eq!(
            compare(
                "string",
                "equals",
                &json!("ABC"),
                &json!("abc"),
                false,
                false,
                Tz::UTC
            ),
            Ok(true)
        );
        assert_eq!(
            cmp("string", "regex", json!("abc123"), json!("/^[a-z]+\\d+$/")),
            Ok(true)
        );
    }

    #[test]
    fn existence_and_arrays() {
        assert_eq!(cmp("string", "notEmpty", json!(""), json!(null)), Ok(false));
        assert_eq!(cmp("array", "lengthGt", json!([1, 2]), json!(1)), Ok(true));
        assert_eq!(
            cmp("array", "contains", json!([1, "a"]), json!("a")),
            Ok(true)
        );
        assert_eq!(
            cmp(
                "dateTime",
                "before",
                json!("2024-01-01"),
                json!("2024-06-01T00:00:00Z")
            ),
            Ok(true)
        );
        assert_eq!(cmp("boolean", "true", json!(true), json!(null)), Ok(true));
    }
}
