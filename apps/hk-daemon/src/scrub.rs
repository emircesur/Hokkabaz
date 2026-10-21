//! Execution data must never carry secrets. Webhook items hold the caller's request headers,
//! which can include the instance bearer token or session cookies; they stay available to
//! nodes during the run but are blanked before anything is persisted.

use serde_json::Value;

const SENSITIVE: [&str; 6] =
    ["authorization", "proxy-authorization", "cookie", "set-cookie", "x-n8n-api-key", "x-api-key"];
pub const REDACTED: &str = "[redacted]";

/// Blank credential-bearing entries of every `headers` object, at any depth.
pub fn scrub_headers(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (k, child) in m.iter_mut() {
                if k.eq_ignore_ascii_case("headers")
                    && let Value::Object(h) = child
                {
                    for (name, val) in h.iter_mut() {
                        if SENSITIVE.iter().any(|s| name.eq_ignore_ascii_case(s)) {
                            *val = Value::String(REDACTED.into());
                        }
                    }
                }
                scrub_headers(child);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(scrub_headers),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn blanks_auth_headers_at_depth_only_under_headers() {
        let mut v = json!({"resultData": {"runData": {"Webhook": [{"data": {"main": [[{"json": {
            "headers": {"Authorization": "Bearer hkt_x", "content-type": "a/b", "cookie": "s=1"},
            "body": {"authorization": "kept: not a header"}
        }}]]}}]}}});
        scrub_headers(&mut v);
        let j = &v["resultData"]["runData"]["Webhook"][0]["data"]["main"][0][0]["json"];
        assert_eq!(j["headers"], json!({"Authorization": REDACTED, "content-type": "a/b", "cookie": REDACTED}));
        assert_eq!(j["body"]["authorization"], "kept: not a header");
        assert!(!v.to_string().contains("hkt_x"));
    }
}
