//! Date & Time v2: add/subtract, format, current date, difference, extract, round.
//!
//! Output ISO strings follow Luxon's `toISO()`; formats are Luxon tokens (English locale).

use async_trait::async_trait;
use chrono::{Datelike, Timelike};
use chrono_tz::Tz;
use serde_json::{Map, Value, json};

use super::date::*;
use super::util::*;
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::item::Item;
use crate::node::{FlowNode, NodeContext, NodeOutput};

#[derive(Debug)]
pub struct DateTimeNode(NodeTypeDescription);

impl Default for DateTimeNode {
    fn default() -> Self {
        Self::new()
    }
}

impl DateTimeNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Date & Time",
            "name": "n8n-nodes-base.dateTime",
            "version": [2],
            "description": "Allows you to manipulate date and time values",
            "group": ["transform"],
            "defaults": {"name": "Date & Time", "color": "#33cc33"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Operation", "name": "operation", "type": "options", "default": "getCurrentDate",
                 "options": [
                    {"name": "Add to a Date", "value": "addToDate"},
                    {"name": "Extract Part of a Date", "value": "extractDate"},
                    {"name": "Format a Date", "value": "formatDate"},
                    {"name": "Get Current Date", "value": "getCurrentDate"},
                    {"name": "Get Time Between Dates", "value": "getTimeBetweenDates"},
                    {"name": "Round a Date", "value": "roundDate"},
                    {"name": "Subtract From a Date", "value": "subtractFromDate"}]},
                {"displayName": "Include Current Time", "name": "includeTime", "type": "boolean", "default": true,
                 "displayOptions": {"show": {"operation": ["getCurrentDate"]}}},
                {"displayName": "Date", "name": "date", "type": "string", "default": "",
                 "displayOptions": {"hide": {"operation": ["getCurrentDate", "getTimeBetweenDates"]}}},
                {"displayName": "Duration", "name": "magnitude", "type": "number", "default": 0,
                 "displayOptions": {"show": {"operation": ["addToDate", "subtractFromDate"]}}},
                {"displayName": "Time Unit", "name": "timeUnit", "type": "options", "default": "days",
                 "displayOptions": {"show": {"operation": ["addToDate", "subtractFromDate"]}}},
                {"displayName": "Format", "name": "format", "type": "options", "default": "yyyy-MM-dd",
                 "displayOptions": {"show": {"operation": ["formatDate"]}}},
                {"displayName": "Custom Format", "name": "customFormat", "type": "string", "default": "",
                 "displayOptions": {"show": {"operation": ["formatDate"], "format": ["custom"]}}},
                {"displayName": "Start Date", "name": "startDate", "type": "string", "default": "",
                 "displayOptions": {"show": {"operation": ["getTimeBetweenDates"]}}},
                {"displayName": "End Date", "name": "endDate", "type": "string", "default": "",
                 "displayOptions": {"show": {"operation": ["getTimeBetweenDates"]}}},
                {"displayName": "Units", "name": "units", "type": "multiOptions", "default": ["day"],
                 "displayOptions": {"show": {"operation": ["getTimeBetweenDates"]}}},
                {"displayName": "Part", "name": "extract", "type": "options", "default": "day",
                 "displayOptions": {"show": {"operation": ["extractDate"]}}},
                {"displayName": "Mode", "name": "mode", "type": "options", "default": "roundDown",
                 "displayOptions": {"show": {"operation": ["roundDate"]}}},
                {"displayName": "To Nearest", "name": "toNearest", "type": "options", "default": "day",
                 "displayOptions": {"show": {"operation": ["roundDate"], "mode": ["roundDown"]}}},
                {"displayName": "To", "name": "to", "type": "options", "default": "month",
                 "displayOptions": {"show": {"operation": ["roundDate"], "mode": ["roundUp"]}}},
                {"displayName": "Output Field Name", "name": "outputFieldName", "type": "string", "default": ""},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add option",
                 "options": [
                    {"displayName": "Include Input Fields", "name": "includeInputFields", "type": "boolean", "default": false},
                    {"displayName": "Timezone", "name": "timezone", "type": "string", "default": ""},
                    {"displayName": "ISO String Output", "name": "isoString", "type": "boolean", "default": false}]}
            ]
        })))
    }
}

fn parse_in(ctx: &NodeContext, v: &Value, tz: Tz, what: &str, i: usize) -> Result<chrono::DateTime<Tz>, FlowError> {
    parse_date(v, tz).ok_or_else(|| {
        node_err(
            ctx,
            format!("Invalid date: '{}' for {what} [item {i}]", to_display(v)),
        )
    })
}

