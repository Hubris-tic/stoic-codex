//! Anthropic Messages API.
//!
//! Four translation traps, all handled here so no other code has to know:
//!  1. `system` is a top-level field, not a message.
//!  2. `tool_result` blocks must live in a **user** message, immediately after the
//!     assistant turn that made the call.
//!  3. Roles must alternate. The harness produces runs of same-role messages, so we
//!     merge them here rather than making every caller remember.
//!  4. Tool arguments stream as `input_json_delta` fragments of a JSON *string*;
//!     nothing is parseable until the block closes.

use serde_json::{json, Map, Value};

use crate::adapter::{json_of, AuthSpec, StreamDecoder, WireAdapter, WireRequest};
use crate::capabilities::{Capabilities, ToolChoiceSupport};
use crate::error::{Error, Result};
use crate::ir::{
    CacheStrategy, CanonicalEvent, CanonicalRequest, CanonicalResponse, Content, Role, StopReason,
    ToolChoice, Usage,
};
use crate::sse::SseFrame;

const NAME: &str = "anthropic";
const API_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 8_192;

#[derive(Debug, Clone)]
pub struct Anthropic {
    pub base_url: String,
    /// Emit `cache_control` breakpoints. Requires the prompt-caching beta only on
    /// ancient API versions; 2023-06-01 plus the flag is enough today.
    pub enable_cache_control: bool,
}

impl Default for Anthropic {
    fn default() -> Self {
        Anthropic {
            base_url: "https://api.anthropic.com".into(),
            enable_cache_control: true,
        }
    }
}

impl Anthropic {
    pub fn capabilities() -> Capabilities {
        Capabilities {
            tools: true,
            parallel_tools: true,
            streaming: true,
            stream_usage: true,
            vision: true,
            implicit_cache: false,
            explicit_cache: true,
            reasoning: true,
            json_mode: false, // Anthropic has no JSON mode; use a tool call as the schema
            tool_choice: ToolChoiceSupport::Full,
            stop_sequences: true,
            max_context: 200_000,
            max_output: 32_000,
        }
    }
}

/// Collected system text plus any `System`-roled messages folded out of the transcript.
fn system_text(req: &CanonicalRequest) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = &req.system {
        if !s.is_empty() {
            parts.push(s.clone());
        }
    }
    for m in &req.messages {
        if m.role == Role::System {
            for c in &m.content {
                if let Some(t) = c.as_text() {
                    parts.push(t.to_string());
                }
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

fn block(c: &Content) -> Result<Value> {
    Ok(match c {
        Content::Text { text } => json!({ "type": "text", "text": text }),
        Content::Image { media_type, data } => json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media_type, "data": data }
        }),
        Content::ToolUse {
            id, name, input, ..
        } => json!({
            "type": "tool_use", "id": id, "name": name, "input": input
        }),
        Content::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let inner: Vec<Value> = content.iter().map(block).collect::<Result<_>>()?;
            json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": inner,
                "is_error": is_error
            })
        }
        Content::Thinking { text, signature } => {
            // Sending a thinking block back without its signature is a 400. A
            // replayed transcript that lost the signature is better dropped than fatal.
            match signature {
                Some(sig) => json!({ "type": "thinking", "thinking": text, "signature": sig }),
                None => {
                    return Err(Error::Config {
                        detail: "anthropic: thinking block without signature cannot be replayed"
                            .into(),
                    })
                }
            }
        }
    })
}

pub struct AnthropicStream {
    /// content index -> is a tool_use block
    tools_at: Vec<bool>,
}

impl AnthropicStream {
    fn new() -> Self {
        AnthropicStream {
            tools_at: Vec::new(),
        }
    }
}

