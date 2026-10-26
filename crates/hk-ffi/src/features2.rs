//! Gate handlers: speech/image generation, checkpoints, templates, benchmarks.

use std::path::PathBuf;

use serde_json::{Map, Value, json};

use crate::error::HkError;
use crate::gate::{Ctx, bad, str_arg};

fn internal(e: impl std::fmt::Display) -> HkError {
    HkError::internal(e.to_string())
}

/// Built-in template catalog shipped with the app (works offline). Same manifests as the
/// GitHub registry's `templates/` folder.
const STARTER: &str = include_str!("../assets/starter_templates.json");

fn starter() -> Vec<Value> {
    serde_json::from_str::<Vec<Value>>(STARTER).unwrap_or_default()
}

async fn genai_cfg(c: &Ctx<'_>, key: &str) -> Result<Value, HkError> {
    let doc: Value = c
        .store
        .get_setting("genai_settings")
        .await
        .map_err(internal)?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    doc.get(key)
        .cloned()
        .filter(|v| !v.is_null())
        .ok_or_else(|| bad(format!("configure `{key}` in Settings → Generation first")))
}

/// `{{inputs.key}}` placeholder substitution over a JSON payload.
fn render(v: &Value, inputs: &Map<String, Value>) -> Value {
    match v {
        Value::String(s) => {
            for (k, val) in inputs {
                let ph = format!("{{{{inputs.{k}}}}}");
                if *s == ph {
                    return val.clone();
                }
            }
            let mut out = s.clone();
            for (k, val) in inputs {
                let text = val.as_str().map_or_else(|| val.to_string(), str::to_string);
                out = out.replace(&format!("{{{{inputs.{k}}}}}"), &text);
            }
            Value::String(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| render(x, inputs)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), render(x, inputs)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(crate) async fn call(
    c: &Ctx<'_>,
    domain: &str,
    action: &str,
    p: Value,
) -> Result<Value, HkError> {
    match (domain, action) {
        ("genai", "get") => Ok(c
            .store
            .get_setting("genai_settings")
            .await
            .map_err(internal)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null)),
        ("genai", "save") => {
            let v = p.get("settings").cloned().unwrap_or(Value::Null);
            c.store
                .set_setting("genai_settings", &v.to_string())
                .await
                .map_err(internal)?;
            apply_embedder(c.store).await;
            Ok(v)
        }
        ("tts", "speak") => {
            let cfg: hk_genai::TtsConfig = serde_json::from_value(genai_cfg(c, "tts").await?)
                .map_err(|e| bad(e.to_string()))?;
            let out = c
                .data_dir
                .join("genai")
                .join(format!("tts_{}.mp3", now_ms()));
            hk_genai::speak(&cfg, str_arg(&p, "text")?, &out)
                .await
                .map_err(internal)?;
            Ok(json!(out.to_string_lossy()))
        }
        ("image", "generate") => {
            let cfg: hk_genai::ImageConfig = serde_json::from_value(genai_cfg(c, "image").await?)
                .map_err(|e| bad(e.to_string()))?;
            let req: hk_genai::ImageRequest =
                serde_json::from_value(p.clone()).map_err(|e| bad(e.to_string()))?;
            let out = c
                .data_dir
                .join("genai")
                .join(format!("img_{}.png", now_ms()));
            hk_genai::generate_image(&cfg, &req, &out)
                .await
                .map_err(internal)?;
            Ok(json!(out.to_string_lossy()))
        }
        ("checkpoints", a) => {
            let cps = hk_tools::builtin::Checkpoints::new(c.data_dir);
            let tree = PathBuf::from(str_arg(&p, "path")?);
            let label = p
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or("manual")
                .to_string();
            let id = p
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let a = a.to_string();
            tokio::task::spawn_blocking(move || -> Result<Value, HkError> {
                match a.as_str() {
                    "list" => Ok(json!(cps.list(&tree).map_err(internal)?)),
                    "create" => Ok(json!(cps.create(&tree, &label).map_err(internal)?)),
                    "restore" => {
                        cps.restore(&tree, &id).map_err(internal)?;
                        Ok(Value::Null)
                    }
                    other => Err(bad(format!("unknown `checkpoints.{other}`"))),
                }
            })
            .await
            .map_err(internal)?
        }
        ("templates", "list") => {
            let mut all = starter();
            if p.get("remote").and_then(Value::as_bool) == Some(true)
                && let Ok(idx) =
                    hk_plugin::registry::fetch_index(hk_plugin::registry::DEFAULT_INDEX_URL).await
            {
                for t in idx.templates {
                    all.push(json!({"id": t.id, "name": t.name, "version": t.version, "kind": t.kind, "description": t.description, "remote": true}));
                }
            }
            Ok(json!(all))
        }
        ("templates", "install") => {
            let id = str_arg(&p, "id")?;
            let t = starter()
                .into_iter()
                .find(|t| t["id"] == id)
                .ok_or_else(|| bad(format!("unknown template `{id}`")))?;
            let inputs = p
                .get("inputs")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for i in t["inputs"].as_array().into_iter().flatten() {
                let key = i["key"].as_str().unwrap_or_default();
                if i["required"].as_bool() == Some(true)
                    && !inputs.contains_key(key)
                    && i.get("default").is_none()
                {
                    return Err(bad(format!("input `{key}` is required")));
                }
            }
            let mut filled = inputs.clone();
            for i in t["inputs"].as_array().into_iter().flatten() {
                if let (Some(k), Some(d)) = (i["key"].as_str(), i.get("default"))
                    && !filled.contains_key(k)
                {
                    filled.insert(k.to_string(), d.clone());
                }
            }
            let payload = render(&t["payload"], &filled);
            let kind = t["kind"].as_str().unwrap_or_default();
            // Flows are saved by the app's flow store; the rest land in settings documents.
            let doc = match kind {
                "agent" => Some("custom_agents"),
                "prompt" => Some("prompt_library"),
                "settings_preset" => Some("ai_presets_user"),
                _ => None,
            };
            if let Some(key) = doc {
                let mut items: Vec<Value> = c
                    .store
                    .get_setting(key)
                    .await
                    .map_err(internal)?
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                let mut item = payload.clone();
                if item.get("id").is_none() {
                    item["id"] = json!(id);
                }
                items.retain(|x| x.get("id") != item.get("id"));
                items.push(item);
                c.store
                    .set_setting(key, &Value::Array(items).to_string())
                    .await
                    .map_err(internal)?;
            }
            if kind == "studio" {
                let dir = c.data_dir.join("studio").join("templates");
                tokio::fs::create_dir_all(&dir).await.map_err(internal)?;
                tokio::fs::write(dir.join(format!("{id}.json")), payload.to_string())
                    .await
                    .map_err(internal)?;
            }
            Ok(json!({"kind": kind, "name": t["name"], "payload": payload}))
        }
        ("local", "benchmark") => {
            let model = str_arg(&p, "model")?.to_string();
            let prompt = "Write a 150-word paragraph about the history of juggling.";
            let r = crate::features::arena(c, prompt, vec![model]).await?;
            let row = r.get(0).cloned().unwrap_or(Value::Null);
            let out = row["usage"]["output_tokens"]
                .as_f64()
                .unwrap_or_else(|| row["text"].as_str().map_or(0.0, |t| t.len() as f64 / 4.0));
            let gen_ms =
                row["total_ms"].as_f64().unwrap_or(0.0) - row["ttft_ms"].as_f64().unwrap_or(0.0);
            Ok(json!({
                "model": row["model"], "error": row["error"], "ttft_ms": row["ttft_ms"],
                "tokens_per_s": if gen_ms > 0.0 { (out / (gen_ms / 1000.0) * 10.0).round() / 10.0 } else { 0.0 },
            }))
        }
        ("posts", a) => {
            // Publish calendar entries (P9.39); the scheduler in `local.rs` fires due ones.
            let key = "post_schedules";
            let mut items: Vec<Value> = c
                .store
                .get_setting(key)
                .await
                .map_err(internal)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            match a {
                "list" => {}
                "save" => {
                    let item = p
                        .get("item")
                        .cloned()
                        .ok_or_else(|| bad("missing `item`"))?;
                    items.retain(|x| x.get("id") != item.get("id"));
                    items.push(item);
                }
                "delete" => {
                    let id = str_arg(&p, "id")?;
                    items.retain(|x| x["id"] != id);
                }
                other => return Err(bad(format!("unknown `posts.{other}`"))),
            }
            c.store
                .set_setting(key, &Value::Array(items.clone()).to_string())
                .await
                .map_err(internal)?;
            Ok(Value::Array(items))
        }
        _ => Err(bad(format!("unknown `{domain}.{action}`"))),
    }
}

struct GenEmbedder(hk_genai::EmbedConfig);

impl hk_tools::builtin::Embedder for GenEmbedder {
    fn embed(
        &self,
        texts: Vec<String>,
    ) -> futures::future::BoxFuture<'_, Result<Vec<Vec<f32>>, String>> {
        Box::pin(async move {
            hk_genai::embed(&self.0, &texts)
                .await
                .map_err(|e| e.to_string())
        })
    }
}

/// Turn knowledge-base hybrid search on/off from `genai_settings.embed`.
pub(crate) async fn apply_embedder(store: &hk_store::Store) {
    let cfg: Option<hk_genai::EmbedConfig> = store
        .get_setting("genai_settings")
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("embed").cloned())
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value(v).ok());
    hk_tools::builtin::set_embedder(cfg.map(|c| {
        std::sync::Arc::new(GenEmbedder(c)) as std::sync::Arc<dyn hk_tools::builtin::Embedder>
    }));
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_catalog_parses_and_renders() {
        let all = starter();
        assert!(all.len() >= 25, "{}", all.len());
        let mut m = Map::new();
        m.insert("chat_id".into(), json!(42));
        let v = render(
            &json!({"a": "{{inputs.chat_id}}", "b": "id={{inputs.chat_id}}"}),
            &m,
        );
        assert_eq!(v, json!({"a": 42, "b": "id=42"}));
    }
}
