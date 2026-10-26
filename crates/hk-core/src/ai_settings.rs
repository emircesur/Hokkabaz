//! Layered AI settings (DESIGN §3.15, D18): the same knobs from global down to a single
//! message, resolved with per-field provenance so the UI can say "inherited from …".
// port: design §3.15 (dsh has flat per-model settings; layering + provenance is Hokkabaz-native)

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Reasoning effort hint for models that take a coarse knob instead of a token budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}

/// How reasoning text is presented. Display-only; never changes the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningVisibility {
    Show,
    Collapse,
    Hide,
}

/// Generates `AiSettings` plus the field table that merge / provenance / capability
/// filtering walk, so adding a knob is one line and can't drift out of any of them.
/// `model` fields are request parameters a model may not support; `harness` fields are
/// applied by Hokkabaz itself and are never dropped by capability filtering.
macro_rules! ai_settings {
    ($( $(#[$m:meta])* [$kind:ident] $name:ident : $ty:ty ),* $(,)?) => {
        /// One layer of AI settings. Every field is optional: `None` means "inherit".
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        #[serde(default, rename_all = "snake_case")]
        pub struct AiSettings {
            $( $(#[$m])* #[serde(skip_serializing_if = "Option::is_none")] pub $name: Option<$ty>, )*
        }

        impl AiSettings {
            /// All field names, in declaration order.
            pub const FIELDS: &'static [&'static str] = &[$(stringify!($name)),*];

            /// Whether `field` is a model request parameter (subject to capability filtering).
            pub fn is_model_param(field: &str) -> bool {
                match field {
                    $( stringify!($name) => ai_settings!(@is_model $kind), )*
                    _ => false,
                }
            }

            /// Names of fields set in this layer.
            pub fn set_fields(&self) -> Vec<&'static str> {
                let mut out = Vec::new();
                $( if self.$name.is_some() { out.push(stringify!($name)); } )*
                out
            }

            /// Copy every field set in `over` onto `self`; returns the names copied.
            fn overlay(&mut self, over: &AiSettings) -> Vec<&'static str> {
                let mut out = Vec::new();
                $( if let Some(v) = &over.$name {
                    self.$name = Some(v.clone());
                    out.push(stringify!($name));
                } )*
                out
            }

            /// Clear a field by name; false if the name is unknown.
            fn clear(&mut self, field: &str) -> bool {
                match field {
                    $( stringify!($name) => { self.$name = None; true } )*
                    _ => false,
                }
            }
        }
    };
    (@is_model model) => { true };
    (@is_model harness) => { false };
}

ai_settings! {
    // --- sampling ---
    [model] temperature: f64,
    [model] top_p: f64,
    [model] top_k: u32,
    [model] min_p: f64,
    [model] max_output_tokens: u32,
    [model] presence_penalty: f64,
    [model] frequency_penalty: f64,
    [model] repetition_penalty: f64,
    [model] seed: u64,
    [model] stop: Vec<String>,
    /// Token id (as string, JSON object keys) → bias.
    [model] logit_bias: BTreeMap<String, f64>,
    // --- reasoning ---
    [model] reasoning_effort: ReasoningEffort,
    /// Thinking budget in tokens; 0 disables thinking where the provider allows it.
    [model] thinking_budget: u32,
    [harness] show_reasoning: ReasoningVisibility,
    [harness] keep_reasoning_in_history: bool,
    // --- output ---
    [model] json_mode: bool,
    [model] json_schema: Value,
    [harness] auto_repair_retries: u8,
    [model] stream: bool,
    // --- context ---
    /// Context window override in tokens (clamped to the model's real limit downstream).
    [harness] context_limit: u32,
    /// Fraction of the context window (0..=1) at which compaction triggers.
    [harness] compaction_threshold: f64,
    /// Tool output larger than this many KiB spills to a file instead of the transcript.
    [harness] spill_kb: u32,
    [harness] memory_injection: bool,
    [harness] rag_top_k: u32,
    // --- cost ---
    [harness] per_turn_cost_cap_usd: f64,
}

impl AiSettings {
    /// `self` overlaid by `over` (fields set in `over` win).
    pub fn merged(&self, over: &AiSettings) -> AiSettings {
        let mut out = self.clone();
        out.overlay(over);
        out
    }

    pub fn is_empty(&self) -> bool {
        self.set_fields().is_empty()
    }

    /// Model request parameters, in declaration order.
    pub fn model_params() -> impl Iterator<Item = &'static str> {
        Self::FIELDS
            .iter()
            .copied()
            .filter(|f| Self::is_model_param(f))
    }

    /// Drop set model parameters the target model can't take. Harness fields always pass.
    /// Returns the filtered settings and the names dropped (for logging / UI hints).
    pub fn retain_supported<C: ModelCapsLike + ?Sized>(
        &self,
        caps: &C,
    ) -> (AiSettings, Vec<&'static str>) {
        let mut out = self.clone();
        let mut dropped = Vec::new();
        for field in self.set_fields() {
            if Self::is_model_param(field) && !caps.supports(field) {
                out.clear(field);
                dropped.push(field);
            }
        }
        (out, dropped)
    }
}