impl StreamDecoder for AnthropicStream {
    fn push_frame(&mut self, frame: &SseFrame) -> Result<Vec<CanonicalEvent>> {
        let Some(v) = json_of(NAME, frame)? else {
            return Ok(vec![]);
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
        let mut out = Vec::new();

        match ty {
            "message_start" => {
                let msg = v.get("message").cloned().unwrap_or(Value::Null);
                out.push(CanonicalEvent::MessageStart {
                    model: msg
                        .get("model")
                        .and_then(|m| m.as_str())
                        .unwrap_or_default()
                        .to_string(),
                });
                if let Some(u) = msg.get("usage") {
                    out.push(CanonicalEvent::Usage { usage: usage_of(u) });
                }
            }
            "content_block_start" => {
                let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                while self.tools_at.len() <= idx {
                    self.tools_at.push(false);
                }
                let cb = v.get("content_block").cloned().unwrap_or(Value::Null);
                let bt = cb.get("type").and_then(|t| t.as_str()).unwrap_or_default();
                match bt {
                    "tool_use" => {
                        self.tools_at[idx] = true;
                        out.push(CanonicalEvent::ToolUseBegin {
                            index: idx,
                            id: cb
                                .get("id")
                                .and_then(|i| i.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            name: cb
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or_default()
                                .to_string(),
                        });
                        // Some gateways send the full input at start instead of deltas.
                        if let Some(input) = cb.get("input") {
                            if !input.is_null()
                                && input.as_object().map(|o| !o.is_empty()).unwrap_or(true)
                            {
                                out.push(CanonicalEvent::ToolUseInputDelta {
                                    index: idx,
                                    fragment: input.to_string(),
                                });
                            }
                        }
                    }
                    "text" => {
                        if let Some(t) = cb.get("text").and_then(|t| t.as_str()) {
                            if !t.is_empty() {
                                out.push(CanonicalEvent::TextDelta {
                                    text: t.to_string(),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let d = v.get("delta").cloned().unwrap_or(Value::Null);
                match d.get("type").and_then(|t| t.as_str()).unwrap_or_default() {
                    "text_delta" => {
                        if let Some(t) = d.get("text").and_then(|t| t.as_str()) {
                            out.push(CanonicalEvent::TextDelta {
                                text: t.to_string(),
                            });
                        }
                    }
                    "input_json_delta" => {
                        if let Some(p) = d.get("partial_json").and_then(|t| t.as_str()) {
                            out.push(CanonicalEvent::ToolUseInputDelta {
                                index: idx,
                                fragment: p.to_string(),
                            });
                        }
                    }
                    "thinking_delta" => {
                        if let Some(t) = d.get("thinking").and_then(|t| t.as_str()) {
                            out.push(CanonicalEvent::ThinkingDelta {
                                text: t.to_string(),
                            });
                        }
                    }
                    "signature_delta" => {} // signature handled by non-stream round-trip
                    _ => {}
                }
            }
            "content_block_stop" => {
                let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                if self.tools_at.get(idx).copied().unwrap_or(false) {
                    out.push(CanonicalEvent::ToolUseEnd {
                        index: idx,
                        signature: None,
                    });
                }
            }
            "message_delta" => {
                if let Some(u) = v.get("usage") {
                    out.push(CanonicalEvent::Usage { usage: usage_of(u) });
                }
                if let Some(sr) = v
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(|s| s.as_str())
                {
                    out.push(CanonicalEvent::Stop {
                        reason: stop_reason(sr),
                    });
                }
            }
            "message_stop" => out.push(CanonicalEvent::Done),
            "ping" => {}
            "error" => {
                let e = v.get("error").cloned().unwrap_or(Value::Null);
                out.push(CanonicalEvent::Error {
                    kind: e
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("error")
                        .to_string(),
                    message: e
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or_default()
                        .to_string(),
                });
            }
            _ => {}
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<CanonicalEvent>> {
        Ok(vec![])
    }
}

fn usage_of(u: &Value) -> Usage {
    let g = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    Usage {
        input_tokens: g("input_tokens"),
        output_tokens: g("output_tokens"),
        cache_read_tokens: g("cache_read_input_tokens"),
        cache_write_tokens: g("cache_creation_input_tokens"),
        reasoning_tokens: 0,
    }
}

fn stop_reason(s: &str) -> StopReason {
    match s {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "tool_use" => StopReason::ToolUse,
        "stop_sequence" => StopReason::StopSequence,
        "refusal" => StopReason::ContentFilter,
        "pause_turn" => StopReason::Paused,
        _ => StopReason::Unknown,
    }
}

impl WireAdapter for Anthropic {
    fn id(&self) -> &'static str {
        NAME
    }

    fn capabilities(&self) -> Capabilities {
        Self::capabilities()
    }

    fn build_request(&self, req: &CanonicalRequest) -> Result<WireRequest> {
        let mut body = Map::new();
        body.insert("model".into(), json!(req.model));
        // max_tokens is REQUIRED by this API. Default rather than 400.
        body.insert(
            "max_tokens".into(),
            json!(req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
        );

        if let Some(sys) = system_text(req) {
            if self.enable_cache_control && req.cache != CacheStrategy::Off {
                body.insert(
                    "system".into(),
                    json!([{
                        "type": "text",
                        "text": sys,
                        "cache_control": { "type": "ephemeral" }
                    }]),
                );
            } else {
                body.insert("system".into(), json!(sys));
            }
        }

        // Fold transcript into alternating user/assistant runs.
        let mut msgs: Vec<Value> = Vec::new();
        let mut pending: Option<(Role, Vec<Value>)> = None;
        let flush = |pending: &mut Option<(Role, Vec<Value>)>, msgs: &mut Vec<Value>| {
            if let Some((role, blocks)) = pending.take() {
                if !blocks.is_empty() {
                    msgs.push(json!({
                        "role": if role == Role::Assistant { "assistant" } else { "user" },
                        "content": blocks
                    }));
                }
            }
        };

        for m in &req.messages {
            if m.role == Role::System {
                continue; // folded into `system` above
            }
            // tool results and user turns both map to `user`
            let wire_role = if m.role == Role::Assistant {
                Role::Assistant
            } else {
                Role::User
            };
            let blocks: Vec<Value> = m.content.iter().map(block).collect::<Result<_>>()?;
            match &mut pending {
                Some((r, b)) if *r == wire_role => b.extend(blocks),
                _ => {
                    flush(&mut pending, &mut msgs);
                    pending = Some((wire_role, blocks));
                }
            }
        }
        flush(&mut pending, &mut msgs);
        body.insert("messages".into(), json!(msgs));

        if !req.tools.is_empty() {
            let mut tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    let mut o = Map::new();
                    o.insert("name".into(), json!(t.name));
                    if let Some(d) = &t.description {
                        o.insert("description".into(), json!(d));
                    }
                    o.insert("input_schema".into(), t.input_schema.clone());
                    if self.enable_cache_control && req.cache != CacheStrategy::Off {
                        o.insert("cache_control".into(), json!({ "type": "ephemeral" }));
                    }
                    Value::Object(o)
                })
                .collect();
            // Only the last tool carries the breakpoint: cache_control marks the END
            // of a cached prefix, so marking every tool wastes writes.
            if self.enable_cache_control && req.cache != CacheStrategy::Off {
                let keep = tools.len().saturating_sub(1);
                for t in tools.iter_mut().take(keep) {
                    if let Some(o) = t.as_object_mut() {
                        o.remove("cache_control");
                    }
                }
            }
            body.insert("tools".into(), json!(tools));

            match &req.tool_choice {
                Some(ToolChoice::Auto) => {
                    body.insert("tool_choice".into(), json!({"type": "auto"}));
                }
                Some(ToolChoice::None) => {
                    body.insert("tool_choice".into(), json!({"type": "none"}));
                }
                Some(ToolChoice::Required) => {
                    body.insert("tool_choice".into(), json!({"type": "any"}));
                }
                Some(ToolChoice::Tool { name }) => {
                    body.insert("tool_choice".into(), json!({"type": "tool", "name": name}));
                }
                None => {}
            }
        } else if matches!(
            req.tool_choice,
            Some(ToolChoice::Required) | Some(ToolChoice::Tool { .. })
        ) {
            return Err(Error::unsupported(NAME, "tool_choice without tools"));
        }

        if !req.stop.is_empty() {
            body.insert("stop_sequences".into(), json!(req.stop));
        }
        if let Some(t) = req.temperature {
            body.insert("temperature".into(), json!(t));
        }
        if let Some(p) = req.top_p {
            body.insert("top_p".into(), json!(p));
        }
        if let Some(r) = &req.reasoning {
            let budget = match r.as_str() {
                "low" => 2_048,
                "medium" => 8_192,
                _ => 16_000,
            };
            body.insert(
                "thinking".into(),
                json!({"type": "enabled", "budget_tokens": budget}),
            );
            // Extended thinking requires temperature == 1.
            body.remove("temperature");
        }
        let stream = req.stream.unwrap_or(true);
        body.insert("stream".into(), json!(stream));
        for (k, v) in &req.extra {
            body.insert(k.clone(), v.clone());
        }

        Ok(WireRequest {
            method: "POST".to_string(),
            url: format!("{}/v1/messages", self.base_url.trim_end_matches('/')),
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("anthropic-version".into(), API_VERSION.into()),
                (
                    "accept".into(),
                    if stream {
                        "text/event-stream"
                    } else {
                        "application/json"
                    }
                    .into(),
                ),
            ],
            auth: AuthSpec::header("x-api-key"),
            body: Value::Object(body),
        })
    }

    fn stream_decoder(&self, _req: &CanonicalRequest) -> Box<dyn StreamDecoder> {
        Box::new(AnthropicStream::new())
    }

    fn list_models_request(&self) -> Option<crate::adapter::WireRequest> {
        Some(crate::adapter::WireRequest {
            method: "GET".to_string(),
            url: format!("{}/v1/models", self.base_url.trim_end_matches('/')),
            headers: vec![
                ("anthropic-version".into(), API_VERSION.into()),
                ("accept".into(), "application/json".into()),
            ],
            auth: AuthSpec::header("x-api-key"),
            body: serde_json::Value::Null,
        })
    }

    fn parse_response(&self, req: &CanonicalRequest, body: &Value) -> Result<CanonicalResponse> {
        let mut content = Vec::new();
        let blocks = body
            .get("content")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        for b in blocks {
            match b.get("type").and_then(|t| t.as_str()).unwrap_or_default() {
                "text" => content.push(Content::text(
                    b.get("text").and_then(|t| t.as_str()).unwrap_or_default(),
                )),
                "tool_use" => content.push(Content::ToolUse {
                    id: b
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    name: b
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    input: b.get("input").cloned().unwrap_or(json!({})),
                    signature: None,
                }),
                "thinking" => content.push(Content::Thinking {
                    text: b
                        .get("thinking")
                        .and_then(|t| t.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    signature: b
                        .get("signature")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string()),
                }),
                _ => {}
            }
        }
        let sr = body
            .get("stop_reason")
            .and_then(|s| s.as_str())
            .unwrap_or("end_turn");
        Ok(CanonicalResponse {
            model: body
                .get("model")
                .and_then(|m| m.as_str())
                .map(|m| m.to_string())
                .unwrap_or_else(|| req.model.clone()),
            content,
            stop_reason: stop_reason(sr),
            usage: body.get("usage").map(usage_of).unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Message;

    #[test]
    fn system_is_top_level_not_a_message() {
        let req = CanonicalRequest::new("claude", "hi").with_system("You are terse.");
        let w = Anthropic::default().build_request(&req).unwrap();
        assert_eq!(w.body["system"], json!("You are terse."));
        let msgs = w.body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], json!("user"));
    }

    #[test]
    fn tool_results_are_folded_into_user_role_and_merged() {
        let mut req = CanonicalRequest::new("claude", "read the file");
        req.messages.push(Message {
            role: Role::Assistant,
            content: vec![Content::ToolUse {
                id: "tu_1".into(),
                name: "read".into(),
                input: json!({"path":"a.rs"}),
                signature: None,
            }],
        });
        req.messages.push(Message::tools(vec![
            Content::ToolResult {
                tool_use_id: "tu_1".into(),
                content: vec![Content::text("fn main(){}")],
                is_error: false,
            },
            Content::ToolResult {
                tool_use_id: "tu_2".into(),
                content: vec![Content::text("boom")],
                is_error: true,
            },
        ]));
        let w = Anthropic::default().build_request(&req).unwrap();
        let msgs = w.body["messages"].as_array().unwrap();
        // user, assistant, user(tool_result x2 merged into one message)
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[2]["role"], json!("user"));
        let blocks = msgs[2]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1]["is_error"], json!(true));
    }

    #[test]
    fn consecutive_same_role_messages_are_merged() {
        let mut req = CanonicalRequest::new("claude", "a");
        req.messages.push(Message::user("b"));
        req.messages.push(Message::user("c"));
        let w = Anthropic::default().build_request(&req).unwrap();
        let msgs = w.body["messages"].as_array().unwrap();
        assert_eq!(
            msgs.len(),
            1,
            "three user turns must collapse into one message"
        );
        assert_eq!(msgs[0]["content"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn max_tokens_defaults_instead_of_erroring() {
        let req = CanonicalRequest::new("claude", "hi");
        let w = Anthropic::default().build_request(&req).unwrap();
        assert_eq!(w.body["max_tokens"], json!(DEFAULT_MAX_TOKENS));
    }

    #[test]
    fn cache_control_only_on_last_tool() {
        let tools = vec![
            crate::ir::ToolSpec {
                name: "a".into(),
                description: None,
                input_schema: json!({"type":"object"}),
            },
            crate::ir::ToolSpec {
                name: "b".into(),
                description: None,
                input_schema: json!({"type":"object"}),
            },
        ];
        let req = CanonicalRequest::new("claude", "hi")
            .with_tools(tools)
            .with_cache(CacheStrategy::Prefix);
        let w = Anthropic::default().build_request(&req).unwrap();
        let t = w.body["tools"].as_array().unwrap();
        assert!(
            t[0].get("cache_control").is_none(),
            "earlier tools must not carry a breakpoint"
        );
        assert!(
            t[1].get("cache_control").is_some(),
            "last tool carries the prefix breakpoint"
        );
    }

    #[test]
    fn key_never_appears_in_request() {
        let req = CanonicalRequest::new("claude", "hi");
        let w = Anthropic::default().build_request(&req).unwrap();
        assert!(
            w.header("x-api-key").is_none(),
            "credential must be injected by the transport"
        );
        assert_eq!(w.auth, AuthSpec::header("x-api-key"));
        assert!(!w.url.contains("key="));
    }
}
