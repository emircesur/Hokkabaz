//! Generic command gate: `domain.action` + JSON params → JSON result, and named event feeds.
//! Lets features ship without growing the typed FFI surface (see `crate::gate`).

use crate::error::HkError;
use crate::frb_generated::StreamSink;
use crate::handle::core;
use futures::StreamExt;

/// Run `method` (e.g. `jobs.list`, `memory.save`) with JSON `params_json`; returns JSON.
pub async fn hk_call(method: String, params_json: String) -> Result<String, HkError> {
    let params: serde_json::Value = if params_json.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(&params_json).map_err(|e| {
            HkError::new(
                crate::error::HkErrorKind::InvalidArgument,
                format!("params: {e}"),
            )
        })?
    };
    let out = core()?.call(&method, params).await?;
    Ok(out.to_string())
}

/// Stream of JSON events for `topic` (`jobs`).
pub async fn hk_events(topic: String, sink: StreamSink<String>) -> Result<(), HkError> {
    let mut s = core()?.events(&topic).await?;
    while let Some(ev) = s.next().await {
        if sink.add(ev).is_err() {
            break;
        }
    }
    Ok(())
}