/// Capability view of a model, keyed by `AiSettings` field name. hk-llm implements this
/// from models.dev data; hk-core stays free of provider deps.
pub trait ModelCapsLike {
    /// Whether the model accepts the given model parameter (one of `AiSettings::model_params()`).
    fn supports(&self, field: &str) -> bool;
}

impl<F: Fn(&str) -> bool> ModelCapsLike for F {
    fn supports(&self, field: &str) -> bool {
        self(field)
    }
}

/// Where a settings layer comes from. Precedence follows declaration order (later wins);
/// `Agent` and `Mode` share a rank, ties broken by input order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum Scope {
    Global,
    Provider(String),
    Model(String),
    Agent(String),
    Mode(String),
    Project(String),
    Session(String),
    Message,
}

impl Scope {
    pub fn rank(&self) -> u8 {
        match self {
            Scope::Global => 0,
            Scope::Provider(_) => 1,
            Scope::Model(_) => 2,
            Scope::Agent(_) | Scope::Mode(_) => 3,
            Scope::Project(_) => 4,
            Scope::Session(_) => 5,
            Scope::Message => 6,
        }
    }
}

/// Effective settings plus which scope set each field.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resolved {
    settings: AiSettings,
    provenance: BTreeMap<&'static str, Scope>,
}

impl Resolved {
    pub fn settings(&self) -> &AiSettings {
        &self.settings
    }

    pub fn into_settings(self) -> AiSettings {
        self.settings
    }

    /// Scope that supplied the effective value of `field`; `None` if unset everywhere.
    pub fn provenance(&self, field: &str) -> Option<&Scope> {
        self.provenance.get(field)
    }

    pub fn provenance_map(&self) -> &BTreeMap<&'static str, Scope> {
        &self.provenance
    }
}

/// Ordered collection of settings layers.
#[derive(Debug, Clone, Default)]
pub struct SettingsStack {
    layers: Vec<(Scope, AiSettings)>,
}

impl SettingsStack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, scope: Scope, settings: AiSettings) -> &mut Self {
        self.layers.push((scope, settings));
        self
    }

    pub fn layers(&self) -> &[(Scope, AiSettings)] {
        &self.layers
    }

    pub fn resolved(&self) -> Resolved {
        Self::resolve(&self.layers)
    }

    /// Resolve layers by scope rank (input order breaks ties), later overriding earlier.
    pub fn resolve(layers: &[(Scope, AiSettings)]) -> Resolved {
        let mut ordered: Vec<&(Scope, AiSettings)> = layers.iter().collect();
        ordered.sort_by_key(|(scope, _)| scope.rank()); // stable
        let mut out = Resolved::default();
        for (scope, layer) in ordered {
            for field in out.settings.overlay(layer) {
                out.provenance.insert(field, scope.clone());
            }
        }
        out
    }
}

/// Named, shareable bundle of settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub description: String,
    pub settings: AiSettings,
}

