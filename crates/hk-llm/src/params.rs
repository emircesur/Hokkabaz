//! Layered AI settings (hk-core, DESIGN §3.15) -> [`ChatRequest`] knobs, plus the
//! per-provider support table adapters use to drop what the target can't take.
//!
//! Parameter keys are `hk_core::ai_settings::AiSettings` field names, so the
//! UI's capability check and the adapters' drop list speak the same language.

use std::borrow::Cow;

use hk_core::ai_settings::{self as core, AiSettings, ModelCapsLike, Resolved};

use crate::registry::ProviderKind;
use crate::{ChatRequest, ModelInfo, ReasoningEffort, SamplingParams};

impl From<core::ReasoningEffort> for ReasoningEffort {
    fn from(e: core::ReasoningEffort) -> Self {
        match e {
            core::ReasoningEffort::Low => Self::Low,
            core::ReasoningEffort::Medium => Self::Medium,
            core::ReasoningEffort::High => Self::High,
        }
    }
}

impl From<&AiSettings> for SamplingParams {
    fn from(s: &AiSettings) -> Self {
        Self {
            top_p: s.top_p,
            top_k: s.top_k,
            min_p: s.min_p,
            presence_penalty: s.presence_penalty,
            frequency_penalty: s.frequency_penalty,
            repetition_penalty: s.repetition_penalty,
            seed: s.seed,
            stop: s.stop.clone().unwrap_or_default(),
            logit_bias: s.logit_bias.clone().unwrap_or_default(),
            thinking_budget: s.thinking_budget,
            json_mode: s.json_mode.unwrap_or(false),
            json_schema: s.json_schema.clone(),
        }
    }
}

impl From<&Resolved> for SamplingParams {
    fn from(r: &Resolved) -> Self {
        r.settings().into()
    }
}

impl ChatRequest {
    /// Overlay every model parameter set in `s`; unset fields keep their value.
    /// Harness-only fields are ignored, as is `stream` (adapters always stream).
    pub fn apply_settings(&mut self, s: &AiSettings) {
        if let Some(t) = s.temperature {
            // Temperatures live in 0..=2; f32 is plenty.
            #[allow(clippy::cast_possible_truncation)]
            {
                self.temperature = Some(t as f32);
            }
        }
        if let Some(n) = s.max_output_tokens {
            self.max_output_tokens = Some(u64::from(n));
        }
        if let Some(e) = s.reasoning_effort {
            self.reasoning_effort = Some(e.into());
        }
        let from = SamplingParams::from(s);
        let p = &mut self.sampling;
        macro_rules! overlay_opt {
            ($($f:ident),*) => { $( if from.$f.is_some() { p.$f = from.$f; } )* };
        }
        overlay_opt!(
            top_p,
            top_k,
            min_p,
            presence_penalty,
            frequency_penalty,
            repetition_penalty,
            seed,
            thinking_budget,
            json_schema
        );
        if s.stop.is_some() {
            p.stop = from.stop;
        }
        if s.logit_bias.is_some() {
            p.logit_bias = from.logit_bias;
        }
        if let Some(j) = s.json_mode {
            p.json_mode = j;
        }
    }

    /// `self` with the resolved settings stack applied.
    #[must_use]
    pub fn with_settings(mut self, r: &Resolved) -> Self {
        self.apply_settings(r.settings());
        self
    }

