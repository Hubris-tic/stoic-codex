//! OpenAI-compatible vendor presets.
//!
//! "OpenAI-compatible" is a spectrum, not a spec. Each preset below encodes the
//! quirks that otherwise surface as a 400 in production: which key caps output,
//! whether `tool_choice` is accepted, whether streaming usage needs an opt-in, and
//! what the reasoning field is called. Only vendors we can name accurately are here;
//! anything else is configured by the user with [`Preset::custom`].

use crate::adapter::AuthSpec;
use crate::capabilities::{Capabilities, ToolChoiceSupport};

use super::openai_chat::{MaxTokensField, OpenAiChat, Quirks};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub id: &'static str,
    pub base_url: &'static str,
    pub max_tokens_field: MaxTokensField,
    pub supports_tool_choice: bool,
    pub needs_stream_options: bool,
    pub reasoning_field: Option<&'static str>,
    pub vision: bool,
    pub local: bool,
    pub max_context: u32,
    pub auth: AuthSpec,
}

impl Preset {
    pub fn to_adapter(&self) -> OpenAiChat {
        OpenAiChat {
            id: self.id,
            base_url: self.base_url.to_string(),
            quirks: Quirks {
                max_tokens_field: self.max_tokens_field,
                supports_tool_choice: self.supports_tool_choice,
                needs_stream_options: self.needs_stream_options,
                reasoning_field: self.reasoning_field,
                ..Default::default()
            },
            caps: Capabilities {
                tools: true,
                parallel_tools: true,
                streaming: true,
                stream_usage: self.needs_stream_options,
                vision: self.vision,
                implicit_cache: true,
                explicit_cache: false,
                reasoning: self.reasoning_field.is_some(),
                json_mode: true,
                tool_choice: if self.supports_tool_choice {
                    ToolChoiceSupport::Full
                } else {
                    ToolChoiceSupport::None
                },
                stop_sequences: true,
                max_context: self.max_context,
                max_output: 8_192,
            },
            auth: self.auth.clone(),
        }
    }

    /// A custom endpoint (self-hosted vLLM, an internal gateway, LM Studio on a
    /// custom port). Defaults are the conservative set: fewest surprises.
    pub fn custom(id: &'static str, base_url: &'static str) -> Preset {
        Preset {
            id,
            base_url,
            max_tokens_field: MaxTokensField::MaxTokens,
            supports_tool_choice: false,
            needs_stream_options: false,
            reasoning_field: None,
            vision: false,
            local: true,
            max_context: 32_000,
            auth: AuthSpec::Bearer,
        }
    }
}

/// Built-in presets. Base URLs are the vendors' documented defaults.
pub const PRESETS: &[Preset] = &[
    Preset {
        id: "groq",
        base_url: "https://api.groq.com/openai/v1",
        max_tokens_field: MaxTokensField::MaxTokens,
        supports_tool_choice: true,
        needs_stream_options: true,
        reasoning_field: None,
        vision: true,
        local: false,
        max_context: 128_000,
        auth: AuthSpec::Bearer,
    },
    Preset {
        id: "mistral",
        base_url: "https://api.mistral.ai/v1",
        max_tokens_field: MaxTokensField::MaxTokens,
        supports_tool_choice: true,
        needs_stream_options: false,
        reasoning_field: None,
        vision: true,
        local: false,
        max_context: 128_000,
        auth: AuthSpec::Bearer,
    },
    Preset {
        id: "openrouter",
        base_url: "https://openrouter.ai/api/v1",
        max_tokens_field: MaxTokensField::MaxTokens,
        supports_tool_choice: true,
        needs_stream_options: true,
        reasoning_field: Some("reasoning"),
        vision: true,
        local: false,
        max_context: 200_000,
        auth: AuthSpec::Bearer,
    },
    Preset {
        id: "deepseek",
        base_url: "https://api.deepseek.com/v1",
        max_tokens_field: MaxTokensField::MaxTokens,
        supports_tool_choice: true,
        needs_stream_options: true,
        reasoning_field: Some("reasoning_content"),
        vision: false,
        local: false,
        max_context: 128_000,
        auth: AuthSpec::Bearer,
    },
    Preset {
        id: "xai",
        base_url: "https://api.x.ai/v1",
        max_tokens_field: MaxTokensField::MaxTokens,
        supports_tool_choice: true,
        needs_stream_options: true,
        reasoning_field: Some("reasoning_content"),
        vision: true,
        local: false,
        max_context: 256_000,
        auth: AuthSpec::Bearer,
    },
    Preset {
        id: "together",
        base_url: "https://api.together.xyz/v1",
        max_tokens_field: MaxTokensField::MaxTokens,
        supports_tool_choice: true,
        needs_stream_options: true,
        reasoning_field: None,
        vision: true,
        local: false,
        max_context: 128_000,
        auth: AuthSpec::Bearer,
    },
    Preset {
        id: "fireworks",
        base_url: "https://api.fireworks.ai/inference/v1",
        max_tokens_field: MaxTokensField::MaxTokens,
        supports_tool_choice: true,
        needs_stream_options: true,
        reasoning_field: None,
        vision: true,
        local: false,
        max_context: 128_000,
        auth: AuthSpec::Bearer,
    },
    Preset {
        id: "cerebras",
        base_url: "https://api.cerebras.ai/v1",
        max_tokens_field: MaxTokensField::MaxTokens,
        supports_tool_choice: true,
        needs_stream_options: true,
        reasoning_field: None,
        vision: false,
        local: false,
        max_context: 64_000,
        auth: AuthSpec::Bearer,
    },
    Preset {
        id: "ollama",
        base_url: "http://localhost:11434/v1",
        max_tokens_field: MaxTokensField::MaxTokens,
        supports_tool_choice: false,
        needs_stream_options: false,
        reasoning_field: None,
        vision: false,
        local: true,
        max_context: 32_000,
        auth: AuthSpec::None,
    },
    Preset {
        id: "lmstudio",
        base_url: "http://localhost:1234/v1",
        max_tokens_field: MaxTokensField::MaxTokens,
        supports_tool_choice: false,
        needs_stream_options: false,
        reasoning_field: None,
        vision: false,
        local: true,
        max_context: 32_000,
        auth: AuthSpec::None,
    },
    Preset {
        id: "vllm",
        base_url: "http://localhost:8000/v1",
        max_tokens_field: MaxTokensField::MaxTokens,
        supports_tool_choice: false,
        needs_stream_options: false,
        reasoning_field: None,
        vision: false,
        local: true,
        max_context: 32_000,
        auth: AuthSpec::None,
    },
];

pub fn find(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_duplicate_preset_ids() {
        let mut ids: Vec<&str> = PRESETS.iter().map(|p| p.id).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "preset ids must be unique");
    }

    #[test]
    fn local_presets_need_no_key() {
        for p in PRESETS.iter().filter(|p| p.local) {
            assert_eq!(
                p.auth,
                AuthSpec::None,
                "{} runs locally and must not require a key",
                p.id
            );
        }
    }

    #[test]
    fn presets_reach_their_own_endpoints() {
        use crate::adapter::WireAdapter;
        for p in PRESETS {
            let a = p.to_adapter();
            let w = a
                .build_request(&crate::ir::CanonicalRequest::new("m", "x"))
                .unwrap();
            assert!(w.url.starts_with(p.base_url), "{} url was {}", p.id, w.url);
            assert!(w.url.ends_with("/chat/completions"));
        }
    }
}
