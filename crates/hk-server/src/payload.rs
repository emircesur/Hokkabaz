//! Webhook request -> n8n item `{headers, params, query, body}`.
//!
//! Body by content type: JSON -> value; urlencoded -> object (repeated keys become arrays);
//! multipart -> object of *text* parts (file parts are dropped for now); `text/*` and XML ->
//! string; anything else -> `{}` (the raw bytes still travel next to the item for a Raw Body
//! option). Header names are lower-cased like Node does.

use axum::http::HeaderMap;
use serde_json::{Map, Value};

use crate::error::ApiError;

pub fn headers_json(h: &HeaderMap) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in h {
        let Ok(v) = v.to_str() else { continue };
        match out.get_mut(k.as_str()) {
            // Node joins repeated headers with ", ".
            Some(Value::String(prev)) => {
                prev.push_str(", ");
                prev.push_str(v);
            }
            _ => {
                out.insert(k.as_str().to_owned(), Value::String(v.to_owned()));
            }
        }
    }
    out
}

pub fn query_json(raw: Option<&str>) -> Map<String, Value> {
    form_pairs(raw.unwrap_or("").as_bytes())
}

fn form_pairs(bytes: &[u8]) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in form_urlencoded::parse(bytes) {
        push_field(&mut out, k.into_owned(), Value::String(v.into_owned()));
    }
    out
}

fn push_field(out: &mut Map<String, Value>, k: String, v: Value) {
    match out.remove(&k) {
        None => {
            out.insert(k, v);
        }
        Some(Value::Array(mut a)) => {
            a.push(v);
            out.insert(k, Value::Array(a));
        }
        Some(prev) => {
            out.insert(k, Value::Array(vec![prev, v]));
        }
    }
}

/// Media type without parameters, lower-cased.
fn media_type(headers: &HeaderMap) -> String {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|m| m.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

pub fn parse_body(headers: &HeaderMap, body: &[u8]) -> Result<Value, ApiError> {
    let mt = media_type(headers);
    let empty = || Value::Object(Map::new());
    if body.is_empty() {
        return Ok(empty());
    }
    if mt == "application/json" || mt.ends_with("+json") {
        return serde_json::from_slice(body)
            .map_err(|e| ApiError::BadRequest(format!("Invalid JSON in request body: {e}")));
    }
    if mt == "application/x-www-form-urlencoded" {
        return Ok(Value::Object(form_pairs(body)));
    }
    if mt == "multipart/form-data" {
        let boundary = multipart_boundary(headers)
            .ok_or_else(|| ApiError::BadRequest("multipart without boundary".into()))?;
        return Ok(Value::Object(multipart_text(body, &boundary)));
    }
    if mt.starts_with("text/") || mt == "application/xml" || mt.ends_with("+xml") {
        return Ok(Value::String(String::from_utf8_lossy(body).into_owned()));
    }
    Ok(empty())
}

fn multipart_boundary(headers: &HeaderMap) -> Option<String> {
    let ct = headers
        .get(axum::http::header::CONTENT_TYPE)?
        .to_str()
        .ok()?;
    ct.split(';').skip(1).find_map(|p| {
        let (k, v) = p.trim().split_once('=')?;
        k.eq_ignore_ascii_case("boundary")
            .then(|| v.trim().trim_matches('"').to_owned())
    })
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

/// Text parts only: a part with a `filename` is a file upload and is skipped.
fn multipart_text(body: &[u8], boundary: &str) -> Map<String, Value> {
    let delim = format!("--{boundary}").into_bytes();
    let mut out = Map::new();
    let mut pos = match find(body, &delim, 0) {
        Some(p) => p + delim.len(),
        None => return out,
    };
    loop {
        // `--` right after a delimiter closes the message.
        if body[pos..].starts_with(b"--") {
            break;
        }
        let part_start = pos
            + if body[pos..].starts_with(b"\r\n") {
                2
            } else {
                0
            };
        let Some(next) = find(body, &delim, part_start) else {
            break;
        };
        // Part ends at the CRLF that precedes the next delimiter.
        let part_end = if next >= 2 && &body[next - 2..next] == b"\r\n" {
            next - 2
        } else {
            next
        };
        if part_end >= part_start {
            handle_part(&body[part_start..part_end], &mut out);
        }
        pos = next + delim.len();
        if pos >= body.len() {
            break;
        }
    }
    out
}

fn handle_part(part: &[u8], out: &mut Map<String, Value>) {
    let Some(split) = find(part, b"\r\n\r\n", 0) else {
        return;
    };
    let head = String::from_utf8_lossy(&part[..split]);
    let value = &part[split + 4..];
    let Some(disp) = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-disposition:"))
    else {
        return;
    };
    let param = |name: &str| -> Option<String> {
        let needle = format!("{name}=\"");
        let i = disp.find(&needle)? + needle.len();
        let j = disp[i..].find('"')?;
        Some(disp[i..i + j].to_owned())
    };
    if param("filename").is_some() {
        return;
    }
    if let Some(name) = param("name") {
        push_field(
            out,
            name,
            Value::String(String::from_utf8_lossy(value).into_owned()),
        );
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ct(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(axum::http::header::CONTENT_TYPE, v.parse().unwrap());
        h
    }

    #[test]
    fn json_form_text_and_raw() {
        assert_eq!(
            parse_body(&ct("application/json; charset=utf-8"), br#"{"a":[1]}"#).unwrap(),
            json!({"a":[1]})
        );
        assert!(matches!(
            parse_body(&ct("application/json"), b"{"),
            Err(ApiError::BadRequest(_))
        ));
        assert_eq!(
            parse_body(&ct("application/x-www-form-urlencoded"), b"a=1&b=x%20y&a=2").unwrap(),
            json!({"a":["1","2"],"b":"x y"})
        );
        assert_eq!(parse_body(&ct("text/plain"), b"hi").unwrap(), json!("hi"));
        assert_eq!(
            parse_body(&ct("application/octet-stream"), b"\x00\x01").unwrap(),
            json!({})
        );
        assert_eq!(parse_body(&HeaderMap::new(), b"").unwrap(), json!({}));
    }

    #[test]
    fn multipart_text_parts_only() {
        let body = "--XX\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\none\r\n\
                    --XX\r\nContent-Disposition: form-data; name=\"f\"; filename=\"x.bin\"\r\nContent-Type: application/octet-stream\r\n\r\nBIN\r\n\
                    --XX\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\ntwo\r\n--XX--\r\n";
        let v = parse_body(&ct("multipart/form-data; boundary=XX"), body.as_bytes()).unwrap();
        assert_eq!(v, json!({"a":["one","two"]}));
        assert!(matches!(
            parse_body(&ct("multipart/form-data"), b"x"),
            Err(ApiError::BadRequest(_))
        ));
    }

    #[test]
    fn headers_lowercase_and_join_query_repeat() {
        let mut h = HeaderMap::new();
        h.append("X-Thing", "a".parse().unwrap());
        h.append("x-thing", "b".parse().unwrap());
        assert_eq!(headers_json(&h)["x-thing"], json!("a, b"));
        assert_eq!(
            Value::Object(query_json(Some("k=1&k=2&z=%C3%BC"))),
            json!({"k":["1","2"],"z":"ü"})
        );
        assert!(query_json(None).is_empty());
    }
}
