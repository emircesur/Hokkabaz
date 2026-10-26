//! Phase 9 feature handlers reached through the generic gate ([`crate::gate`]).

use std::collections::BTreeMap;
use std::time::Instant;

use futures::StreamExt;
use hk_llm::{CancellationToken, ChatRequest, Message, Role, StreamEvent};
use hk_server::AgentRuntime;
use hk_session::EventKind;
use serde_json::{Value, json};

use crate::error::HkError;
use crate::gate::{Ctx, bad, str_arg};

fn internal(e: impl std::fmt::Display) -> HkError {
    HkError::internal(e.to_string())
}

fn user(text: &str) -> Message {
    Message { images: Vec::new(),
        role: Role::User,
        content: text.to_string(),
        reasoning: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
    }
}

pub(crate) async fn session(c: &Ctx<'_>, action: &str, p: Value) -> Result<Value, HkError> {
    match action {
        // Context inspector (P9.18): what the next request carries, with token estimates.
        "context" => {
            let d = c
                .runtime
                .session(str_arg(&p, "session_id")?, None)
                .await
                .map_err(internal)?;
            let rows: Vec<Value> = d
                .messages
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let chars = m.content.len() + m.reasoning.as_ref().map_or(0, String::len);
                    json!({
                        "index": i,
                        "role": m.role,
                        "tokens": chars / 4,
                        "tool_calls": m.tool_calls.len(),
                        "preview": m.content.chars().take(200).collect::<String>(),
                    })
                })
                .collect();
            let total: usize = rows
                .iter()
                .filter_map(|r| r["tokens"].as_u64())
                .sum::<u64>() as usize;
            Ok(json!({"messages": rows, "total_tokens": total}))
        }
        // Submit with attached images (P9.15): paths are read here and sent inline.
        "submit" => {
            use base64::Engine as _;
            let mut images = Vec::new();
            for path in p.get("images").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                let bytes = tokio::fs::read(path).await.map_err(|e| bad(format!("{path}: {e}")))?;
                if bytes.len() > 20 * 1024 * 1024 {
                    return Err(bad(format!("{path} is larger than 20 MB")));
                }
                let ext = path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
                let mime = match ext.as_str() {
                    "jpg" | "jpeg" => "image/jpeg",
                    "webp" => "image/webp",
                    "gif" => "image/gif",
                    _ => "image/png",
                };
                images.push(hk_llm::ImagePart {
                    mime: mime.into(),
                    data: base64::engine::general_purpose::STANDARD.encode(bytes),
                });
            }
            let r = c
                .runtime
                .submit(
                    str_arg(&p, "session_id")?,
                    hk_server::TurnRequest {
                        prompt: str_arg(&p, "prompt")?.to_string(),
                        mode: None,
                        model: p.get("model").and_then(Value::as_str).map(str::to_string),
                        images,
                    },
                )
                .await
                .map_err(internal)?;
            Ok(json!(r))
        }
        _ => Err(bad(format!("unknown `session.{action}`"))),
    }
}

pub(crate) async fn recall(
    c: &Ctx<'_>,
    domain: &str,
    action: &str,
    p: Value,
) -> Result<Value, HkError> {
    use hk_tools::builtin::{KnowledgeBase, MemoryStore};
    let dir = c.data_dir.to_path_buf();
    let action = action.to_string();
    let domain = domain.to_string();
    tokio::task::spawn_blocking(move || -> Result<Value, HkError> {
        if domain == "memory" {
            let m = MemoryStore::new(&dir);
            let project = p.get("project").and_then(Value::as_str);
            return match action.as_str() {
                "list" => Ok(json!(m.all())),
                "save" => {
                    let scope = match project {
                        Some(pr) => format!("project:{pr}"),
                        None => "user".into(),
                    };
                    Ok(json!(m.save(&scope, str_arg(&p, "text")?).map_err(internal)?))
                }
                "search" => Ok(json!(m.search(str_arg(&p, "query")?, project, 10))),
                "delete" => {
                    let id = p.get("id").and_then(Value::as_u64).ok_or_else(|| bad("missing `id`"))?;
                    Ok(json!(m.delete(id).map_err(internal)?))
                }
                _ => Err(bad(format!("unknown `memory.{action}`"))),
            };
        }
        let kb = KnowledgeBase::new(&dir);
        match action.as_str() {
            "list" => Ok(json!(kb.list())),
            "index" => {
                let (files, chunks) = kb
                    .index(str_arg(&p, "name")?, std::path::Path::new(str_arg(&p, "path")?))
                    .map_err(internal)?;
                Ok(json!({"files": files, "chunks": chunks}))
            }
            "search" => {
                let bases: Vec<String> = p
                    .get("bases")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                Ok(json!(
                    kb.search(str_arg(&p, "query")?, &bases, 10)
                        .into_iter()
                        .map(|(b, ch)| json!({"base": b, "cite": format!("{}:{}-{}", ch.file, ch.start_line, ch.end_line), "text": ch.text}))
                        .collect::<Vec<_>>()
                ))
            }
            "remove" => Ok(json!(kb.remove(str_arg(&p, "name")?))),
            _ => Err(bad(format!("unknown `knowledge.{action}`"))),
        }
    })
    .await
    .map_err(internal)?
}

