//! Date helpers: zone parsing, tolerant date parsing, Luxon-style `toFormat`.

use chrono::{
    DateTime, Datelike, Duration, FixedOffset, LocalResult, NaiveDate, NaiveDateTime, NaiveTime,
    Offset, TimeZone, Timelike,
};
use chrono_tz::Tz;
use serde_json::Value;

/// IANA name, `UTC`, or fixed `UTC+2` / `UTC-03:30`. Unknown zones fall back to UTC.
pub(crate) fn parse_zone(name: &str) -> Tz {
    let name = name.trim();
    if let Ok(tz) = name.parse::<Tz>() {
        return tz;
    }
    if let Some(rest) = name
        .strip_prefix("UTC")
        .or_else(|| name.strip_prefix("GMT"))
        && let Some(sign) = rest.chars().next()
        && (sign == '+' || sign == '-')
    {
        let hours: Option<i32> = rest[1..].split(':').next().and_then(|h| h.parse().ok());
        if let Some(h) = hours {
            // POSIX `Etc/GMT` zones have the sign inverted
            let etc = if sign == '+' {
                format!("Etc/GMT-{h}")
            } else {
                format!("Etc/GMT+{h}")
            };
            if let Ok(tz) = etc.parse::<Tz>() {
                return tz;
            }
        }
    }
    Tz::UTC
}

pub(crate) fn from_millis(ms: i64, tz: Tz) -> Option<DateTime<Tz>> {
    DateTime::from_timestamp_millis(ms).map(|d| d.with_timezone(&tz))
}

pub(crate) fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn local_to_dt(naive: NaiveDateTime, tz: Tz) -> Option<DateTime<Tz>> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(d) => Some(d),
        LocalResult::Ambiguous(d, _) => Some(d),
        LocalResult::None => None,
    }
}

/// Dates n8n accepts: ISO 8601 (with/without offset or time), `YYYY-MM-DD HH:MM:SS`, unix
/// ms (numbers), unix seconds for 10-digit numbers is NOT guessed. Zone-less strings are
/// read in `tz`.
pub(crate) fn parse_date(v: &Value, tz: Tz) -> Option<DateTime<Tz>> {
    match v {
        Value::Number(n) => from_millis(n.as_f64()? as i64, tz),
        Value::String(s) => parse_date_str(s.trim(), tz),
        // `$now` / DateTime objects arrive as ISO strings after JSON conversion
        _ => None,
    }
}

pub(crate) fn parse_date_str(s: &str, tz: Tz) -> Option<DateTime<Tz>> {
    if s.is_empty() {
        return None;
    }
    if let Ok(d) = DateTime::<FixedOffset>::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&tz));
    }
    if let Ok(d) = DateTime::<FixedOffset>::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f%:z") {
        return Some(d.with_timezone(&tz));
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, fmt) {
            return local_to_dt(n, tz);
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return local_to_dt(d.and_time(NaiveTime::MIN), tz);
    }
    if let Ok(ms) = s.parse::<i64>()
        && s.len() >= 12
    {
        return from_millis(ms, tz);
    }
    None
}

/// ISO text like Luxon `toISO()`: `Z` for UTC, numeric offset otherwise.
pub(crate) fn to_iso(d: &DateTime<Tz>) -> String {
    let off = d.offset().fix().local_minus_utc();
    let tail = if off == 0 && d.timezone() == Tz::UTC {
        "Z".to_owned()
    } else {
        let sign = if off < 0 { '-' } else { '+' };
        format!("{sign}{:02}:{:02}", off.abs() / 3600, off.abs() % 3600 / 60)
    };
    format!("{}{tail}", d.format("%Y-%m-%dT%H:%M:%S%.3f"))
}

// ------------------------------------------------------------------ arithmetic

