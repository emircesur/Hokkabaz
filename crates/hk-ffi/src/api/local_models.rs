//! On-device models: browse, download, delete, offline fallback. Needs the in-process core.

use crate::error::HkError;
use crate::frb_generated::StreamSink;
use crate::local_models::ctx;

fn to_json(v: &serde_json::Value) -> Result<String, HkError> {
    serde_json::to_string(v).map_err(|e| HkError::internal(e.to_string()))
}

/// `{engine, device:{total_ram,available_ram,cores}, recommended, models:[..], custom:[..], fallback}`.
/// Each catalog model carries `fit` (good|tight|too_big), `installed`, `downloading`, `progress`.
pub async fn local_models_overview() -> Result<String, HkError> {
    to_json(&ctx()?.overview())
}

/// Download a catalog model. Emits `{"done":N,"total":N}` JSON progress events and completes when
/// the file is verified; dropping the Dart stream does not stop it, use `local_model_cancel`.
pub async fn local_model_download(id: String, sink: StreamSink<String>) -> Result<(), HkError> {
    ctx()?
        .download(&id, |p| {
            let _ = sink.add(format!(r#"{{"done":{},"total":{}}}"#, p.done, p.total));
        })
        .await
}

pub async fn local_model_cancel(id: String) -> Result<(), HkError> {
    ctx()?.cancel(&id);
    Ok(())
}

/// Delete a downloaded model (a custom one is only forgotten, its file stays).
pub async fn local_model_delete(id: String) -> Result<(), HkError> {
    let c = ctx()?;
    if c.overview()["fallback"].as_str() == Some(id.as_str()) {
        c.set_fallback(None).await?;
    }
    c.store
        .remove(&id)
        .map_err(|e| HkError::invalid(e.to_string()))
}

/// Use a GGUF file already on disk.
pub async fn local_model_add_file(path: String, name: Option<String>) -> Result<String, HkError> {
    let m = ctx()?
        .store
        .add_custom(std::path::Path::new(&path), name.as_deref())
        .map_err(|e| HkError::invalid(e.to_string()))?;
    Ok(m.id)
}

/// Model used when the chosen default model is unreachable (offline, rate limited, key problem).
/// `None` turns the fallback off. Applies immediately.
pub async fn local_set_fallback(id: Option<String>) -> Result<(), HkError> {
    let c = ctx()?;
    c.set_fallback(id).await?;
    c.rebuild_providers().await;
    Ok(())
}
