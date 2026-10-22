//! `core::*` surface: lifecycle, version probe + demo stream (P0.5 wiring check).

use std::path::PathBuf;
use std::time::Duration;

use flutter_rust_bridge::frb;

use crate::dto::CoreProfile;
use crate::error::{HkError, HkErrorKind};
use crate::frb_generated::StreamSink;
use crate::handle;
use crate::instances::{self, Manager};
use crate::local::{CoreOptions, LocalCore};
use crate::secrets;

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

/// Start the in-process core rooted at `data_dir` (created if missing). Calling it again replaces
/// the running core (Flutter hot restart keeps the native lib loaded).
pub async fn core_init(data_dir: String, profile: CoreProfile) -> Result<(), HkError> {
    if data_dir.trim().is_empty() {
        return Err(HkError::new(HkErrorKind::InvalidArgument, "data_dir must not be empty"));
    }
    for old in handle::take_all() {
        old.shutdown().await;
    }
    let dir = PathBuf::from(data_dir);
    let core = LocalCore::open(dir.clone(), profile, CoreOptions::default()).await?;
    // Instance book next to the local data; a broken file must not stop the local core.
    instances::set_global(Manager::open(&dir, profile, secrets::select(profile, &dir)));
    for old in handle::install_local(core) {
        old.shutdown().await;
    }
    Ok(())
}

/// Stop every core (local and any connected remote): cancels running turns and ends every
/// `session_events` stream. Idempotent.
pub async fn core_shutdown() -> Result<(), HkError> {
    instances::clear_global();
    for core in handle::take_all() {
        core.shutdown().await;
    }
    Ok(())
}