fn iso_duration(parts: &[(&str, f64)]) -> String {
    let mut date = String::new();
    let mut time = String::new();
    for (u, n) in parts {
        if *n == 0.0 {
            continue;
        }
        let t = match *u {
            "years" => ('Y', false),
            "months" => ('M', false),
            "weeks" => ('W', false),
            "days" => ('D', false),
            "hours" => ('H', true),
            "minutes" => ('M', true),
            "seconds" | "milliseconds" => ('S', true),
            _ => continue,
        };
        let val = if *u == "milliseconds" { n / 1000.0 } else { *n };
        let s = format!("{}{}", num(val).to_string().trim_matches('"'), t.0);
        if t.1 { time.push_str(&s) } else { date.push_str(&s) }
    }
    if date.is_empty() && time.is_empty() {
        return "PT0S".into();
    }
    format!("P{date}{}", if time.is_empty() { String::new() } else { format!("T{time}") })
}

/// Largest `k >= 0` with `start + k units <= end` (calendar units).
fn whole_units(start: chrono::DateTime<Tz>, end: chrono::DateTime<Tz>, unit: &str) -> i64 {
    let months_per = match unit {
        "years" => 12,
        "quarters" => 3,
        _ => 1,
    };
    let approx = (i64::from(end.year()) - i64::from(start.year())) * 12
        + i64::from(end.month0())
        - i64::from(start.month0());
    let mut k = approx / months_per;
    while k > 0 && add_unit(start, unit, k as f64).is_none_or(|d| d > end) {
        k -= 1;
    }
    while add_unit(start, unit, (k + 1) as f64).is_some_and(|d| d <= end) {
        k += 1;
    }
    k.max(0)
}

fn time_between(
    a: chrono::DateTime<Tz>,
    b: chrono::DateTime<Tz>,
    units: &[String],
) -> Vec<(&'static str, f64)> {
    const ORDER: [(&str, &str); 8] = [
        ("year", "years"),
        ("month", "months"),
        ("week", "weeks"),
        ("day", "days"),
        ("hour", "hours"),
        ("minute", "minutes"),
        ("second", "seconds"),
        ("millisecond", "milliseconds"),
    ];
    const MS: [(&str, f64); 6] = [
        ("weeks", 604_800_000.0),
        ("days", 86_400_000.0),
        ("hours", 3_600_000.0),
        ("minutes", 60_000.0),
        ("seconds", 1000.0),
        ("milliseconds", 1.0),
    ];
    let sign = if b < a { -1.0 } else { 1.0 };
    let (s, e) = if b < a { (b, a) } else { (a, b) };
    let wanted: Vec<&str> = ORDER
        .iter()
        .filter(|(sing, plur)| units.iter().any(|u| u == sing || u == plur))
        .map(|(_, plur)| *plur)
        .collect();
    let mut out = Vec::new();
    let mut cur = s;
    for (idx, unit) in wanted.iter().enumerate() {
        if *unit == "years" || *unit == "months" {
            let k = whole_units(cur, e, unit);
            cur = add_unit(cur, unit, k as f64).unwrap_or(cur);
            out.push((*unit, sign * k as f64));
        } else {
            let rem = (e - cur).num_milliseconds() as f64;
            let per = MS.iter().find(|(u, _)| u == unit).map_or(1.0, |m| m.1);
            let last = idx == wanted.len() - 1;
            let n = if last { rem / per } else { (rem / per).floor() };
            cur += chrono::Duration::milliseconds((n.floor() * per) as i64);
            out.push((*unit, sign * n));
        }
    }
    out
}

