//! "What does this endpoint serve **today**?"
//!
//! Every provider's model list is a moving target, and every agent that hardcodes one is
//! wrong within a quarter. This project already paid for that lesson the expensive way: a
//! seeded `gemini-2.5-flash` preset 404'd on a fresh key, and the only way to find out was a
//! failed request. So the adapter is given a second, tiny job — describe its own catalogue
//! endpoint and parse the answer — and the CLI can ask instead of guess.
//!
//! The parsing is deliberately forgiving about extra fields and strict about the shape: a
//! list of models is not worth failing over, but silently returning an empty list when the
//! payload was actually an error document is worth failing over.

use serde_json::Value;

use crate::error::{Error, Result};

/// One model as the endpoint describes it. Everything but `id` is optional, because
/// providers disagree about what is worth reporting and invent new field names regularly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    /// The name to pass as `--model` (already stripped of any `models/` prefix).
    pub id: String,
    /// Human-facing name, when the provider offers one.
    pub display: Option<String>,
    /// Input context window in tokens, when reported.
    pub context: Option<u32>,
    /// Provider-reported input price (OpenRouter reports USD per token).
    pub input_price: Option<String>,
    /// Provider-reported output price (OpenRouter reports USD per token).
    pub output_price: Option<String>,
}

impl ModelInfo {
    pub fn new(id: impl Into<String>) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            display: None,
            context: None,
            input_price: None,
            output_price: None,
        }
    }

    /// `true` only when the endpoint explicitly labels the model as free or reports
    /// zero for both input and output. A missing price is unknown, not a free-model claim.
    pub fn is_free(&self) -> bool {
        is_free_id(&self.id)
            || matches!(
                (&self.input_price, &self.output_price),
                (Some(input), Some(output)) if is_zero_price(input) && is_zero_price(output)
            )
    }

    /// Sort key: longest context first, then alphabetical. The most useful model for a
    /// context-hungry agent should be at the top of the list, not the alphabetically first.
    pub fn ordering(&self) -> (std::cmp::Reverse<u32>, &str) {
        (
            std::cmp::Reverse(self.context.unwrap_or(0)),
            self.id.as_str(),
        )
    }
}

/// True when the id looks like a **free** model on a gateway that mixes paid and free ones.
///
/// OpenRouter marks them `:free`; that suffix is part of the id and is part of the request,
/// so it is not stripped. This is the whole reason the function exists: on a zero budget,
/// "which of these 400 models can I actually call" is the only question that matters.
pub fn is_free_id(id: &str) -> bool {
    id.ends_with(":free") || id.contains(":free/") || id.ends_with("-free")
}

