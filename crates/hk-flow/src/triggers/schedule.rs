//! Schedule Trigger: n8n interval rules -> cron -> next fire time.
//!
//! Every rule is lowered to a 6-field cron (`sec min hour dom mon dow`) the way the n8n
//! node does (docs: Schedule Trigger), evaluated in the workflow timezone with `croner`:
//! fixed-time rules (`0 30 2 * * *`) fire once after a DST gap and once on an overlap,
//! wildcard/step rules skip a gap and fire in both passes of an overlap. "Every N weeks"
//! counts ISO weeks from the activation week. Missed fires are not replayed.

use chrono::{DateTime, Datelike};
use chrono_tz::Tz;
use croner::Cron;
use croner::parser::{CronParser, Seconds};
use serde_json::{Map, Value};

use crate::error::FlowError;
use crate::item::Item;
use crate::nodes::date::{from_millis, parse_zone};
use crate::workflow::Node;

pub const SCHEDULE_TYPE: &str = "n8n-nodes-base.scheduleTrigger";

/// One lowered rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleRule {
    /// 6-field cron, or the user's 5/6-field expression for `cronExpression` rules.
    pub cron: String,
    /// Fire only every N-th ISO week (`weeks` rules with N > 1).
    pub every_weeks: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScheduleSpec {
    pub rules: Vec<ScheduleRule>,
}

fn int(v: &Map<String, Value>, k: &str, default: i64) -> i64 {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(default),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
        _ => default,
    }
}

fn ranged(
    node: &str,
    v: &Map<String, Value>,
    k: &str,
    default: i64,
    min: i64,
    max: i64,
) -> Result<i64, FlowError> {
    let n = int(v, k, default);
    if (min..=max).contains(&n) {
        Ok(n)
    } else {
        Err(FlowError::Invalid(format!(
            "schedule `{node}`: `{k}` must be {min}..={max}, got {n}"
        )))
    }
}

impl ScheduleSpec {
    /// Read `parameters.rule.interval[]`. No rules = n8n default of one rule firing
    /// every day at midnight... which n8n shows as "Days"; here an empty list is an error
    /// (activating a trigger that never fires is a mistake worth surfacing).
    pub fn from_node(node: &Node) -> Result<Self, FlowError> {
        let intervals = node
            .parameters
            .get("rule")
            .and_then(|r| r.get("interval"))
            .and_then(Value::as_array)
            .ok_or_else(|| {
                FlowError::Invalid(format!("schedule `{}` has no rule.interval", node.name))
            })?;
        let mut rules = Vec::new();
        for i in intervals {
            let Some(v) = i.as_object() else { continue };
            rules.push(lower(&node.name, v)?);
        }
        if rules.is_empty() {
            return Err(FlowError::Invalid(format!(
                "schedule `{}` has an empty rule list",
                node.name
            )));
        }
        let spec = Self { rules };
        spec.validate()?;
        Ok(spec)
    }

    fn validate(&self) -> Result<(), FlowError> {
        for r in &self.rules {
            parse_cron(&r.cron)?;
        }
        Ok(())
    }

    /// Earliest fire strictly after `after_ms` over all rules. `anchor_ms` (activation
    /// time) fixes which weeks count for "every N weeks".
    pub fn next_fire(&self, after_ms: i64, tz: Tz, anchor_ms: i64) -> Option<i64> {
        self.rules
            .iter()
            .filter_map(|r| r.next_fire(after_ms, tz, anchor_ms))
            .min()
    }
}