#[async_trait]
impl FlowNode for DateTimeNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let input = port(ctx, 0);
        // getCurrentDate works without input items; n8n runs it once per input item
        let mut out = Vec::with_capacity(input.len());
        for (i, item) in input.iter().enumerate() {
            let op = param_str(ctx, "operation", i, "getCurrentDate")?;
            let tz_name = match param_str(ctx, "options.timezone", i, "")? {
                s if s.is_empty() => ctx.expr.timezone.clone().unwrap_or_else(|| "UTC".into()),
                s => s,
            };
            let tz = parse_zone(&tz_name);
            let include_input = param_bool(ctx, "options.includeInputFields", i, false)?;
            let mut json = if include_input { item.json.clone() } else { Map::new() };
            let field = |default: &str| -> Result<String, FlowError> {
                param_str(ctx, "outputFieldName", i, default)
                    .map(|s| if s.is_empty() { default.to_owned() } else { s })
            };

            match op.as_str() {
                "getCurrentDate" => {
                    let now = ctx.expr.now_ms.unwrap_or_else(now_millis);
                    let mut d = from_millis(now, tz).ok_or_else(|| node_err(ctx, "time out of range"))?;
                    if !param_bool(ctx, "includeTime", i, true)? {
                        d = start_of(d, "day").unwrap_or(d);
                    }
                    set_path(&mut json, &field("currentDate")?, Value::String(to_iso(&d)), false);
                }
                "addToDate" | "subtractFromDate" => {
                    let v = param_or(ctx, "date", i, Value::Null)?;
                    let d = parse_in(ctx, &v, tz, "'Date'", i)?;
                    let mut amount = param_f64(ctx, "magnitude", i, 0.0)?;
                    if op == "subtractFromDate" {
                        amount = -amount;
                    }
                    let unit = param_str(ctx, "timeUnit", i, "days")?;
                    let r = add_unit(d, &unit, amount)
                        .ok_or_else(|| node_err(ctx, format!("Unknown time unit `{unit}`")))?;
                    set_path(&mut json, &field("newDate")?, Value::String(to_iso(&r)), false);
                }
                "formatDate" => {
                    let v = param_or(ctx, "date", i, Value::Null)?;
                    let d = parse_in(ctx, &v, tz, "'Date'", i)?;
                    let mut fmt = param_str(ctx, "format", i, "yyyy-MM-dd")?;
                    if fmt == "custom" {
                        fmt = param_str(ctx, "customFormat", i, "yyyy-MM-dd")?;
                    }
                    set_path(
                        &mut json,
                        &field("formattedDate")?,
                        Value::String(format_luxon(&d, &fmt)),
                        false,
                    );
                }
                "getTimeBetweenDates" => {
                    let sv = param_or(ctx, "startDate", i, Value::Null)?;
                    let ev = param_or(ctx, "endDate", i, Value::Null)?;
                    let s = parse_in(ctx, &sv, tz, "'Start Date'", i)?;
                    let e = parse_in(ctx, &ev, tz, "'End Date'", i)?;
                    let units: Vec<String> = match param(ctx, "units", i)? {
                        Some(Value::Array(a)) => a.iter().map(to_display).collect(),
                        Some(Value::String(s)) => split_list(&s),
                        _ => vec!["day".into()],
                    };
                    let parts = time_between(s, e, &units);
                    let v = if param_bool(ctx, "options.isoString", i, false)? {
                        Value::String(iso_duration(&parts))
                    } else {
                        Value::Object(parts.iter().map(|(u, n)| ((*u).to_owned(), num(*n))).collect())
                    };
                    set_path(&mut json, &field("timeDifference")?, v, false);
                }
                "extractDate" => {
                    let v = param_or(ctx, "date", i, Value::Null)?;
                    let d = parse_in(ctx, &v, tz, "'Date'", i)?;
                    let n = match param_str(ctx, "extract", i, "day")?.as_str() {
                        "year" => i64::from(d.year()),
                        "month" => i64::from(d.month()),
                        "week" => i64::from(iso_week(&d)),
                        "day" => i64::from(d.day()),
                        "hour" => i64::from(d.hour()),
                        "minute" => i64::from(d.minute()),
                        "second" => i64::from(d.second()),
                        other => return Err(node_err(ctx, format!("Unknown date part `{other}`"))),
                    };
                    set_path(&mut json, &field("datePart")?, Value::from(n), false);
                }
                "roundDate" => {
                    let v = param_or(ctx, "date", i, Value::Null)?;
                    let d = parse_in(ctx, &v, tz, "'Date'", i)?;
                    let r = if param_str(ctx, "mode", i, "roundDown")? == "roundUp" {
                        end_of(d, &param_str(ctx, "to", i, "month")?)
                    } else {
                        start_of(d, &param_str(ctx, "toNearest", i, "day")?)
                    }
                    .ok_or_else(|| node_err(ctx, "Unknown rounding unit"))?;
                    set_path(&mut json, &field("roundedDate")?, Value::String(to_iso(&r)), false);
                }
                other => return Err(node_err(ctx, format!("Unknown operation `{other}`"))),
            }
            let mut o = Item::new(json);
            o.paired_item = paired(i);
            out.push(o);
        }
        Ok(vec![out])
    }
}