impl Preset {
    /// Preset applied with explicit `overrides` on top (user choices beat the preset).
    pub fn with_overrides(&self, overrides: &AiSettings) -> AiSettings {
        self.settings.merged(overrides)
    }

    pub fn builtin(name: &str) -> Option<Preset> {
        builtin_presets().into_iter().find(|p| p.name == name)
    }
}

fn preset(name: &str, description: &str, settings: AiSettings) -> Preset {
    Preset {
        name: name.into(),
        description: description.into(),
        settings,
    }
}

/// The four built-in presets from DESIGN §3.15.
pub fn builtin_presets() -> Vec<Preset> {
    vec![
        preset(
            "Precise coder",
            "Low temperature, deep reasoning; for code edits and refactors.",
            AiSettings {
                temperature: Some(0.1),
                top_p: Some(0.95),
                reasoning_effort: Some(ReasoningEffort::High),
                show_reasoning: Some(ReasoningVisibility::Collapse),
                auto_repair_retries: Some(2),
                ..Default::default()
            },
        ),
        preset(
            "Creative writer",
            "High temperature with mild repetition penalties; for prose and ideation.",
            AiSettings {
                temperature: Some(1.0),
                top_p: Some(0.95),
                presence_penalty: Some(0.3),
                frequency_penalty: Some(0.3),
                reasoning_effort: Some(ReasoningEffort::Low),
                ..Default::default()
            },
        ),
        preset(
            "Cheap & fast",
            "Short outputs, minimal reasoning, early compaction and a per-turn cost cap.",
            AiSettings {
                temperature: Some(0.3),
                max_output_tokens: Some(2048),
                reasoning_effort: Some(ReasoningEffort::Low),
                thinking_budget: Some(0),
                show_reasoning: Some(ReasoningVisibility::Hide),
                keep_reasoning_in_history: Some(false),
                compaction_threshold: Some(0.6),
                rag_top_k: Some(3),
                per_turn_cost_cap_usd: Some(0.05),
                ..Default::default()
            },
        ),
        preset(
            "Offline phone",
            "Small context and outputs for on-device models; no thinking, lean injection.",
            AiSettings {
                max_output_tokens: Some(512),
                reasoning_effort: Some(ReasoningEffort::Low),
                thinking_budget: Some(0),
                keep_reasoning_in_history: Some(false),
                stream: Some(true),
                context_limit: Some(4096),
                compaction_threshold: Some(0.7),
                spill_kb: Some(16),
                memory_injection: Some(false),
                rag_top_k: Some(2),
                ..Default::default()
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(t: f64) -> AiSettings {
        AiSettings {
            temperature: Some(t),
            ..Default::default()
        }
    }

    #[test]
    fn later_scopes_override_regardless_of_input_order() {
        let layers = vec![
            (Scope::Session("s1".into()), temp(0.9)),
            (Scope::Global, temp(0.2)),
            (Scope::Model("m".into()), temp(0.5)),
        ];
        let r = SettingsStack::resolve(&layers);
        assert_eq!(r.settings().temperature, Some(0.9));
        assert_eq!(
            r.provenance("temperature"),
            Some(&Scope::Session("s1".into()))
        );

        let mut stack = SettingsStack::new();
        stack
            .push(Scope::Global, temp(0.2))
            .push(Scope::Message, temp(0.0))
            .push(Scope::Project("/p".into()), temp(0.7));
        assert_eq!(stack.resolved().settings().temperature, Some(0.0));
        assert_eq!(
            stack.resolved().provenance("temperature"),
            Some(&Scope::Message)
        );
    }

    #[test]
    fn agent_and_mode_tie_broken_by_input_order() {
        let layers = vec![
            (Scope::Mode("plan".into()), temp(0.3)),
            (Scope::Agent("a".into()), temp(0.4)),
        ];
        let r = SettingsStack::resolve(&layers);
        assert_eq!(r.settings().temperature, Some(0.4));
        assert_eq!(r.provenance("temperature"), Some(&Scope::Agent("a".into())));
    }

    #[test]
    fn provenance_tracks_each_field_independently() {
        let global = AiSettings {
            temperature: Some(0.2),
            seed: Some(7),
            stream: Some(true),
            ..Default::default()
        };
        let provider = AiSettings {
            seed: Some(42),
            ..Default::default()
        };
        let session = AiSettings {
            stream: Some(false),
            ..Default::default()
        };
        let r = SettingsStack::resolve(&[
            (Scope::Global, global),
            (Scope::Provider("deepseek".into()), provider),
            (Scope::Session("s".into()), session),
        ]);
        assert_eq!(r.provenance("temperature"), Some(&Scope::Global));
        assert_eq!(
            r.provenance("seed"),
            Some(&Scope::Provider("deepseek".into()))
        );
        assert_eq!(r.provenance("stream"), Some(&Scope::Session("s".into())));
        assert_eq!(r.provenance("top_p"), None);
        assert_eq!(r.provenance("nonexistent"), None);
        assert_eq!(r.settings().seed, Some(42));
        assert_eq!(r.settings().stream, Some(false));
        assert_eq!(r.provenance_map().len(), 3);
    }

    #[test]
    fn preset_merge_explicit_overrides_win() {
        assert_eq!(builtin_presets().len(), 4);
        for name in [
            "Precise coder",
            "Creative writer",
            "Cheap & fast",
            "Offline phone",
        ] {
            assert!(Preset::builtin(name).is_some(), "{name}");
        }
        let p = Preset::builtin("Cheap & fast").unwrap();
        let merged = p.with_overrides(&AiSettings {
            max_output_tokens: Some(4096),
            seed: Some(1),
            ..Default::default()
        });
        assert_eq!(merged.max_output_tokens, Some(4096));
        assert_eq!(merged.seed, Some(1));
        assert_eq!(merged.per_turn_cost_cap_usd, Some(0.05));

        // Preset used as a session layer beats global, loses to message.
        let r = SettingsStack::resolve(&[
            (Scope::Global, temp(0.8)),
            (Scope::Session("s".into()), p.settings.clone()),
            (
                Scope::Message,
                AiSettings {
                    rag_top_k: Some(9),
                    ..Default::default()
                },
            ),
        ]);
        assert_eq!(r.settings().temperature, Some(0.3));
        assert_eq!(
            r.provenance("temperature"),
            Some(&Scope::Session("s".into()))
        );
        assert_eq!(r.settings().rag_top_k, Some(9));
        assert_eq!(r.provenance("rag_top_k"), Some(&Scope::Message));
    }

    #[test]
    fn unsupported_model_params_dropped_harness_kept() {
        let s = AiSettings {
            temperature: Some(0.5),
            top_k: Some(40),
            logit_bias: Some(BTreeMap::from([("50256".into(), -100.0)])),
            thinking_budget: Some(1024),
            spill_kb: Some(32),
            per_turn_cost_cap_usd: Some(0.1),
            ..Default::default()
        };
        // Caps: only temperature among model params; harness fields never consulted.
        let caps = |f: &str| f == "temperature";
        let (kept, dropped) = s.retain_supported(&caps);
        assert_eq!(dropped, vec!["top_k", "logit_bias", "thinking_budget"]);
        assert_eq!(kept.temperature, Some(0.5));
        assert_eq!(kept.top_k, None);
        assert_eq!(kept.logit_bias, None);
        assert_eq!(kept.thinking_budget, None);
        assert_eq!(kept.spill_kb, Some(32));
        assert_eq!(kept.per_turn_cost_cap_usd, Some(0.1));
        assert!(AiSettings::is_model_param("top_k"));
        assert!(!AiSettings::is_model_param("spill_kb"));
        assert_eq!(AiSettings::model_params().count(), 16);
    }

    #[test]
    fn serde_snake_case_and_none_omitted() {
        let s = AiSettings {
            max_output_tokens: Some(10),
            reasoning_effort: Some(ReasoningEffort::Medium),
            ..Default::default()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"max_output_tokens": 10, "reasoning_effort": "medium"})
        );
        let back: AiSettings = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
        assert!(AiSettings::default().is_empty());
        assert_eq!(AiSettings::FIELDS.len(), 25);
    }
}
