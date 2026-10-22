//! AWS Signature Version 4 for S3 (header form), hand-rolled over hmac + sha2 so the crate
//! needs no aws-sdk. Only what the S3 node uses: path/query canonicalisation without double
//! encoding, signed payload hash, optional session token.

use std::time::SystemTime;

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

const ALGORITHM: &str = "AWS4-HMAC-SHA256";

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    // HMAC accepts keys of any length
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("hmac accepts any key length");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

/// Percent-encode everything but the unreserved set; `/` kept when `keep_slash` (object keys).
pub(crate) fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(b))
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `YYYYMMDDTHHMMSSZ` in UTC.
pub(crate) fn amz_date(now: SystemTime) -> String {
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // days since 1970-01-01 -> civil date (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[derive(Debug, Clone)]
pub(crate) struct Keys {
    pub access_key: String,
    pub secret_key: String,
    pub region: String,
    pub session_token: Option<String>,
}

/// Headers to add to the request: `x-amz-date`, `x-amz-content-sha256`, `authorization` and,
/// with a session token, `x-amz-security-token`. `extra` headers (lowercase names) are signed
/// too and must be sent unchanged. `url` must already be percent-encoded canonically (build
/// it with [`uri_encode`]), the host header is derived from it.
pub(crate) fn sign(
    keys: &Keys,
    service: &str,
    method: &str,
    url: &reqwest::Url,
    extra: &[(String, String)],
    payload: &[u8],
    date: &str,
) -> Vec<(String, String)> {
    let payload_hash = sha256_hex(payload);
    let host = match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_owned(),
        _ => String::new(),
    };

    let mut headers: Vec<(String, String)> = vec![
        ("host".into(), host),
        ("x-amz-content-sha256".into(), payload_hash.clone()),
        ("x-amz-date".into(), date.to_owned()),
    ];
    if let Some(t) = &keys.session_token {
        headers.push(("x-amz-security-token".into(), t.clone()));
    }
    headers.extend(
        extra
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned())),
    );
    headers.sort();

    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");

    let mut query: Vec<String> = url
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            if p.contains('=') {
                p.to_owned()
            } else {
                format!("{p}=")
            }
        })
        .collect();
    query.sort();

    let canonical = format!(
        "{method}\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        url.path(),
        query.join("&")
    );

    let day = &date[..8];
    let scope = format!("{day}/{}/{service}/aws4_request", keys.region);
    let to_sign = format!(
        "{ALGORITHM}\n{date}\n{scope}\n{}",
        sha256_hex(canonical.as_bytes())
    );
    let k_date = hmac(
        format!("AWS4{}", keys.secret_key).as_bytes(),
        day.as_bytes(),
    );
    let k_region = hmac(&k_date, keys.region.as_bytes());
    let k_service = hmac(&k_region, service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex(&hmac(&k_signing, to_sign.as_bytes()));

    let mut out = vec![
        ("x-amz-date".to_owned(), date.to_owned()),
        ("x-amz-content-sha256".to_owned(), payload_hash),
        (
            "authorization".to_owned(),
            format!(
                "{ALGORITHM} Credential={}/{scope},SignedHeaders={signed_headers},Signature={signature}",
                keys.access_key
            ),
        ),
    ];
    if let Some(t) = &keys.session_token {
        out.push(("x-amz-security-token".to_owned(), t.clone()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Keys {
        Keys {
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            region: "us-east-1".into(),
            session_token: None,
        }
    }

    fn header<'a>(h: &'a [(String, String)], name: &str) -> &'a str {
        h.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .unwrap()
    }

    /// "GET Object" example from the AWS SigV4 S3 documentation.
    #[test]
    fn aws_get_object_vector() {
        let url = reqwest::Url::parse("https://examplebucket.s3.amazonaws.com/test.txt").unwrap();
        let h = sign(
            &keys(),
            "s3",
            "GET",
            &url,
            &[("range".into(), "bytes=0-9".into())],
            b"",
            "20130524T000000Z",
        );
        assert_eq!(
            header(&h, "authorization"),
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,\
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,\
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
        assert_eq!(
            header(&h, "x-amz-content-sha256"),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// "GET Bucket (List Objects)" example: query string canonicalisation.
    #[test]
    fn aws_list_objects_vector() {
        let url =
            reqwest::Url::parse("https://examplebucket.s3.amazonaws.com/?max-keys=2&prefix=J")
                .unwrap();
        let h = sign(&keys(), "s3", "GET", &url, &[], b"", "20130524T000000Z");
        assert!(header(&h, "authorization").ends_with(
            "Signature=34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
        ));
    }

    #[test]
    fn encoding_and_date() {
        assert_eq!(uri_encode("a b/ü~.txt", true), "a%20b/%C3%BC~.txt");
        assert_eq!(uri_encode("a/b", false), "a%2Fb");
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_369_353_600 + 3_661);
        assert_eq!(amz_date(t), "20130524T010101Z");
        assert_eq!(amz_date(SystemTime::UNIX_EPOCH), "19700101T000000Z");
    }
}
