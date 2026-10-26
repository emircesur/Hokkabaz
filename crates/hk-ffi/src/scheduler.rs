//! Background scheduler (P9.20 scheduled agents, P9.39 publish calendar). Ticks every
//! minute over two settings documents; each fired item becomes a normal session / publish
//! call, so it shows up in the job tree and the Runs screen like manual work.

use std::time::Duration;

use hk_server::{AgentRuntime, CreateSession, LocalRuntime, TurnRequest};
use hk_store::Store;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn now_ms() -> u64 {
    crate::features2::now_ms()
}

async fn load(store: &Store, key: &str) -> Vec<Value> {
    store
        .get_setting(key)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Agent schedule `{id, name, prompt, model?, every_minutes, enabled, last_run_ms?}` is due.
pub(crate) fn agent_due(item: &Value, now: u64) -> bool {
    let every = item["every_minutes"].as_u64().unwrap_or(0);
    item["enabled"].as_bool().unwrap_or(true)
        && every > 0
        && now.saturating_sub(item["last_run_ms"].as_u64().unwrap_or(0)) >= every * 60_000
}

/// Post `{id, at_ms, project, clip, privacy?, done?}` is due.
pub(crate) fn post_due(item: &Value, now: u64) -> bool {
    !item["done"].as_bool().unwrap_or(false) && item["at_ms"].as_u64().is_some_and(|t| t <= now)
}

pub(crate) fn spawn(runtime: LocalRuntime, store: Store, stop: CancellationToken) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(Duration::from_secs(60)) => {}
            }
            let now = now_ms();
            let mut agents = load(&store, "agent_schedules").await;
            let mut changed = false;
            for a in agents.iter_mut().filter(|a| agent_due(a, now)) {
                a["last_run_ms"] = json!(now);
                changed = true;
                let title = format!("⏰ {}", a["name"].as_str().unwrap_or("scheduled"));
                let model = a["model"].as_str().map(str::to_string);
                let prompt = a["prompt"].as_str().unwrap_or_default().to_string();
                match runtime
                    .create_session(CreateSession {
                        cwd: None,
                        model: model.clone(),
                        title: Some(title),
                    })
                    .await
                {
                    Ok(s) => {
                        let r = runtime
                            .submit(
                                &s.id,
                                TurnRequest { images: Vec::new(),
                                    prompt,
                                    mode: None,
                                    model,
                                },
                            )
                            .await;
                        if let Err(e) = r {
                            tracing::warn!(error = %e, "scheduled agent: submit failed");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "scheduled agent: create failed"),
                }
            }
            if changed {
                let _ = store
                    .set_setting("agent_schedules", &Value::Array(agents).to_string())
                    .await;
            }

            let mut posts = load(&store, "post_schedules").await;
            let mut changed = false;
            for p in posts.iter_mut().filter(|p| post_due(p, now)) {
                p["done"] = json!(true);
                changed = true;
                let Ok(host) = crate::studio::host() else {
                    continue;
                };
                let args = json!({"id": p["project"], "clip": p["clip"], "privacy": p["privacy"]});
                match host.call("publish_youtube", args).await {
                    Ok(v) => p["result"] = v,
                    Err(e) => p["error"] = json!(e.to_string()),
                }
            }
            if changed {
                let _ = store
                    .set_setting("post_schedules", &Value::Array(posts).to_string())
                    .await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_rules() {
        assert!(agent_due(&json!({"every_minutes": 60}), 3_600_000));
        assert!(!agent_due(
            &json!({"every_minutes": 60, "last_run_ms": 1_000}),
            3_600_000
        ));
        assert!(!agent_due(
            &json!({"every_minutes": 60, "enabled": false}),
            9_999_999
        ));
        assert!(post_due(&json!({"at_ms": 5}), 10));
        assert!(!post_due(&json!({"at_ms": 5, "done": true}), 10));
    }
}