    /// Keys of the model parameters set on this request.
    pub fn set_params(&self) -> Vec<&'static str> {
        let p = &self.sampling;
        [
            ("temperature", self.temperature.is_some()),
            ("top_p", p.top_p.is_some()),
            ("top_k", p.top_k.is_some()),
            ("min_p", p.min_p.is_some()),
            ("max_output_tokens", self.max_output_tokens.is_some()),
            ("presence_penalty", p.presence_penalty.is_some()),
            ("frequency_penalty", p.frequency_penalty.is_some()),
            ("repetition_penalty", p.repetition_penalty.is_some()),
            ("seed", p.seed.is_some()),
            ("stop", !p.stop.is_empty()),
            ("logit_bias", !p.logit_bias.is_empty()),
            ("reasoning_effort", self.reasoning_effort.is_some()),
            ("thinking_budget", p.thinking_budget.is_some()),
            ("json_mode", p.json_mode),
            ("json_schema", p.json_schema.is_some()),
        ]
        .into_iter()
        .filter_map(|(k, set)| set.then_some(k))
        .collect()
    }

    fn clear_param(&mut self, key: &str) {
        let p = &mut self.sampling;
        match key {
            "temperature" => self.temperature = None,
            "top_p" => p.top_p = None,
            "top_k" => p.top_k = None,
            "min_p" => p.min_p = None,
            "max_output_tokens" => self.max_output_tokens = None,
            "presence_penalty" => p.presence_penalty = None,
            "frequency_penalty" => p.frequency_penalty = None,
            "repetition_penalty" => p.repetition_penalty = None,
            "seed" => p.seed = None,
            "stop" => p.stop.clear(),
            "logit_bias" => p.logit_bias.clear(),
            "reasoning_effort" => self.reasoning_effort = None,
            "thinking_budget" => p.thinking_budget = None,
            "json_mode" => p.json_mode = false,
            "json_schema" => p.json_schema = None,
            _ => {}
        }
    }
}

/// `req` minus the params `supports` rejects. Dropped keys are logged at
/// debug; borrows when nothing is dropped.
pub(crate) fn retain_supported<'a>(
    provider: &str,
    req: &'a ChatRequest,
    supports: impl Fn(&str) -> bool,
) -> Cow<'a, ChatRequest> {
    let dropped: Vec<&'static str> = req
        .set_params()
        .into_iter()
        .filter(|k| !supports(k))
        .collect();
    if dropped.is_empty() {
        return Cow::Borrowed(req);
    }
    tracing::debug!(
        provider,
        model = %req.model,
        ?dropped,
        "dropping request params the provider does not support"
    );
    let mut out = req.clone();
    for k in &dropped {
        out.clear_param(k);
    }
    Cow::Owned(out)
}

/// Whether an adapter of `kind`, with its default options, sends `key` for
/// `model`. Request-dependent rules (Anthropic drops sampling while thinking)
/// are applied at build time on top of this.
pub fn supports_param(kind: ProviderKind, model: &str, key: &str) -> bool {
    match kind {
        ProviderKind::Deepseek => {
            crate::openai::request::supports(&crate::deepseek::body_options(), model, key)
        }
        ProviderKind::OpenaiCompat => {
            crate::openai::request::supports(&crate::openai::BodyOptions::default(), model, key)
        }
        ProviderKind::Anthropic => crate::anthropic::request::supports_static(model, key),
        ProviderKind::Gemini => crate::gemini::request::supports(model, key),
        ProviderKind::Ollama => crate::ollama::request::supports(key),
    }
}

