//! Webhook HMAC verification presets. The configured secret is a *handle name* into the
//! `SecretStore`; this module only ever sees the resolved key bytes for the duration of one
//! check and never formats them.

use axum::http::HeaderMap;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::Sha256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HmacAlgorithm {
    #[default]
    Sha256,
    Sha1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigEncoding {
    #[default]
    Hex,
    Base64,
}

/// How to find and check the signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "preset", rename_all = "snake_case")]
pub enum HmacPreset {
    /// `X-Hub-Signature-256: sha256=<hex>` over the raw body.
    Github,
    /// `Stripe-Signature: t=<unix>,v1=<hex>[,v1=..]` over `"<t>.<body>"`, with replay window.
    Stripe {
        #[serde(default = "default_tolerance")]
        tolerance_secs: u64,
    },
    /// `<header>: [<prefix>]<sig>` over the raw body.
    Generic {
        header: String,
        #[serde(default)]
        algorithm: HmacAlgorithm,
        #[serde(default)]
        encoding: SigEncoding,
        /// Stripped from the header value when present (e.g. `sha256=`).
        #[serde(default)]
        prefix: Option<String>,
    },
}

fn default_tolerance() -> u64 {
    300
}

/// Per-webhook verification config. `secret` names a `SecretStore` entry; the value is never
/// part of the config, so configs can be stored in workflow JSON and logged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HmacSpec {
    #[serde(flatten)]
    pub preset: HmacPreset,
    pub secret: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    #[error("signature header missing")]
    Missing,
    #[error("signature header malformed")]
    Malformed,
    #[error("signature mismatch")]
    Mismatch,
    #[error("signature timestamp outside tolerance")]
    Stale,
}