fn lower(node: &str, v: &Map<String, Value>) -> Result<ScheduleRule, FlowError> {
    let field = v.get("field").and_then(Value::as_str).unwrap_or("days");
    let minute = |v| ranged(node, v, "triggerAtMinute", 0, 0, 59);
    let hour = |v| ranged(node, v, "triggerAtHour", 0, 0, 23);
    let (cron, every_weeks) = match field {
        "seconds" => (
            format!(
                "*/{} * * * * *",
                ranged(node, v, "secondsInterval", 30, 1, 59)?
            ),
            None,
        ),
        "minutes" => (
            format!(
                "0 */{} * * * *",
                ranged(node, v, "minutesInterval", 5, 1, 59)?
            ),
            None,
        ),
        "hours" => (
            format!(
                "0 {} */{} * * *",
                minute(v)?,
                ranged(node, v, "hoursInterval", 1, 1, 23)?
            ),
            None,
        ),
        "days" => (
            format!(
                "0 {} {} */{} * *",
                minute(v)?,
                hour(v)?,
                ranged(node, v, "daysInterval", 1, 1, 31)?
            ),
            None,
        ),
        "weeks" => {
            let days: Vec<String> = match v.get("triggerAtDay") {
                Some(Value::Array(a)) if !a.is_empty() => a
                    .iter()
                    .filter_map(|d| d.as_i64().or_else(|| d.as_str()?.parse().ok()))
                    .filter(|d| (0..=6).contains(d))
                    .map(|d| d.to_string())
                    .collect(),
                _ => vec!["0".into()],
            };
            if days.is_empty() {
                return Err(FlowError::Invalid(format!(
                    "schedule `{node}`: triggerAtDay needs values 0..=6 (0 = Sunday)"
                )));
            }
            let n = ranged(node, v, "weeksInterval", 1, 1, 52)?;
            (
                format!("0 {} {} * * {}", minute(v)?, hour(v)?, days.join(",")),
                (n > 1).then_some(n as u32),
            )
        }
        "months" => (
            format!(
                "0 {} {} {} */{} *",
                minute(v)?,
                hour(v)?,
                ranged(node, v, "triggerAtDayOfMonth", 1, 1, 31)?,
                ranged(node, v, "monthsInterval", 1, 1, 12)?
            ),
            None,
        ),
        "cronExpression" => {
            let e = v
                .get("expression")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if e.is_empty() {
                return Err(FlowError::Invalid(format!(
                    "schedule `{node}`: empty cron expression"
                )));
            }
            (e.to_owned(), None)
        }
        other => {
            return Err(FlowError::Invalid(format!(
                "schedule `{node}`: unknown interval field `{other}`"
            )));
        }
    };
    Ok(ScheduleRule { cron, every_weeks })
}

fn parse_cron(expr: &str) -> Result<Cron, FlowError> {
    CronParser::builder()
        .seconds(Seconds::Optional)
        .build()
        .parse(expr)
        .map_err(|e| FlowError::Invalid(format!("bad cron expression `{expr}`: {e}")))
}

/// ISO-week index counted from a fixed Monday; stable across years.
fn week_index(d: &DateTime<Tz>) -> i64 {
    let days = d.date_naive().num_days_from_ce();
    // 0001-01-01 is a Monday in the proleptic Gregorian calendar
    i64::from(days - 1).div_euclid(7)
}

impl ScheduleRule {
    pub fn next_fire(&self, after_ms: i64, tz: Tz, anchor_ms: i64) -> Option<i64> {
        let cron = parse_cron(&self.cron).ok()?;
        let anchor_week = week_index(&from_millis(anchor_ms, tz)?);
        let mut cursor = from_millis(after_ms, tz)?;
        // a few hundred hops covers "every 52 weeks" with several days per week
        for _ in 0..4000 {
            let next = cron.find_next_occurrence(&cursor, false).ok()?;
            match self.every_weeks {
                Some(n) if (week_index(&next) - anchor_week).rem_euclid(i64::from(n)) != 0 => {
                    cursor = next;
                }
                _ => return Some(next.timestamp_millis()),
            }
        }
        None
    }
}

// ------------------------------------------------------------------- item