fn first_price(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| match value.get(*key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

fn is_zero_price(value: &str) -> bool {
    value
        .trim()
        .parse::<f64>()
        .map(|n| n == 0.0)
        .unwrap_or(false)
}

/// Parse a catalogue response.
///
/// Three shapes cover essentially every provider in use:
///
/// * **Gemini** — `{"models": [{"name": "models/x", "displayName": …, "inputTokenLimit": …,
///   "supportedGenerationMethods": ["generateContent"]}]}`
/// * **OpenAI and everything that copies it** (Groq, OpenRouter, Mistral, DeepSeek, xAI,
///   Ollama) — `{"data": [{"id": "x", "context_length" | "context_window": …}]}`
/// * **Anthropic** — `{"data": [{"id": "x", "display_name": …}]}`, plus pagination fields we
///   ignore.
pub fn parse(provider: &'static str, body: &Value) -> Result<Vec<ModelInfo>> {
    let (array, gemini) = if let Some(models) = body.get("models").and_then(Value::as_array) {
        (models, true)
    } else if let Some(data) = body.get("data").and_then(Value::as_array) {
        (data, false)
    } else {
        // An error document arrives here when the caller forgot to check the status code.
        // Say so, with the provider's own words if it gave any, rather than "no models".
        let detail = body
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("no `models` or `data` array in the response");
        return Err(Error::protocol(
            provider,
            format!("unexpected catalogue payload: {detail}"),
            &body.to_string(),
        ));
    };

    let mut out = Vec::with_capacity(array.len());
    for entry in array {
        let name = if gemini {
            entry.get("name").and_then(Value::as_str)
        } else {
            entry.get("id").and_then(Value::as_str)
        };
        let Some(name) = name else { continue };

        // Gemini lists embedding and AQA models in the same array. A chat agent asking for
        // one and getting `text-embedding-004` back is a confusing 400 later.
        if gemini {
            let methods = entry
                .get("supportedGenerationMethods")
                .and_then(Value::as_array);
            if let Some(methods) = methods {
                let chat = methods
                    .iter()
                    .any(|m| m.as_str() == Some("generateContent"));
                if !chat {
                    continue;
                }
            }
        }

        let id = name.strip_prefix("models/").unwrap_or(name).to_string();
        let display = entry
            .get(if gemini {
                "displayName"
            } else {
                "display_name"
            })
            .and_then(Value::as_str)
            .map(str::to_string);
        let context = [
            "inputTokenLimit",
            "context_length",
            "context_window",
            "max_context_length",
            "max_input_tokens",
        ]
        .iter()
        .find_map(|k| entry.get(*k).and_then(Value::as_u64))
        .map(|n| n.min(u32::MAX as u64) as u32);

        let pricing = entry.get("pricing");
        let input_price = pricing.and_then(|p| first_price(p, &["prompt", "input", "input_price"]));
        let output_price =
            pricing.and_then(|p| first_price(p, &["completion", "output", "output_price"]));

        out.push(ModelInfo {
            id,
            display,
            context,
            input_price,
            output_price,
        });
    }

    out.sort_by(|a, b| a.ordering().cmp(&b.ordering()));
    out.dedup_by(|a, b| a.id == b.id);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_openai_shaped_catalogue_parses() {
        let body = json!({"object": "list", "data": [
            {"id": "llama-3.3-70b-versatile", "object": "model", "context_window": 131072,
             "pricing": {"prompt": "0.00000059", "completion": "0.00000079"}},
            {"id": "llama-3.1-8b-instant", "object": "model"}
        ]});
        let models = parse("groq", &body).expect("parses");
        assert_eq!(models[0].id, "llama-3.3-70b-versatile");
        assert_eq!(models[0].context, Some(131_072));
        assert_eq!(models[0].input_price.as_deref(), Some("0.00000059"));
        assert_eq!(models[0].output_price.as_deref(), Some("0.00000079"));
        assert!(!models[0].is_free());
        assert_eq!(models[1].id, "llama-3.1-8b-instant");
        assert_eq!(models[1].context, None);
        // Longest context first, so the useful model is at the top of the list.
        assert_eq!(models[0].id, "llama-3.3-70b-versatile");
    }

    #[test]
    fn a_gemini_catalogue_strips_the_prefix_and_drops_embeddings() {
        let body = json!({"models": [
            {"name": "models/gemini-3.1-flash-lite", "displayName": "Gemini 3.1 Flash-Lite",
             "inputTokenLimit": 1048576, "supportedGenerationMethods": ["generateContent", "countTokens"]},
            {"name": "models/text-embedding-004", "displayName": "Embedding",
             "inputTokenLimit": 2048, "supportedGenerationMethods": ["embedContent"]},
            {"name": "models/gemini-3.8-flash", "displayName": "Gemini 3.8 Flash",
             "inputTokenLimit": 1048576, "supportedGenerationMethods": ["generateContent"]}
        ]});
        let models = parse("gemini", &body).expect("parses");
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["gemini-3.1-flash-lite", "gemini-3.8-flash"]);
        assert!(
            !ids.iter().any(|i| i.contains("embedding")),
            "no embedding models"
        );
        assert_eq!(models[0].display.as_deref(), Some("Gemini 3.1 Flash-Lite"));
        assert_eq!(models[0].context, Some(1_048_576));
    }

    #[test]
    fn an_anthropic_shaped_catalogue_parses() {
        let body = json!({"data": [
            {"id": "claude-sonnet-4-5", "display_name": "Claude Sonnet 4.5", "type": "model"},
            {"id": "claude-haiku-4-5", "display_name": "Claude Haiku 4.5", "type": "model"}
        ], "has_more": false});
        let models = parse("anthropic", &body).expect("parses");
        assert_eq!(models.len(), 2);
        assert!(models
            .iter()
            .any(|m| m.display.as_deref() == Some("Claude Sonnet 4.5")));
    }

    #[test]
    fn free_models_on_a_gateway_are_recognisable() {
        assert!(is_free_id("deepseek/deepseek-r1:free"));
        assert!(is_free_id("meta-llama/llama-3-8b-instruct:free"));
        assert!(!is_free_id("anthropic/claude-sonnet-4.5"));
        assert!(!is_free_id("gemini-3.1-flash-lite"));

        let zero = ModelInfo {
            id: "router/zero-priced".into(),
            display: None,
            context: None,
            input_price: Some("0".into()),
            output_price: Some("0.000000".into()),
        };
        assert!(zero.is_free());
        let unknown = ModelInfo::new("gemini-3.1-flash-lite");
        assert!(
            !unknown.is_free(),
            "unknown pricing must not be asserted free"
        );
    }

    #[test]
    fn an_error_document_is_an_error_not_an_empty_list() {
        // This is the failure mode that matters: a 429 body parsed as "you have no models"
        // would send a user hunting for a config bug that does not exist.
        let body = json!({"error": {"code": 429, "message": "Resource has been exhausted"}});
        let err = parse("gemini", &body).expect_err("must not be an empty list");
        assert!(err.to_string().contains("exhausted"), "{err}");
    }

    #[test]
    fn a_payload_with_no_recognisable_entries_is_empty_not_an_error() {
        let body = json!({"data": [{"something_else": 1}]});
        assert_eq!(parse("groq", &body).expect("parses"), Vec::new());
    }

    #[test]
    fn junk_context_fields_are_ignored_rather_than_overflowing() {
        let body = json!({"data": [
            {"id": "a", "context_length": -5},
            {"id": "b", "context_length": "lots"},
            {"id": "c", "context_length": 4_294_967_296u64}
        ]});
        let models = parse("openai", &body).expect("parses");
        assert_eq!(models.len(), 3);
        assert_eq!(models.iter().find(|m| m.id == "a").unwrap().context, None);
        assert_eq!(models.iter().find(|m| m.id == "b").unwrap().context, None);
        assert_eq!(
            models.iter().find(|m| m.id == "c").unwrap().context,
            Some(u32::MAX)
        );
    }
}