/// Check `body` against the signature in `headers`. `now_unix` is injected for replay checks.
pub fn verify(
    preset: &HmacPreset,
    key: &[u8],
    headers: &HeaderMap,
    body: &[u8],
    now_unix: u64,
) -> Result<(), SignatureError> {
    match preset {
        HmacPreset::Github => {
            let v = header(headers, "x-hub-signature-256")?;
            let sig = v.strip_prefix("sha256=").ok_or(SignatureError::Malformed)?;
            let sig = decode(sig, SigEncoding::Hex)?;
            check(HmacAlgorithm::Sha256, key, &[body], &sig)
        }
        HmacPreset::Stripe { tolerance_secs } => {
            let v = header(headers, "stripe-signature")?;
            let (mut ts, mut sigs) = (None, Vec::new());
            for part in v.split(',') {
                match part.trim().split_once('=') {
                    Some(("t", t)) => ts = t.parse::<u64>().ok(),
                    Some(("v1", s)) => sigs.push(s),
                    _ => {}
                }
            }
            let ts = ts.ok_or(SignatureError::Malformed)?;
            if sigs.is_empty() {
                return Err(SignatureError::Malformed);
            }
            let prefix = format!("{ts}.");
            // Any v1 may match (secret rotation). Check the MAC first, age second, so a bad
            // signature is reported as mismatch rather than leaking the window.
            let ok = sigs.iter().any(|s| {
                decode(s, SigEncoding::Hex)
                    .and_then(|sig| check(HmacAlgorithm::Sha256, key, &[prefix.as_bytes(), body], &sig))
                    .is_ok()
            });
            if !ok {
                return Err(SignatureError::Mismatch);
            }
            if now_unix.abs_diff(ts) > *tolerance_secs {
                return Err(SignatureError::Stale);
            }
            Ok(())
        }
        HmacPreset::Generic { header: name, algorithm, encoding, prefix } => {
            let v = header(headers, name)?;
            let v = match prefix {
                Some(p) => v.strip_prefix(p.as_str()).ok_or(SignatureError::Malformed)?,
                None => v,
            };
            check(*algorithm, key, &[body], &decode(v.trim(), *encoding)?)
        }
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, SignatureError> {
    headers
        .get(name)
        .ok_or(SignatureError::Missing)?
        .to_str()
        .map(str::trim)
        .map_err(|_| SignatureError::Malformed)
}

fn decode(s: &str, enc: SigEncoding) -> Result<Vec<u8>, SignatureError> {
    match enc {
        SigEncoding::Hex => {
            if s.len() % 2 != 0 || !s.is_ascii() {
                return Err(SignatureError::Malformed);
            }
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| SignatureError::Malformed))
                .collect()
        }
        SigEncoding::Base64 => STANDARD
            .decode(s)
            .or_else(|_| URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')))
            .map_err(|_| SignatureError::Malformed),
    }
}

/// Constant-time compare via `Mac::verify_slice`.
fn check(alg: HmacAlgorithm, key: &[u8], chunks: &[&[u8]], sig: &[u8]) -> Result<(), SignatureError> {
    fn run<M: Mac + hmac::digest::KeyInit>(key: &[u8], chunks: &[&[u8]], sig: &[u8]) -> bool {
        let Ok(mut m) = <M as Mac>::new_from_slice(key) else { return false };
        for c in chunks {
            m.update(c);
        }
        m.verify_slice(sig).is_ok()
    }
    let ok = match alg {
        HmacAlgorithm::Sha256 => run::<Hmac<Sha256>>(key, chunks, sig),
        HmacAlgorithm::Sha1 => run::<Hmac<Sha1>>(key, chunks, sig),
    };
    if ok { Ok(()) } else { Err(SignatureError::Mismatch) }
}

/// Test/diagnostic helper: hex HMAC-SHA256.
#[cfg(test)]
pub(crate) fn sign_hex(key: &[u8], msg: &[u8]) -> String {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).unwrap();
    m.update(msg);
    m.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&'static str, String)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(*k, v.parse().unwrap());
        }
        m
    }

    #[test]
    fn github_vector() {
        // Vector from GitHub's webhook docs.
        let key = b"It's a Secret to Everybody";
        let body = b"Hello, World!";
        let sig = "757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        let ok = h(&[("x-hub-signature-256", format!("sha256={sig}"))]);
        assert_eq!(verify(&HmacPreset::Github, key, &ok, body, 0), Ok(()));
        assert_eq!(verify(&HmacPreset::Github, key, &ok, b"Hello, World?", 0), Err(SignatureError::Mismatch));
        assert_eq!(verify(&HmacPreset::Github, b"wrong", &ok, body, 0), Err(SignatureError::Mismatch));
        assert_eq!(verify(&HmacPreset::Github, key, &HeaderMap::new(), body, 0), Err(SignatureError::Missing));
        let bare = h(&[("x-hub-signature-256", sig.to_owned())]);
        assert_eq!(verify(&HmacPreset::Github, key, &bare, body, 0), Err(SignatureError::Malformed));
        let odd = h(&[("x-hub-signature-256", "sha256=abc".to_owned())]);
        assert_eq!(verify(&HmacPreset::Github, key, &odd, body, 0), Err(SignatureError::Malformed));
    }

    #[test]
    fn stripe_signs_timestamp_dot_body_with_window() {
        let key = b"whsec_test";
        let body = br#"{"id":"evt_1"}"#;
        let t = 1_700_000_000u64;
        let sig = sign_hex(key, format!("{t}.{}", std::str::from_utf8(body).unwrap()).as_bytes());
        let hdr = h(&[("stripe-signature", format!("t={t},v1=deadbeef,v1={sig},v0=00"))]);
        let p = HmacPreset::Stripe { tolerance_secs: 300 };
        assert_eq!(verify(&p, key, &hdr, body, t + 10), Ok(()));
        assert_eq!(verify(&p, key, &hdr, body, t + 301), Err(SignatureError::Stale));
        assert_eq!(verify(&p, key, &hdr, b"{}", t), Err(SignatureError::Mismatch));
        // forged timestamp: sig no longer matches
        let forged = h(&[("stripe-signature", format!("t={},v1={sig}", t + 1))]);
        assert_eq!(verify(&p, key, &forged, body, t + 1), Err(SignatureError::Mismatch));
        let nots = h(&[("stripe-signature", format!("v1={sig}"))]);
        assert_eq!(verify(&p, key, &nots, body, t), Err(SignatureError::Malformed));
    }

    #[test]
    fn generic_hex_and_base64_with_prefix() {
        let key = b"k";
        let body = b"payload";
        let hex = sign_hex(key, body);
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).unwrap();
        m.update(body);
        let b64 = STANDARD.encode(m.finalize().into_bytes());

        let p_hex = HmacPreset::Generic {
            header: "X-Sig".into(),
            algorithm: HmacAlgorithm::Sha256,
            encoding: SigEncoding::Hex,
            prefix: Some("sha256=".into()),
        };
        assert_eq!(verify(&p_hex, key, &h(&[("x-sig", format!("sha256={hex}"))]), body, 0), Ok(()));
        assert_eq!(verify(&p_hex, key, &h(&[("x-sig", hex.clone())]), body, 0), Err(SignatureError::Malformed));

        let p_b64 = HmacPreset::Generic {
            header: "x-sig".into(),
            algorithm: HmacAlgorithm::Sha256,
            encoding: SigEncoding::Base64,
            prefix: None,
        };
        assert_eq!(verify(&p_b64, key, &h(&[("x-sig", b64)]), body, 0), Ok(()));
        assert_eq!(verify(&p_b64, key, &h(&[("x-sig", "AAAA".into())]), body, 0), Err(SignatureError::Mismatch));
    }

    #[test]
    fn generic_sha1() {
        let key = b"k";
        let mut m = <Hmac<Sha1> as Mac>::new_from_slice(key).unwrap();
        m.update(b"x");
        let hex: String = m.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
        let p = HmacPreset::Generic {
            header: "x-s".into(),
            algorithm: HmacAlgorithm::Sha1,
            encoding: SigEncoding::Hex,
            prefix: None,
        };
        assert_eq!(verify(&p, key, &h(&[("x-s", hex)]), b"x", 0), Ok(()));
    }

    #[test]
    fn spec_roundtrips_as_json_without_secret_value() {
        let j = r#"{"preset":"stripe","secret":"wh.stripe"}"#;
        let s: HmacSpec = serde_json::from_str(j).unwrap();
        assert_eq!(s.preset, HmacPreset::Stripe { tolerance_secs: 300 });
        assert_eq!(s.secret, "wh.stripe");
    }
}
