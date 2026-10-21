//! `core::*` surface: version probe + demo stream (P0.5 wiring check).

use std::time::Duration;

use flutter_rust_bridge::frb;

use crate::error::{HkError, HkErrorKind};
use crate::frb_generated::StreamSink;

/// Hard cap so a bad caller cannot park a task forever.
const MAX_TICKS: u32 = 10_000;

/// Version of the Rust core linked into the app.
#[frb(sync)]
pub fn core_version() -> Result<String, HkError> {
    Ok(env!("CARGO_PKG_VERSION").to_owned())
}

/// Emits `0..count` every `interval_ms`, then completes. Proves Rust -> Dart streaming.
pub async fn demo_ticker(
    count: u32,
    interval_ms: u32,
    sink: StreamSink<u32>,
) -> Result<(), HkError> {
    if count > MAX_TICKS {
        return Err(HkError::new(
            HkErrorKind::InvalidArgument,
            format!("count {count} exceeds {MAX_TICKS}"),
        ));
    }
    let period = Duration::from_millis(u64::from(interval_ms));
    for i in 0..count {
        sink.add(i)
            .map_err(|e| HkError::new(HkErrorKind::Closed, e.to_string()))?;
        tokio::time::sleep(period).await;
    }
    Ok(())
}
