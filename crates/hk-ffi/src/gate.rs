//! Generic `domain.action` command gate behind `api::hk::hk_call` / `hk_events`.
//! New features land here instead of growing the typed FFI surface (no codegen needed).

use std::path::Path;
use std::sync::Arc;

use futures::StreamExt as _;
use futures::stream::BoxStream;
use hk_llm::Provider;
use hk_server::{AgentRuntime, LocalRuntime};
use hk_store::Store;
use serde_json::{Value, json};

use crate::error::{HkError, HkErrorKind};

pub(crate) struct Ctx<'a> {
    pub runtime: &'a LocalRuntime,
    pub store: &'a Store,
    pub data_dir: &'a Path,
    pub provider: Arc<dyn Provider>,
}

pub(crate) fn bad(msg: impl Into<String>) -> HkError {
    HkError::new(HkErrorKind::InvalidArgument, msg)
}

pub(crate) fn str_arg<'a>(p: &'a Value, k: &str) -> Result<&'a str, HkError> {
    p.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| bad(format!("missing `{k}`")))
}

fn internal(e: impl std::fmt::Display) -> HkError {
    HkError::internal(e.to_string())
}

/// JSON document kept in the settings table under `key` (`null` when unset).
async fn doc_get(store: &Store, key: &str) -> Result<Value, HkError> {
    Ok(store
        .get_setting(key)
        .await
        .map_err(internal)?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null))
}

async fn doc_set(store: &Store, key: &str, v: &Value) -> Result<Value, HkError> {
    store
        .set_setting(key, &v.to_string())
        .await
        .map_err(internal)?;
    Ok(v.clone())
}

/// Named-item list document (`[{id, ...}]`): upsert by `id` / delete by `id`.
async fn list_doc(store: &Store, key: &str, action: &str, p: &Value) -> Result<Value, HkError> {
    let mut items: Vec<Value> = match doc_get(store, key).await? {
        Value::Array(a) => a,
        _ => Vec::new(),
    };
    match action {
        "list" => {}
        "save" => {
            let item = p
                .get("item")
                .cloned()
                .ok_or_else(|| bad("missing `item`"))?;
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("item needs a string `id`"))?
                .to_string();
            items.retain(|i| i.get("id").and_then(Value::as_str) != Some(id.as_str()));
            items.push(item);
            doc_set(store, key, &Value::Array(items.clone())).await?;
        }
        "delete" => {
            let id = str_arg(p, "id")?;
            items.retain(|i| i.get("id").and_then(Value::as_str) != Some(id));
            doc_set(store, key, &Value::Array(items.clone())).await?;
        }
        other => return Err(bad(format!("unknown action `{other}`"))),
    }
    Ok(Value::Array(items))
}

fn jobs(rt: &LocalRuntime) -> Result<hk_core::JobRegistry, HkError> {
    rt.jobs()
        .ok_or_else(|| HkError::new(HkErrorKind::Unsupported, "jobs are not tracked"))
}

pub(crate) async fn call(c: &Ctx<'_>, method: &str, p: Value) -> Result<Value, HkError> {
    let (domain, action) = method.split_once('.').unwrap_or((method, ""));
    match domain {
        "jobs" => {
            let reg = jobs(c.runtime)?;
            match action {
                "list" => Ok(json!(reg.list())),
                "cancel" => {
                    let id = p
                        .get("id")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| bad("missing `id`"))?;
                    Ok(json!(reg.cancel(hk_core::JobId(id))))
                }
                "cancel_all" => Ok(json!(reg.cancel_all())),
                _ => Err(bad(format!("unknown `{method}`"))),
            }
        }
        "session" => match action {
            "pause" => {
                c.runtime
                    .pause(str_arg(&p, "session_id")?)
                    .await
                    .map_err(|e| HkError::internal(e.to_string()))?;
                Ok(Value::Null)
            }
            _ => crate::features::session(c, action, p).await,
        },
        "studio" => crate::studio::host()?
            .call(action, p)
            .await
            .map_err(crate::studio::studio_err),
        "memory" | "knowledge" => crate::features::recall(c, domain, action, p).await,
        "settings" => match action {
            "ai.get" => doc_get(c.store, "ai_settings").await,
            "ai.save" => {
                let v = doc_set(
                    c.store,
                    "ai_settings",
                    p.get("settings").unwrap_or(&Value::Null),
                )
                .await?;
                apply_ai_defaults(c.runtime, c.store).await;
                Ok(v)
            }
            "ai.presets" => Ok(json!(hk_core::builtin_presets())),
            "approvals.get" => doc_get(c.store, "approval_policy").await,
            "approvals.save" => {
                let v = doc_set(
                    c.store,
                    "approval_policy",
                    p.get("policy").unwrap_or(&Value::Null),
                )
                .await?;
                apply_ai_defaults(c.runtime, c.store).await;
                Ok(v)
            }
            "local.get" => doc_get(c.store, "local_runtime_profiles").await,
            "local.save" => {
                doc_set(
                    c.store,
                    "local_runtime_profiles",
                    p.get("profiles").unwrap_or(&Value::Null),
                )
                .await
            }
            _ => Err(bad(format!("unknown `{method}`"))),
        },
        "prompts" => list_doc(c.store, "prompt_library", action, &p).await,
        "agents" if action != "start" => list_doc(c.store, "custom_agents", action, &p).await,
        "schedules" => list_doc(c.store, "agent_schedules", action, &p).await,
        _ => crate::features::call(c, domain, action, p).await,
    }
}

pub(crate) fn events(
    rt: &LocalRuntime,
    topic: &str,
) -> Result<BoxStream<'static, String>, HkError> {
    match topic {
        "jobs" => {
            let rx = jobs(rt)?.subscribe();
            Ok(futures::stream::unfold(rx, |mut rx| async move {
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            return Some((serde_json::to_string(&ev).unwrap_or_default(), rx));
                        }
                        // A slow UI skips progress frames; `jobs.list` resyncs.
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => return None,
                    }
                }
            })
            .boxed())
        }
        other => Err(bad(format!("unknown topic `{other}`"))),
    }
}

/// Push the stored global AI settings and approval policy into the agent runtime.
pub(crate) async fn apply_ai_defaults(rt: &LocalRuntime, store: &Store) {
    let ai = doc_get(store, "ai_settings").await.unwrap_or(Value::Null);
    let settings: hk_core::AiSettings = ai
        .get("global")
        .cloned()
        .and_then(|g| serde_json::from_value(g).ok())
        .unwrap_or_default();
    // Reuse the provider-facing mapping so UI fields and wire fields never drift.
    let mut probe = hk_llm::ChatRequest::new(String::new(), Vec::new());
    probe.apply_settings(&settings);
    let approvals = doc_get(store, "approval_policy").await.unwrap_or(Value::Null);
    let dry_run = approvals.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
    let policy: std::collections::BTreeMap<String, String> = approvals
        .get("rules")
        .cloned()
        .and_then(|r| serde_json::from_value(r).ok())
        .unwrap_or_default();
    rt.set_ai_defaults(hk_server::AiDefaults {
        sampling: probe.sampling,
        temperature: probe.temperature,
        max_output_tokens: probe.max_output_tokens,
        reasoning_effort: probe.reasoning_effort,
        policy,
        dry_run,
    });
}