fn ordinal(n: u32) -> String {
    let suffix = match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// The item the trigger emits for a fire at `fire_ms` (n8n docs output shape).
pub fn schedule_item(fire_ms: i64, tz: Tz) -> Option<Item> {
    let d = from_millis(fire_ms, tz)?;
    let offset = d.format("%:z").to_string();
    let time = d.format("%-I:%M:%S %P").to_string();
    let mut m = Map::new();
    let mut put = |k: &str, v: String| {
        m.insert(k.to_owned(), Value::String(v));
    };
    put(
        "timestamp",
        d.format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string(),
    );
    put(
        "Readable date",
        format!(
            "{} {} {}, {time}",
            d.format("%B"),
            ordinal(d.day()),
            d.format("%Y")
        ),
    );
    put("Readable time", time);
    put("Day of week", d.format("%A").to_string());
    put("Year", d.format("%Y").to_string());
    put("Month", d.format("%B").to_string());
    put("Day of month", d.format("%d").to_string());
    put("Hour", d.format("%H").to_string());
    put("Minute", d.format("%M").to_string());
    put("Second", d.format("%S").to_string());
    put("Timezone", format!("{} (UTC{offset})", tz.name()));
    Some(Item::new(m))
}

/// Workflow timezone (`settings.timezone`), UTC when unset/unknown.
pub fn workflow_tz(name: Option<&str>) -> Tz {
    parse_zone(name.unwrap_or("UTC"))
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s)
            .unwrap()
            .timestamp_millis()
    }

    fn spec(intervals: Value) -> ScheduleSpec {
        let mut n = Node::new(
            "S",
            SCHEDULE_TYPE,
            serde_json::Number::from_f64(1.2).unwrap(),
            [0.into(), 0.into()],
        );
        n.parameters
            .insert("rule".into(), json!({"interval": intervals}));
        ScheduleSpec::from_node(&n).unwrap()
    }

    #[test]
    fn lowers_n8n_rules_to_cron() {
        let s = spec(json!([
            {"field": "seconds", "secondsInterval": 15},
            {"field": "minutes", "minutesInterval": 7},
            {"field": "hours", "hoursInterval": 2, "triggerAtMinute": 10},
            {"field": "days", "daysInterval": 1, "triggerAtHour": 9, "triggerAtMinute": 30},
            {"field": "weeks", "triggerAtDay": [1, 5], "triggerAtHour": 8},
            {"field": "months", "triggerAtDayOfMonth": 15, "monthsInterval": 3},
            {"field": "cronExpression", "expression": "0 15 * * 1-5"}
        ]));
        let crons: Vec<&str> = s.rules.iter().map(|r| r.cron.as_str()).collect();
        assert_eq!(
            crons,
            [
                "*/15 * * * * *",
                "0 */7 * * * *",
                "0 10 */2 * * *",
                "0 30 9 */1 * *",
                "0 0 8 * * 1,5",
                "0 0 0 15 */3 *",
                "0 15 * * 1-5"
            ]
        );
    }

    #[test]
    fn rejects_bad_rules() {
        let mut n = Node::new("S", SCHEDULE_TYPE, 1.into(), [0.into(), 0.into()]);
        for bad in [
            json!([{"field": "minutes", "minutesInterval": 0}]),
            json!([{"field": "cronExpression", "expression": "not a cron"}]),
            json!([{"field": "fortnights"}]),
            json!([]),
        ] {
            n.parameters.insert("rule".into(), json!({"interval": bad}));
            assert!(ScheduleSpec::from_node(&n).is_err(), "{bad}");
        }
    }

    #[test]
    fn next_fire_utc_and_zone() {
        let daily = spec(json!([{"field": "days", "triggerAtHour": 9, "triggerAtMinute": 30}]));
        let after = utc(2024, 1, 10, 9, 30, 0);
        // strictly after: the instant itself does not fire again
        assert_eq!(
            daily.next_fire(after, Tz::UTC, after),
            Some(utc(2024, 1, 11, 9, 30, 0))
        );
        // 09:30 in Istanbul (UTC+3, no DST) is 06:30 UTC
        let ist: Tz = "Europe/Istanbul".parse().unwrap();
        assert_eq!(
            daily.next_fire(utc(2024, 1, 10, 0, 0, 0), ist, 0),
            Some(utc(2024, 1, 10, 6, 30, 0))
        );
        // 5-field and seconds-first 6-field expressions both parse
        let every_5s = spec(json!([{"field": "seconds", "secondsInterval": 5}]));
        assert_eq!(
            every_5s.next_fire(utc(2024, 1, 1, 0, 0, 3), Tz::UTC, 0),
            Some(utc(2024, 1, 1, 0, 0, 5))
        );
    }

    #[test]
    fn earliest_rule_wins() {
        let s = spec(json!([
            {"field": "days", "triggerAtHour": 12},
            {"field": "cronExpression", "expression": "0 0 10 * * *"}
        ]));
        assert_eq!(
            s.next_fire(utc(2024, 5, 1, 0, 0, 0), Tz::UTC, 0),
            Some(utc(2024, 5, 1, 10, 0, 0))
        );
    }

    #[test]
    fn dst_spring_forward_gap() {
        // New York 2024-03-10: 02:00 -> 03:00 (EST -> EDT). 02:30 does not exist.
        let ny: Tz = "America/New_York".parse().unwrap();
        let s = spec(json!([{"field": "days", "triggerAtHour": 2, "triggerAtMinute": 30}]));
        let day_before = utc(2024, 3, 9, 12, 0, 0);
        // 12:00 UTC is 07:00 EST, so the next 02:30 is on Mar 10
        let day_of = s.next_fire(day_before, ny, 0).unwrap();
        let local = from_millis(day_of, ny).unwrap();
        // fixed-time job: runs once, right after the gap, same calendar day
        assert_eq!(local.format("%Y-%m-%d").to_string(), "2024-03-10");
        assert_eq!(local.format("%H").to_string(), "03");
        // and the following day it is back to 02:30 local
        let next = s.next_fire(day_of, ny, 0).unwrap();
        assert_eq!(
            from_millis(next, ny)
                .unwrap()
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            "2024-03-11 02:30"
        );
    }

    #[test]
    fn dst_fall_back_overlap_fires_once() {
        // New York 2024-11-03: 02:00 EDT -> 01:00 EST, so 01:30 happens twice.
        let ny: Tz = "America/New_York".parse().unwrap();
        let s = spec(json!([{"field": "days", "triggerAtHour": 1, "triggerAtMinute": 30}]));
        let before = utc(2024, 11, 3, 4, 0, 0); // 00:00 EDT
        let first = s.next_fire(before, ny, 0).unwrap();
        assert_eq!(first, utc(2024, 11, 3, 5, 30, 0)); // 01:30 EDT, the first pass
        let second = s.next_fire(first, ny, 0).unwrap();
        // the repeated 01:30 EST (06:30 UTC) is skipped: next is the following day
        assert_eq!(
            from_millis(second, ny)
                .unwrap()
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            "2024-11-04 01:30"
        );
    }

    #[test]
    fn every_n_weeks_counts_from_anchor() {
        // Mondays 08:00, every 2nd week, anchored in the week of 2024-01-01 (a Monday)
        let s = spec(
            json!([{"field": "weeks", "weeksInterval": 2, "triggerAtDay": [1], "triggerAtHour": 8}]),
        );
        let anchor = utc(2024, 1, 1, 0, 0, 0);
        let a = s.next_fire(anchor, Tz::UTC, anchor).unwrap();
        assert_eq!(a, utc(2024, 1, 1, 8, 0, 0));
        let b = s.next_fire(a, Tz::UTC, anchor).unwrap();
        assert_eq!(b, utc(2024, 1, 15, 8, 0, 0));
        let c = s.next_fire(b, Tz::UTC, anchor).unwrap();
        assert_eq!(c, utc(2024, 1, 29, 8, 0, 0));
    }

    #[test]
    fn item_shape() {
        let ny: Tz = "America/New_York".parse().unwrap();
        let it = schedule_item(utc(2023, 5, 28, 9, 30, 0), ny).unwrap();
        assert_eq!(
            Value::Object(it.json),
            json!({
                "timestamp": "2023-05-28T05:30:00.000-04:00",
                "Readable date": "May 28th 2023, 5:30:00 am",
                "Readable time": "5:30:00 am",
                "Day of week": "Sunday",
                "Year": "2023",
                "Month": "May",
                "Day of month": "28",
                "Hour": "05",
                "Minute": "30",
                "Second": "00",
                "Timezone": "America/New_York (UTC-04:00)"
            })
        );
    }
}