pub(crate) fn add_months(d: DateTime<Tz>, months: i64) -> Option<DateTime<Tz>> {
    let total = i64::from(d.year()) * 12 + i64::from(d.month0()) + months;
    let year = i32::try_from(total.div_euclid(12)).ok()?;
    let month0 = u32::try_from(total.rem_euclid(12)).ok()?;
    // clamp the day like Luxon (Jan 31 + 1 month = Feb 28/29)
    let mut day = d.day();
    loop {
        if let Some(date) = NaiveDate::from_ymd_opt(year, month0 + 1, day) {
            return local_to_dt(date.and_time(d.time()), d.timezone());
        }
        day -= 1;
        if day == 0 {
            return None;
        }
    }
}

/// `unit` is a Luxon plural (`years` ... `milliseconds`).
pub(crate) fn add_unit(d: DateTime<Tz>, unit: &str, amount: f64) -> Option<DateTime<Tz>> {
    let whole = amount.trunc() as i64;
    let frac = amount - amount.trunc();
    let ms = |per: f64| (frac * per) as i64;
    match unit {
        "years" | "year" => add_months(d, whole * 12),
        "quarters" | "quarter" => add_months(d, whole * 3),
        "months" | "month" => add_months(d, whole),
        // calendar days keep the wall-clock time across DST
        "weeks" | "week" => add_days_wall(d, whole * 7),
        "days" | "day" => add_days_wall(d, whole),
        "hours" | "hour" => Some(d + Duration::milliseconds(whole * 3_600_000 + ms(3_600_000.0))),
        "minutes" | "minute" => Some(d + Duration::milliseconds(whole * 60_000 + ms(60_000.0))),
        "seconds" | "second" => Some(d + Duration::milliseconds(whole * 1000 + ms(1000.0))),
        "milliseconds" | "millisecond" => Some(d + Duration::milliseconds(whole)),
        _ => None,
    }
}

fn add_days_wall(d: DateTime<Tz>, days: i64) -> Option<DateTime<Tz>> {
    let naive = d.naive_local().checked_add_signed(Duration::days(days))?;
    local_to_dt(naive, d.timezone())
}

/// `startOf(unit)`; `unit` singular or plural.
pub(crate) fn start_of(d: DateTime<Tz>, unit: &str) -> Option<DateTime<Tz>> {
    let tz = d.timezone();
    let date = d.date_naive();
    let naive = match unit.trim_end_matches('s') {
        "year" => NaiveDate::from_ymd_opt(d.year(), 1, 1)?.and_time(NaiveTime::MIN),
        "quarter" => {
            NaiveDate::from_ymd_opt(d.year(), d.month0() / 3 * 3 + 1, 1)?.and_time(NaiveTime::MIN)
        }
        "month" => NaiveDate::from_ymd_opt(d.year(), d.month(), 1)?.and_time(NaiveTime::MIN),
        "week" => {
            let back = i64::from(d.weekday().num_days_from_monday());
            date.checked_sub_signed(Duration::days(back))?
                .and_time(NaiveTime::MIN)
        }
        "day" => date.and_time(NaiveTime::MIN),
        "hour" => date.and_hms_opt(d.hour(), 0, 0)?,
        "minute" => date.and_hms_opt(d.hour(), d.minute(), 0)?,
        "second" => date.and_hms_opt(d.hour(), d.minute(), d.second())?,
        _ => return None,
    };
    local_to_dt(naive, tz)
}

/// `endOf(unit)`: last millisecond of the unit.
pub(crate) fn end_of(d: DateTime<Tz>, unit: &str) -> Option<DateTime<Tz>> {
    let start = start_of(d, unit)?;
    let next = add_unit(start, &format!("{}s", unit.trim_end_matches('s')), 1.0)?;
    Some(next - Duration::milliseconds(1))
}

pub(crate) fn iso_week(d: &DateTime<Tz>) -> u32 {
    d.iso_week().week()
}

// ---------------------------------------------------------------------- format

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

fn offset_text(secs: i32, colon: bool, short: bool) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let (h, m) = (secs.abs() / 3600, secs.abs() % 3600 / 60);
    if short {
        return if m == 0 {
            format!("{sign}{h}")
        } else {
            format!("{sign}{h}:{m:02}")
        };
    }
    if colon {
        format!("{sign}{h:02}:{m:02}")
    } else {
        format!("{sign}{h:02}{m:02}")
    }
}

