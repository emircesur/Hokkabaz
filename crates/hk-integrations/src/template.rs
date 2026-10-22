//! `$credentials.*` templates used by credential `authenticate` / `test` definitions.
//!
//! n8n writes these as expressions (`={{ $credentials.token }}`, `=Bearer {{$credentials.key}}`).
//! Credential values must never reach the general expression engine (it can log, throw
//! with the source text, and sees `$env`), so this renderer understands exactly one form:
//! `{{ $credentials.<dotted.path> }}` placeholders inside literal text. Anything else in a
//! placeholder is an error, not a silent empty string.

use serde_json::{Map, Value};

use crate::credentials::CredentialError;

/// Render `tpl` against decrypted credential `data`. A leading `=` (n8n expression marker)
/// is stripped. Text without placeholders is returned as is.
pub fn render(tpl: &str, data: &Map<String, Value>) -> Result<String, CredentialError> {
    let tpl = tpl.strip_prefix('=').unwrap_or(tpl);
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| CredentialError::Template("unterminated `{{`".into()))?;
        out.push_str(&lookup(after[..end].trim(), data)?);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn lookup(expr: &str, data: &Map<String, Value>) -> Result<String, CredentialError> {
    let path = expr
        .strip_prefix("$credentials.")
        .filter(|p| {
            !p.is_empty()
                && p.split('.')
                    .all(|s| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_'))
        })
        .ok_or_else(|| {
            // names the expression, which is template source, never a credential value
            CredentialError::Template(format!(
                "unsupported expression `{expr}` (only $credentials.<field> is allowed)"
            ))
        })?;
    let mut parts = path.split('.');
    let first = parts.next().unwrap_or_default();
    let mut cur = data
        .get(first)
        .ok_or_else(|| CredentialError::MissingField(first.to_owned()))?;
    for p in parts {
        cur = cur
            .get(p)
            .ok_or_else(|| CredentialError::MissingField(path.to_owned()))?;
    }
    Ok(display(cur))
}

/// String form of a JSON value as n8n would interpolate it.
pub fn display(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(display).collect::<Vec<_>>().join(","),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn data() -> Map<String, Value> {
        match json!({"token": "s3", "oauthTokenData": {"access_token": "at"}, "n": 5}) {
            Value::Object(m) => m,
            _ => unreachable!(),
        }
    }

    #[test]
    fn renders_placeholders() {
        let d = data();
        assert_eq!(render("={{$credentials.token}}", &d).unwrap(), "s3");
        assert_eq!(
            render(
                "=Bearer {{ $credentials.oauthTokenData.access_token }}!",
                &d
            )
            .unwrap(),
            "Bearer at!"
        );
        assert_eq!(render("plain", &d).unwrap(), "plain");
        assert_eq!(render("{{$credentials.n}}", &d).unwrap(), "5");
    }

    #[test]
    fn rejects_everything_else() {
        let d = data();
        assert!(matches!(
            render("={{ $env.HOME }}", &d),
            Err(CredentialError::Template(_))
        ));
        assert!(matches!(
            render("={{ $credentials.nope }}", &d),
            Err(CredentialError::MissingField(_))
        ));
        assert!(render("={{ $credentials.token", &d).is_err());
        // no value leak in the error
        let e = render("={{ $credentials.token + 1 }}", &d)
            .unwrap_err()
            .to_string();
        assert!(!e.contains("s3"));
    }
}
