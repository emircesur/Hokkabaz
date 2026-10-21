//! HTTP plumbing shared by OpenAI-style adapters: credential scrubbing,
//! connect-phase retry, error-response mapping.

use std::time::Duration;

use reqwest::header::{ACCEPT, HeaderMap, RETRY_AFTER};
use serde_json::Value;

use super::error::error_from_response;
use super::retry::RetryPolicy;
use crate::{CancellationToken, LlmError, Result};

/// Removes credentials from text that may be logged or surfaced. Providers
/// sometimes echo the key (or a header secret) in error bodies.
#[derive(Default, Clone)]
pub struct Scrubber(Vec<String>);

impl std::fmt::Debug for Scrubber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Scrubber({} secrets)", self.0.len())
    }
}

impl Scrubber {
    /// Register a secret. Empty values are ignored.
    pub fn add(&mut self, secret: &str) {
        if !secret.is_empty() {
            self.0.push(secret.to_owned());
        }
    }

    /// Replace every known secret in `s`.
    pub fn text(&self, s: String) -> String {
        self.0
            .iter()
            .fold(s, |acc, k| acc.replace(k.as_str(), "<redacted>"))
    }

    /// Scrub the free-text parts of an error.
    pub fn err(&self, e: LlmError) -> LlmError {
        match e {
            LlmError::Http {
                status,
                message,
                retry_after,
            } => LlmError::Http {
                status,
                message: self.text(message),
                retry_after,
            },
            LlmError::Network(m) => LlmError::Network(self.text(m)),
            LlmError::Auth(m) => LlmError::Auth(self.text(m)),
            LlmError::ContextLength(m) => LlmError::ContextLength(self.text(m)),
            LlmError::InvalidRequest(m) => LlmError::InvalidRequest(self.text(m)),
            other => other,
        }
    }
}

/// Everything needed to talk to one backend; borrowed from the adapter.
#[allow(missing_debug_implementations)] // holds the bearer token
pub struct Endpoint<'a> {
    /// Shared HTTP client.
    pub client: &'a reqwest::Client,
    /// Provider id, for logs.
    pub provider: &'a str,
    /// Bearer token, if the backend uses one.
    pub bearer: Option<&'a str>,
    /// Extra headers (marked sensitive by the adapter).
    pub headers: &'a HeaderMap,
    /// Credential scrubber for errors.
    pub scrub: &'a Scrubber,
}

impl Endpoint<'_> {
    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut rb = self
            .client
            .request(method, url)
            .headers(self.headers.clone());
        if let Some(k) = self.bearer {
            rb = rb.bearer_auth(k);
        }
        rb
    }

    async fn finish(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let resp = rb
            .send()
            .await
            .map_err(|e| LlmError::Network(self.scrub.text(e.without_url().to_string())))?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let text = resp.text().await.unwrap_or_default();
        Err(self
            .scrub
            .err(error_from_response(status, retry_after, &text)))
    }

    /// GET `url` and parse the JSON body. No retry: used for model listing.
    pub async fn get_json(&self, url: &str) -> Result<Value> {
        let resp = self.finish(self.request(reqwest::Method::GET, url)).await?;
        resp.json()
            .await
            .map_err(|e| LlmError::Protocol(format!("bad json body: {e}")))
    }

    /// POST `body` expecting an SSE response; retries the connect phase.
    pub async fn post_sse(
        &self,
        url: &str,
        body: &Value,
        retry: &RetryPolicy,
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response> {
        let mut attempt = 0;
        loop {
            let rb = self
                .request(reqwest::Method::POST, url)
                .header(ACCEPT, "text/event-stream")
                .json(body);
            let res = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(LlmError::Cancelled),
                r = self.finish(rb) => r,
            };
            match res {
                Err(e) if e.is_retryable() && attempt < retry.max_retries => {
                    let wait = retry.delay(
                        attempt,
                        match &e {
                            LlmError::Http { retry_after, .. } => *retry_after,
                            _ => None,
                        },
                    );
                    tracing::warn!(provider = self.provider, attempt, ?wait, error = %e, "retrying request");
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => return Err(LlmError::Cancelled),
                        () = tokio::time::sleep(wait) => {}
                    }
                    attempt += 1;
                }
                other => return other,
            }
        }
    }
}