/// Catalog view: unknown provider ids are treated as openai-compat (custom
/// endpoints), and reasoning knobs need the models.dev `reasoning` flag.
impl ModelCapsLike for ModelInfo {
    fn supports(&self, field: &str) -> bool {
        match field {
            "stream" => true,
            "reasoning_effort" | "thinking_budget" if !self.capabilities.reasoning => false,
            _ => {
                let kind =
                    ProviderKind::parse(&self.provider).unwrap_or(ProviderKind::OpenaiCompat);
                supports_param(kind, &self.id, field)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use hk_core::ai_settings::{Scope, SettingsStack};
    use serde_json::json;

    use super::*;
    use crate::Capabilities;

    fn info(provider: &str, id: &str, reasoning: bool) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            provider: provider.into(),
            name: id.into(),
            context_window: 0,
            max_output: None,
            capabilities: Capabilities {
                tools: true,
                reasoning,
                vision: false,
            },
            cost: None,
        }
    }

    #[test]
    fn resolved_stack_maps_onto_request() {
        let mut stack = SettingsStack::new();
        stack.push(
            Scope::Global,
            AiSettings {
                temperature: Some(0.5),
                top_k: Some(40),
                stop: Some(vec!["END".into()]),
                ..Default::default()
            },
        );
        stack.push(
            Scope::Message,
            AiSettings {
                temperature: Some(0.25),
                reasoning_effort: Some(core::ReasoningEffort::High),
                max_output_tokens: Some(512),
                logit_bias: Some(BTreeMap::from([("42".into(), -100.0)])),
                json_schema: Some(json!({"type": "object"})),
                compaction_threshold: Some(0.5),
                ..Default::default()
            },
        );
        let mut req = ChatRequest::new("m", vec![]);
        req.sampling.seed = Some(7); // set by caller, not overridden by unset settings
        let req = req.with_settings(&stack.resolved());
        assert_eq!(req.temperature, Some(0.25));
        assert_eq!(req.max_output_tokens, Some(512));
        assert_eq!(req.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(req.sampling.top_k, Some(40));
        assert_eq!(req.sampling.seed, Some(7));
        assert_eq!(req.sampling.stop, vec!["END".to_string()]);
        assert_eq!(req.sampling.logit_bias["42"], -100.0);
        assert_eq!(
            req.set_params(),
            vec![
                "temperature",
                "top_k",
                "max_output_tokens",
                "seed",
                "stop",
                "logit_bias",
                "reasoning_effort",
                "json_schema"
            ]
        );
    }

    #[test]
    fn retain_drops_and_borrows() {
        let mut req = ChatRequest::new("m", vec![]);
        req.temperature = Some(0.1);
        req.sampling.min_p = Some(0.05);
        let kept = retain_supported("p", &req, |_| true);
        assert!(matches!(kept, Cow::Borrowed(_)));
        let out = retain_supported("p", &req, |k| k != "min_p");
        assert_eq!(out.sampling.min_p, None);
        assert_eq!(out.temperature, Some(0.1));
    }

    #[test]
    fn model_info_caps_follow_adapter_tables() {
        let reasoner = info("deepseek", "deepseek-reasoner", true);
        assert!(!reasoner.supports("temperature"));
        assert!(reasoner.supports("max_output_tokens"));
        assert!(!reasoner.supports("seed"));
        let chat = info("deepseek", "deepseek-chat", false);
        assert!(chat.supports("temperature") && chat.supports("frequency_penalty"));
        assert!(!chat.supports("reasoning_effort"));

        let gpt = info("openai", "gpt-5", true);
        assert!(gpt.supports("logit_bias") && gpt.supports("json_schema"));
        assert!(gpt.supports("reasoning_effort"));
        assert!(!gpt.supports("top_k") && !gpt.supports("thinking_budget"));

        let legacy = info("anthropic", "claude-sonnet-4-5", true);
        assert!(legacy.supports("top_k") && legacy.supports("thinking_budget"));
        let adaptive = info("anthropic", "claude-opus-5-5", true);
        assert!(!adaptive.supports("temperature") && !adaptive.supports("thinking_budget"));
        assert!(adaptive.supports("reasoning_effort") && adaptive.supports("json_schema"));
        assert!(!adaptive.supports("seed"));

        let gem = info("google", "gemini-2.5-flash", true);
        assert!(gem.supports("top_k") && gem.supports("seed") && gem.supports("thinking_budget"));
        assert!(!gem.supports("logit_bias") && !gem.supports("min_p"));

        let local = info("ollama", "qwen3", true);
        assert!(local.supports("min_p") && local.supports("repetition_penalty"));
        assert!(!local.supports("logit_bias"));
        assert!(local.supports("stream"));

        // Filtering through hk-core agrees with the table.
        let s = AiSettings {
            top_k: Some(5),
            temperature: Some(0.2),
            show_reasoning: Some(core::ReasoningVisibility::Hide),
            ..Default::default()
        };
        let (kept, dropped) = s.retain_supported(&gpt);
        assert_eq!(dropped, vec!["top_k"]);
        assert_eq!(kept.temperature, Some(0.2));
    }
}
