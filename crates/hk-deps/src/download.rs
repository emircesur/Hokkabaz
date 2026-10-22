//! Streaming download with progress, cancel and SHA-256 verification.

use std::path::Path;

use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::{DepsError, Result};

pub(crate) fn client() -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .user_agent(concat!("hokkabaz/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(20));
    // Behind a TLS-inspecting proxy the bundled roots do not know the proxy's CA: `HK_CA_BUNDLE`
    // (or the usual `SSL_CERT_FILE`) names a PEM file to trust in addition.
    if let Some(pem) = std::env::var_os("HK_CA_BUNDLE")
        .or_else(|| std::env::var_os("SSL_CERT_FILE"))
        .and_then(|p| std::fs::read(p).ok())
        && let Ok(certs) = reqwest::Certificate::from_pem_bundle(&pem)
    {
        for c in certs {
            b = b.add_root_certificate(c);
        }
    }
    b.build()
        .map_err(|e| DepsError::Download(e.without_url().to_string()))
}

/// Small text resource (checksums, release JSON).
pub(crate) async fn text(client: &reqwest::Client, url: &str) -> Result<String> {
    let r = client
        .get(url)
        .send()
        .await
        .map_err(|e| DepsError::Download(e.without_url().to_string()))?;
    if !r.status().is_success() {
        return Err(DepsError::Download(format!(
            "{url} answered {}",
            r.status()
        )));
    }
    r.text()
        .await
        .map_err(|e| DepsError::Download(e.without_url().to_string()))
}

/// Download `url` to `dest`; returns the SHA-256 (hex) of what was written.
pub(crate) async fn file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    cancel: &CancellationToken,
    progress: impl Fn(u64, u64),
) -> Result<String> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let resp =
        client.get(url).send().await.map_err(|e| {
            DepsError::Download(format!("cannot reach the host: {}", e.without_url()))
        })?;
    if !resp.status().is_success() {
        return Err(DepsError::Download(format!(
            "the host answered {}",
            resp.status()
        )));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut out = tokio::fs::File::create(dest).await?;
    let mut hash = Sha256::new();
    let mut done = 0u64;
    let mut last = std::time::Instant::now();
    let mut stream = resp.bytes_stream();
    loop {
        let next = tokio::select! {
            () = cancel.cancelled() => {
                drop(out);
                let _ = tokio::fs::remove_file(dest).await;
                return Err(DepsError::Cancelled);
            }
            n = stream.next() => n,
        };
        let Some(chunk) = next else { break };
        let chunk = chunk
            .map_err(|e| DepsError::Download(format!("connection lost: {}", e.without_url())))?;
        hash.update(&chunk);
        out.write_all(&chunk).await?;
        done += chunk.len() as u64;
        if last.elapsed() >= std::time::Duration::from_millis(250) {
            last = std::time::Instant::now();
            progress(done, total);
        }
    }
    out.flush().await?;
    progress(done, total.max(done));
    if total > 0 && done != total {
        let _ = tokio::fs::remove_file(dest).await;
        return Err(DepsError::Download(format!(
            "incomplete download ({done} of {total} bytes)"
        )));
    }
    Ok(hex::encode(hash.finalize()))
}

/// `sha256sum` format: `<hex>  <name>` per line; the name may carry a leading `*` or path.
pub(crate) fn sum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (h, n) = l.split_once(char::is_whitespace)?;
        let n = n.trim().trim_start_matches('*');
        (n == name || n.rsplit(['/', '\\']).next() == Some(name)).then(|| h.to_ascii_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_sum_by_file_name() {
        let sums = "aaa  ffmpeg-master-latest-win64-gpl.zip\nBBB *./out/ffmpeg-master-latest-linux64-gpl.tar.xz\n";
        assert_eq!(
            sum_for(sums, "ffmpeg-master-latest-win64-gpl.zip").as_deref(),
            Some("aaa")
        );
        assert_eq!(
            sum_for(sums, "ffmpeg-master-latest-linux64-gpl.tar.xz").as_deref(),
            Some("bbb")
        );
        assert!(sum_for(sums, "nope").is_none());
    }
}
