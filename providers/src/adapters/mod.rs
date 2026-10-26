//! Adapter registry: provider id -> implementation.

pub mod anthropic;
pub mod compat;
pub mod gemini;
pub mod openai_chat;
pub mod openai_responses;

use crate::adapter::WireAdapter;
use crate::error::{Error, Result};

/// Resolve an adapter by provider id.
///
/// `openai` maps to the Responses API because that is what upstream Codex uses;
/// `openai-chat` is available explicitly, and every compat preset is reachable by
/// its own id.
pub fn by_id(id: &str) -> Result<Box<dyn WireAdapter>> {
    match id {
        "anthropic" => Ok(Box::new(anthropic::Anthropic::default())),
        "gemini" | "google" => Ok(Box::new(gemini::Gemini::default())),
        "openai" => Ok(Box::new(openai_responses::OpenAiResponses::default())),
        "openai-responses" => Ok(Box::new(openai_responses::OpenAiResponses::default())),
        "openai-chat" => Ok(Box::new(openai_chat::OpenAiChat::openai())),
        other => compat::find(other).map(|p| Box::new(p.to_adapter()) as Box<dyn WireAdapter>).ok_or_else(|| {
            Error::Config {
                detail: format!(
                    "unknown provider '{other}'; known: anthropic, gemini, openai, openai-responses, openai-chat, {}",
                    compat::PRESETS.iter().map(|p| p.id).collect::<Vec<_>>().join(", ")
                ),
            }
        }),
    }
}

/// Provider ids this build can reach.
pub fn provider_ids() -> Vec<&'static str> {
    let mut v = vec![
        "anthropic",
        "gemini",
        "openai",
        "openai-responses",
        "openai-chat",
    ];
    v.extend(compat::PRESETS.iter().map(|p| p.id));
    v
}