/// Luxon `toFormat` subset (English locale): `yyyy yy y`, `M MM MMM MMMM`, `d dd`,
/// `H HH h hh m mm s ss S SSS`, `a`, `E EEE EEEE c ccc cccc`, `Z ZZ ZZZ z`, `W WW`, `o ooo`,
/// `q`, `X x`, and `'quoted'` literals.
pub(crate) fn format_luxon(d: &DateTime<Tz>, fmt: &str) -> String {
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            i += 1;
            while i < chars.len() && chars[i] != '\'' {
                out.push(chars[i]);
                i += 1;
            }
            i += 1;
            continue;
        }
        let mut n = 1;
        while i + n < chars.len() && chars[i + n] == c {
            n += 1;
        }
        i += n;
        let h12 = match d.hour() % 12 {
            0 => 12,
            h => h,
        };
        let off = d.offset().fix().local_minus_utc();
        let wd = d.weekday().num_days_from_monday() as usize;
        let mi = d.month0() as usize;
        let s = match (c, n) {
            ('y', 2) => format!("{:02}", d.year().rem_euclid(100)),
            ('y', 4) => format!("{:04}", d.year()),
            ('y', 6) => format!("{:06}", d.year()),
            ('y', _) => d.year().to_string(),
            ('M', 1) => d.month().to_string(),
            ('M', 2) => format!("{:02}", d.month()),
            ('M', 3) => MONTHS[mi][..3].to_owned(),
            ('M', 4) => MONTHS[mi].to_owned(),
            ('M', _) => MONTHS[mi][..1].to_owned(),
            ('L', 1) => d.month().to_string(),
            ('L', 2) => format!("{:02}", d.month()),
            ('L', 3) => MONTHS[mi][..3].to_owned(),
            ('L', _) => MONTHS[mi].to_owned(),
            ('d', 1) => d.day().to_string(),
            ('d', _) => format!("{:02}", d.day()),
            ('H', 1) => d.hour().to_string(),
            ('H', _) => format!("{:02}", d.hour()),
            ('h', 1) => h12.to_string(),
            ('h', _) => format!("{h12:02}"),
            ('m', 1) => d.minute().to_string(),
            ('m', _) => format!("{:02}", d.minute()),
            ('s', 1) => d.second().to_string(),
            ('s', _) => format!("{:02}", d.second()),
            ('S', 1) => d.timestamp_subsec_millis().to_string(),
            ('S', _) => format!("{:03}", d.timestamp_subsec_millis()),
            ('u', _) => format!("{:03}", d.timestamp_subsec_millis()),
            ('a', _) => (if d.hour() < 12 { "AM" } else { "PM" }).to_owned(),
            ('E' | 'c', 1) => (wd + 1).to_string(),
            ('E' | 'c', 2) => format!("{:02}", wd + 1),
            ('E' | 'c', 3) => WEEKDAYS[wd][..3].to_owned(),
            ('E' | 'c', 4) => WEEKDAYS[wd].to_owned(),
            ('E' | 'c', _) => WEEKDAYS[wd][..1].to_owned(),
            ('Z', 1) => offset_text(off, false, true),
            ('Z', 2) => offset_text(off, true, false),
            ('Z', _) => offset_text(off, false, false),
            ('z', _) => d.timezone().name().to_owned(),
            ('W', 1) => iso_week(d).to_string(),
            ('W', _) => format!("{:02}", iso_week(d)),
            ('o', 1) => d.ordinal().to_string(),
            ('o', _) => format!("{:03}", d.ordinal()),
            ('q', 1) => (d.month0() / 3 + 1).to_string(),
            ('q', _) => format!("{:02}", d.month0() / 3 + 1),
            ('X', _) => d.timestamp().to_string(),
            ('x', _) => d.timestamp_millis().to_string(),
            _ => c.to_string().repeat(n),
        };
        out.push_str(&s);
    }
    out
}
