//! Model registry: the data the router reasons over.
//!
//! Built-in entries are **placeholders, not facts**. Model lineups and prices change
//! monthly, so the registry loads from JSON (config or `models.json`) and the built-in
//! seed exists only so the crate is usable out of the box. Treat the numbers as
//! "edit me", and prefer sourcing them from the provider's `/models` endpoint where
//! the vendor exposes one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::capabilities::{Capabilities, ToolChoiceSupport};
use crate::cost::Pricing;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Fast/cheap: classification, retrieval query writing, summarization.
    Small,
    /// Daily driver: implementation, test writing.
    Mid,
    /// Planning, architecture, hard debugging, review.
    Strong,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    /// Wire model name, e.g. what goes in the request body.
    pub id: String,
    /// Provider id this model is reached through (`anthropic`, `openai`, `gemini`, `groq`...).
    pub provider: String,
    /// Provider-side model name, when it differs from `id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_name: Option<String>,
    pub tier: Tier,
    pub capabilities: Capabilities,
    #[serde(default)]
    pub pricing: Pricing,
    /// Runs on the user's machine (ollama, lmstudio, vllm). Free and private.
    #[serde(default)]
    pub local: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl ModelSpec {
    pub fn wire(&self) -> &str {
        self.wire_name.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    models: BTreeMap<String, ModelSpec>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, spec: ModelSpec) {
        self.models.insert(spec.id.clone(), spec);
    }

    pub fn get(&self, id: &str) -> Option<&ModelSpec> {
        self.models.get(id)
    }

    /// Find a model by registry id **or** by the name you would put on the wire.
    ///
    /// `--model gemini-3.1-flash-lite` is what a user types; `gemini:flash-lite` is what the
    /// registry calls it. Resolving both means cost accounting does not silently degrade to
    /// "unknown" for anyone who names the model the way the vendor does.
    pub fn resolve(&self, id_or_wire: &str) -> Option<&ModelSpec> {
        self.models
            .get(id_or_wire)
            .or_else(|| self.models.values().find(|m| m.wire() == id_or_wire))
    }

    pub fn len(&self) -> usize {
        self.models.len()
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &ModelSpec> {
        self.models.values()
    }

    pub fn by_provider(&self, provider: &str) -> Vec<&ModelSpec> {
        self.models
            .values()
            .filter(|m| m.provider == provider)
            .collect()
    }

    /// Load a registry from a JSON array of specs, or from `{"models": [...]}`.
    pub fn from_json(s: &str) -> Result<Self> {
        let v: serde_json::Value = serde_json::from_str(s).map_err(|e| Error::Config {
            detail: format!("registry JSON invalid: {e}"),
        })?;
        let arr = match &v {
            serde_json::Value::Array(_) => v.clone(),
            _ => v.get("models").cloned().ok_or(Error::Config {
                detail: "registry JSON must be an array or {\"models\": [...]}".into(),
            })?,
        };
        let specs: Vec<ModelSpec> = serde_json::from_value(arr).map_err(|e| Error::Config {
            detail: format!("registry entry invalid: {e}"),
        })?;
        let mut reg = Registry::new();
        for s in specs {
            reg.insert(s);
        }
        Ok(reg)
    }

    pub fn to_json(&self) -> String {
        let v: Vec<&ModelSpec> = self.models.values().collect();
        serde_json::to_string_pretty(&v).unwrap_or_else(|_| "[]".into())
    }

    /// Seed catalogue. Numbers are illustrative defaults; override from config.
    pub fn seeded() -> Self {
        let mut r = Registry::new();
        let cap = |ctx: u32, out: u32, tools: bool, vision: bool, reasoning: bool| Capabilities {
            tools,
            parallel_tools: tools,
            streaming: true,
            stream_usage: true,
            vision,
            implicit_cache: true,
            explicit_cache: false,
            reasoning,
            json_mode: true,
            tool_choice: if tools {
                ToolChoiceSupport::Full
            } else {
                ToolChoiceSupport::None
            },
            stop_sequences: true,
            max_context: ctx,
            max_output: out,
        };

        r.insert(ModelSpec {
            id: "openai:gpt-5-codex".into(),
            provider: "openai".into(),
            wire_name: Some("gpt-5-codex".into()),
            tier: Tier::Strong,
            capabilities: cap(200_000, 32_000, true, true, true),
            pricing: Pricing::new(1.25, 10.0),
            local: false,
            tags: vec!["coding".into()],
        });
        r.insert(ModelSpec {
            id: "anthropic:claude-sonnet".into(),
            provider: "anthropic".into(),
            wire_name: Some("claude-sonnet-4-5".into()),
            tier: Tier::Strong,
            capabilities: Capabilities {
                explicit_cache: true,
                implicit_cache: false,
                ..cap(200_000, 32_000, true, true, true)
            },
            pricing: Pricing {
                cache_read_multiplier: 0.1,
                cache_write_multiplier: 1.25,
                ..Pricing::new(3.0, 15.0)
            },
            local: false,
            tags: vec!["coding".into(), "long-context".into()],
        });
        // Gemini entries verified live on 2026-10-26: `gemini-2.5-flash` is refused for new
        // keys ("no longer available to new users"), and these are the current ids. Prices
        // from ai.google.dev/gemini-api/docs/pricing on the same date. 3.8 Flash is on an
        // introductory rate through 2026-12-31 and doubles on 2027-01-01 — re-check before
        // trusting a cost report next year.
        r.insert(ModelSpec {
            id: "gemini:flash-lite".into(),
            provider: "gemini".into(),
            wire_name: Some("gemini-3.1-flash-lite".into()),
            tier: Tier::Small,
            capabilities: cap(1_000_000, 64_000, true, true, true),
            pricing: Pricing {
                cache_read_multiplier: 0.1, // $0.025 vs $0.25 input
                ..Pricing::new(0.25, 1.50)
            },
            local: false,
            tags: vec![
                "cheap".into(),
                "huge-context".into(),
                "live-verified".into(),
            ],
        });
        r.insert(ModelSpec {
            id: "gemini:flash".into(),
            provider: "gemini".into(),
            wire_name: Some("gemini-3.8-flash".into()),
            tier: Tier::Mid,
            capabilities: cap(1_000_000, 64_000, true, true, true),
            pricing: Pricing {
                cache_read_multiplier: 0.1, // $0.075 vs $0.75 input
                ..Pricing::new(0.75, 3.75)
            },
            local: false,
            tags: vec!["huge-context".into(), "live-verified".into()],
        });
        r.insert(ModelSpec {
            id: "ollama:qwen-coder".into(),
            provider: "ollama".into(),
            wire_name: Some("qwen2.5-coder".into()),
            tier: Tier::Mid,
            capabilities: cap(32_000, 8_000, true, false, false),
            pricing: Pricing::default(),
            local: true,
            tags: vec!["offline".into(), "private".into()],
        });
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_resolves_by_registry_id_or_wire_name() {
        let r = Registry::seeded();
        let by_id = r.resolve("gemini:flash-lite").expect("by id");
        let by_wire = r.resolve("gemini-3.1-flash-lite").expect("by wire name");
        assert_eq!(by_id.id, by_wire.id);
        assert_eq!(by_wire.pricing.input_per_mtok, 0.25);
        assert_eq!(by_wire.pricing.output_per_mtok, 1.50);
        assert!(r.resolve("no-such-model").is_none());
    }
}