/// Usage & cost (P9.13): summed from session logs, grouped by day and model.
async fn usage_summary(c: &Ctx<'_>, days: i64) -> Result<Value, HkError> {
    let since = chrono_ms() - days * 86_400_000;
    let prices: BTreeMap<String, hk_llm::ModelCost> = c
        .provider
        .models()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| Some((format!("{}/{}", m.provider, m.id), m.cost?)))
        .collect();
    let mut by_day: BTreeMap<String, Value> = BTreeMap::new();
    let mut by_model: BTreeMap<String, (u64, u64, f64)> = BTreeMap::new();
    let sessions = c.runtime.list_sessions(500, 0).await.map_err(internal)?;
    for s in sessions.iter().filter(|s| s.updated_ms >= since) {
        let model = s.model.clone().unwrap_or_else(|| "unknown".into());
        let d = c.runtime.session(&s.id, None).await.map_err(internal)?;
        for e in &d.events {
            let EventKind::StepEnd { usage: Some(u), .. } = &e.kind else {
                continue;
            };
            if e.ts_ms < since {
                continue;
            }
            let cost = prices.get(&model).map_or(0.0, |p| {
                (u.input_tokens as f64 * p.input
                    + u.output_tokens as f64 * p.output
                    + u.cache_read_tokens as f64 * p.cache_read.unwrap_or(p.input))
                    / 1_000_000.0
            });
            let day = day_key(e.ts_ms);
            let slot = by_day
                .entry(day.clone())
                .or_insert_with(|| json!({"day": day, "input": 0, "output": 0, "cost_usd": 0.0}));
            slot["input"] = json!(slot["input"].as_u64().unwrap_or(0) + u.total_input());
            slot["output"] = json!(slot["output"].as_u64().unwrap_or(0) + u.output_tokens);
            slot["cost_usd"] = json!(slot["cost_usd"].as_f64().unwrap_or(0.0) + cost);
            let m = by_model.entry(model.clone()).or_default();
            m.0 += u.total_input();
            m.1 += u.output_tokens;
            m.2 += cost;
        }
    }
    Ok(json!({
        "days": by_day.into_values().collect::<Vec<_>>(),
        "models": by_model.into_iter().map(|(k, (i, o, c))| json!({"model": k, "input": i, "output": o, "cost_usd": c})).collect::<Vec<_>>(),
    }))
}

fn chrono_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// `YYYY-MM-DD` (UTC) from unix ms, civil-from-days.
fn day_key(ms: i64) -> String {
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Model arena (P9.11): same prompt to several models concurrently.
pub(crate) async fn arena(
    c: &Ctx<'_>,
    prompt: &str,
    models: Vec<String>,
) -> Result<Value, HkError> {
    let runs = models.into_iter().take(6).map(|model| {
        let provider = c.provider.clone();
        let prompt = prompt.to_string();
        async move {
            let started = Instant::now();
            let mut first = None;
            let mut text = String::new();
            let mut usage = None;
            let req = ChatRequest::new(model.clone(), vec![user(&prompt)]);
            let res = provider.stream(req, CancellationToken::new()).await;
            let mut err = None;
            match res {
                Ok(mut s) => {
                    while let Some(ev) = s.next().await {
                        match ev {
                            Ok(StreamEvent::TextDelta { text: t }) => {
                                first.get_or_insert_with(|| started.elapsed());
                                text.push_str(&t);
                            }
                            Ok(StreamEvent::Usage(u)) => usage = Some(u),
                            Ok(StreamEvent::Finish(_)) => break,
                            Ok(_) => {}
                            Err(e) => {
                                err = Some(e.to_string());
                                break;
                            }
                        }
                    }
                }
                Err(e) => err = Some(e.to_string()),
            }
            json!({
                "model": model,
                "text": text,
                "error": err,
                "ttft_ms": first.map(|d| d.as_millis() as u64),
                "total_ms": started.elapsed().as_millis() as u64,
                "usage": usage,
            })
        }
    });
    Ok(json!(futures::future::join_all(runs).await))
}

pub(crate) async fn call(
    c: &Ctx<'_>,
    domain: &str,
    action: &str,
    p: Value,
) -> Result<Value, HkError> {
    match (domain, action) {
        // Start a chat as a custom agent (P9.21): its model and system prompt.
        ("agents", "start") => {
            let id = str_arg(&p, "id")?;
            let doc: Vec<Value> = c
                .store
                .get_setting("custom_agents")
                .await
                .map_err(internal)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            let a = doc
                .into_iter()
                .find(|a| a["id"] == id)
                .ok_or_else(|| bad(format!("unknown agent `{id}`")))?;
            let model = a["model"].as_str().filter(|m| !m.is_empty() && !m.contains("{{")).map(str::to_string);
            let info = c
                .runtime
                .create_session(hk_server::CreateSession {
                    cwd: None,
                    model,
                    title: a["name"].as_str().map(|n| format!("🤖 {n}")),
                })
                .await
                .map_err(internal)?;
            let mut system = vec![a["system_prompt"].as_str().unwrap_or_default().to_string()];
            if let Some(tools) = a["tools"].as_array().filter(|t| !t.is_empty()) {
                let names: Vec<&str> = tools.iter().filter_map(Value::as_str).collect();
                system.push(format!("Prefer these tools for this role: {}.", names.join(", ")));
            }
            c.runtime.set_session_system(&info.id, system).map_err(internal)?;
            Ok(json!(info))
        }
        ("usage", "summary") => {
            usage_summary(c, p.get("days").and_then(Value::as_i64).unwrap_or(30)).await
        }
        ("arena", "run") => {
            let models: Vec<String> = p
                .get("models")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .ok_or_else(|| bad("missing `models`"))?;
            arena(c, str_arg(&p, "prompt")?, models).await
        }
        _ => crate::features2::call(c, domain, action, p).await,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn day_keys() {
        assert_eq!(super::day_key(0), "1970-01-01");
        assert_eq!(super::day_key(1_792_972_800_000), "2026-10-26");
    }
}
