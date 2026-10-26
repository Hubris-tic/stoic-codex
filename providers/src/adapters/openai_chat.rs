//! OpenAI Chat Completions — and every vendor that clones it.
//!
//! This one adapter covers OpenAI plus Groq, Mistral, OpenRouter, DeepSeek, xAI,
//! Together, Fireworks, Cerebras, LM Studio, Ollama, vLLM and friends. The long tail
//! of "OpenAI-compatible" is not actually compatible, so quirks are declared per
//! vendor in [`super::compat`] rather than discovered at runtime as 400s.
//!
//! Traps handled here:
//!   * Tool arguments arrive as fragments tagged by `index`, and the first fragment
//!     for an index carries the id/name while later ones may omit both.
//!   * `usage` is absent unless you opt in with `stream_options.include_usage`, and
//!     when enabled it arrives in a final chunk with an empty `choices` array.
//!   * Some servers require `max_completion_tokens`; others reject it.
//!   * Reasoning text shows up as `reasoning_content` (DeepSeek) or `reasoning`
//!     (OpenRouter), not in the documented schema anywhere.

use serde_json::{json, Map, Value};

use crate::adapter::{json_of, AuthSpec, StreamDecoder, WireAdapter, WireRequest};
use crate::capabilities::{Capabilities, ToolChoiceSupport};
use crate::error::{Error, Result};
use crate::ir::{
    CanonicalEvent, CanonicalRequest, CanonicalResponse, Content, Message, Role, StopReason,
    ToolChoice, Usage,
};
use crate::sse::SseFrame;

pub const NAME: &str = "openai-chat";

/// Which key carries the output cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaxTokensField {
    MaxTokens,
    MaxCompletionTokens,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Quirks {
    pub max_tokens_field: MaxTokensField,
    /// Vendor honours `tool_choice`. Some 400 on it even though they support tools.
    pub supports_tool_choice: bool,
    /// Vendor emits usage in the stream only with `stream_options.include_usage`.
    pub needs_stream_options: bool,
    /// Field name carrying chain-of-thought deltas, if any.
    pub reasoning_field: Option<&'static str>,
    /// Prefix overrides for the `usage` object (rare).
    pub usage_input_key: &'static str,
    pub usage_output_key: &'static str,
}

impl Default for Quirks {
    fn default() -> Self {
        Quirks {
            max_tokens_field: MaxTokensField::MaxTokens,
            supports_tool_choice: true,
            needs_stream_options: true,
            reasoning_field: None,
            usage_input_key: "prompt_tokens",
            usage_output_key: "completion_tokens",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiChat {
    pub id: &'static str,
    pub base_url: String,
    pub quirks: Quirks,
    pub caps: Capabilities,
    pub auth: AuthSpec,
}

impl OpenAiChat {
    pub fn openai() -> Self {
        OpenAiChat {
            id: "openai-chat",
            base_url: "https://api.openai.com/v1".into(),
            quirks: Quirks {
                reasoning_field: Some("reasoning"),
                ..Default::default()
            },
            caps: Capabilities {
                tools: true,
                parallel_tools: true,
                streaming: true,
                stream_usage: true,
                vision: true,
                implicit_cache: true,
                explicit_cache: false,
                reasoning: true,
                json_mode: true,
                tool_choice: ToolChoiceSupport::Full,
                stop_sequences: true,
                max_context: 128_000,
                max_output: 16_384,
            },
            auth: AuthSpec::Bearer,
        }
    }

    pub fn with_overrides(
        mut self,
        base_url: impl Into<String>,
        quirks: Quirks,
        caps: Capabilities,
    ) -> Self {
        self.base_url = base_url.into();
        self.quirks = quirks;
        self.caps = caps;
        self
    }
}

fn message_to_wire(m: &Message, id: &'static str) -> Result<Value> {
    match m.role {
        Role::System => {
            let text: String = m
                .content
                .iter()
                .filter_map(|c| c.as_text())
                .collect::<Vec<_>>()
                .join("\n");
            Ok(json!({ "role": "system", "content": text }))
        }
        Role::User => {
            let has_image = m.content.iter().any(|c| matches!(c, Content::Image { .. }));
            if has_image {
                let parts: Vec<Value> = m
                    .content
                    .iter()
                    .map(|c| match c {
                        Content::Text { text } => Ok(json!({"type":"text","text":text})),
                        Content::Image { media_type, data } => Ok(json!({
                            "type": "image_url",
                            "image_url": { "url": format!("data:{media_type};base64,{data}") }
                        })),
                        _ => Err(Error::unsupported(
                            id,
                            "mixed image/tool content in one user turn",
                        )),
                    })
                    .collect::<Result<_>>()?;
                Ok(json!({ "role": "user", "content": parts }))
            } else {
                let text: String = m
                    .content
                    .iter()
                    .filter_map(|c| c.as_text())
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(json!({ "role": "user", "content": text }))
            }
        }
        Role::Assistant => {
            let text: String = m
                .content
                .iter()
                .filter_map(|c| c.as_text())
                .collect::<Vec<_>>()
                .join("\n");
            let calls: Vec<Value> = m
                .content
                .iter()
                .filter_map(|c| match c {
                    Content::ToolUse {
                        id, name, input, ..
                    } => Some(json!({
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": input.to_string() }
                    })),
                    _ => None,
                })
                .collect();
            if calls.is_empty() {
                Ok(json!({ "role": "assistant", "content": text }))
            } else {
                Ok(json!({ "role": "assistant", "content": text, "tool_calls": calls }))
            }
        }
        Role::Tool => {
            // Chat Completions requires one `tool` message per result, not an array.
            // Callers iterate this; see `messages_to_wire`.
            Err(Error::unsupported(id, "tool role must be expanded"))
        }
    }
}

/// Expand a `Role::Tool` message into one wire message per result.
fn messages_to_wire(req: &CanonicalRequest, id: &'static str) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for m in &req.messages {
        if m.role == Role::Tool {
            for c in &m.content {
                if let Content::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } = c
                {
                    let text: String = content
                        .iter()
                        .filter_map(|x| x.as_text())
                        .collect::<Vec<_>>()
                        .join("\n");
                    out.push(
                        json!({ "role": "tool", "tool_call_id": tool_use_id, "content": text }),
                    );
                }
            }
        } else {
            out.push(message_to_wire(m, id)?);
        }
    }
    Ok(out)
}

#[derive(Default)]
pub struct OpenAiChatStream {
    quirks: Quirks,
    /// wire `index` of a tool call -> IR index we emitted
    seen_tools: Vec<usize>,
}

impl OpenAiChatStream {
    fn new(quirks: Quirks) -> Self {
        Self {
            quirks,
            seen_tools: Vec::new(),
        }
    }
}

impl StreamDecoder for OpenAiChatStream {
    fn push_frame(&mut self, frame: &SseFrame) -> Result<Vec<CanonicalEvent>> {
        if frame.is_done_sentinel() {
            return Ok(vec![CanonicalEvent::Done]);
        }
        let Some(v) = json_of(NAME, frame)? else {
            return Ok(vec![]);
        };
        let mut out = Vec::new();

        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            out.push(CanonicalEvent::Usage {
                usage: usage_of(u, &self.quirks),
            });
        }
        if let Some(model) = v.get("model").and_then(|m| m.as_str()) {
            if !model.is_empty() {
                out.push(CanonicalEvent::MessageStart {
                    model: model.to_string(),
                });
            }
        }

        let choice = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .cloned();
        let Some(choice) = choice else { return Ok(out) };
        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);

        if let Some(t) = delta.get("content").and_then(|c| c.as_str()) {
            if !t.is_empty() {
                out.push(CanonicalEvent::TextDelta {
                    text: t.to_string(),
                });
            }
        }
        if let Some(field) = self.quirks.reasoning_field {
            if let Some(t) = delta.get(field).and_then(|c| c.as_str()) {
                if !t.is_empty() {
                    out.push(CanonicalEvent::ThinkingDelta {
                        text: t.to_string(),
                    });
                }
            }
        }

        if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
            for call in calls {
                let wire_idx = call.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                while self.seen_tools.len() <= wire_idx {
                    self.seen_tools.push(usize::MAX);
                }
                if self.seen_tools[wire_idx] == usize::MAX {
                    let ir_idx = self.seen_tools.iter().filter(|x| **x != usize::MAX).count();
                    self.seen_tools[wire_idx] = ir_idx;
                    out.push(CanonicalEvent::ToolUseBegin {
                        index: ir_idx,
                        id: call
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        name: call
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    });
                }
                let ir_idx = self.seen_tools[wire_idx];
                if let Some(args) = call
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                {
                    if !args.is_empty() {
                        out.push(CanonicalEvent::ToolUseInputDelta {
                            index: ir_idx,
                            fragment: args.to_string(),
                        });
                    }
                }
            }
        }

        if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
            // Close any open tool calls before the stop event so consumers can parse.
            for i in 0..self.seen_tools.len() {
                let mapped = self.seen_tools[i];
                if mapped != usize::MAX {
                    out.push(CanonicalEvent::ToolUseEnd {
                        index: mapped,
                        signature: None,
                    });
                    self.seen_tools[i] = usize::MAX;
                }
            }
            out.push(CanonicalEvent::Stop {
                reason: stop_reason(fr),
            });
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<CanonicalEvent>> {
        // Servers that omit `[DONE]` still get a clean terminal event.
        Ok(vec![CanonicalEvent::Done])
    }
}

fn usage_of(u: &Value, q: &Quirks) -> Usage {
    let n = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    Usage {
        input_tokens: n(q.usage_input_key),
        output_tokens: n(q.usage_output_key),
        cache_read_tokens: u
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cache_write_tokens: 0,
        reasoning_tokens: u
            .get("completion_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    }
}

fn stop_reason(s: &str) -> StopReason {
    match s {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::ContentFilter,
        _ => StopReason::Unknown,
    }
}

impl WireAdapter for OpenAiChat {
    fn id(&self) -> &'static str {
        self.id
    }

    fn capabilities(&self) -> Capabilities {
        // A compatible is only as capable as the server behind it, and the spec is where
        // that knowledge lives. OpenAI itself gets the full set.
        self.caps.clone()
    }

    fn build_request(&self, req: &CanonicalRequest) -> Result<WireRequest> {
        let mut body = Map::new();
        body.insert("model".into(), json!(req.model));

        let mut msgs = messages_to_wire(req, self.id)?;
        if let Some(sys) = &req.system {
            // OpenAI keeps system as a message; prepend unless one already exists.
            if !msgs
                .iter()
                .any(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
            {
                msgs.insert(0, json!({ "role": "system", "content": sys }));
            }
        }
        body.insert("messages".into(), json!(msgs));

        if let Some(n) = req.max_tokens {
            match self.quirks.max_tokens_field {
                MaxTokensField::MaxTokens => body.insert("max_tokens".into(), json!(n)),
                MaxTokensField::MaxCompletionTokens => {
                    body.insert("max_completion_tokens".into(), json!(n))
                }
            };
        }

        if !req.tools.is_empty() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    let mut f = Map::new();
                    f.insert("name".into(), json!(t.name));
                    if let Some(d) = &t.description {
                        f.insert("description".into(), json!(d));
                    }
                    f.insert("parameters".into(), t.input_schema.clone());
                    json!({ "type": "function", "function": Value::Object(f) })
                })
                .collect();
            body.insert("tools".into(), json!(tools));

            if self.quirks.supports_tool_choice {
                match &req.tool_choice {
                    Some(ToolChoice::Auto) => {
                        body.insert("tool_choice".into(), json!("auto"));
                    }
                    Some(ToolChoice::None) => {
                        body.insert("tool_choice".into(), json!("none"));
                    }
                    Some(ToolChoice::Required) => {
                        body.insert("tool_choice".into(), json!("required"));
                    }
                    Some(ToolChoice::Tool { name }) => {
                        body.insert(
                            "tool_choice".into(),
                            json!({"type":"function","function":{"name":name}}),
                        );
                    }
                    None => {}
                }
            }
        }

        if req.json_mode {
            body.insert("response_format".into(), json!({"type": "json_object"}));
        }
        if let Some(t) = req.temperature {
            body.insert("temperature".into(), json!(t));
        }
        if let Some(p) = req.top_p {
            body.insert("top_p".into(), json!(p));
        }
        if !req.stop.is_empty() {
            body.insert("stop".into(), json!(req.stop));
        }

        let stream = req.stream.unwrap_or(true);
        body.insert("stream".into(), json!(stream));
        if stream && self.quirks.needs_stream_options {
            body.insert("stream_options".into(), json!({ "include_usage": true }));
        }
        for (k, v) in &req.extra {
            body.insert(k.clone(), v.clone());
        }

        Ok(WireRequest {
            method: "POST".to_string(),
            url: format!("{}/chat/completions", self.base_url.trim_end_matches('/')),
            headers: vec![
                ("content-type".into(), "application/json".into()),
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
            auth: self.auth.clone(),
            body: Value::Object(body),
        })
    }

    fn stream_decoder(&self, _req: &CanonicalRequest) -> Box<dyn StreamDecoder> {
        Box::new(OpenAiChatStream::new(self.quirks.clone()))
    }

    /// `/models` is part of the OpenAI API surface, so every compatible exposes one — which
    /// is why this single implementation covers Groq, OpenRouter, Mistral, DeepSeek, xAI and a
    /// local Ollama server.
    fn list_models_request(&self) -> Option<crate::adapter::WireRequest> {
        Some(crate::adapter::WireRequest {
            method: "GET".to_string(),
            url: format!("{}/models", self.base_url.trim_end_matches('/')),
            headers: vec![("accept".into(), "application/json".into())],
            auth: self.auth.clone(),
            body: serde_json::Value::Null,
        })
    }

    fn parse_response(&self, req: &CanonicalRequest, body: &Value) -> Result<CanonicalResponse> {
        let choice = body
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| Error::protocol(self.id, "no choices in response", &body.to_string()))?;
        let msg = choice.get("message").cloned().unwrap_or(Value::Null);
        let mut content = Vec::new();
        if let Some(t) = msg.get("content").and_then(|c| c.as_str()) {
            if !t.is_empty() {
                content.push(Content::text(t));
            }
        }
        if let Some(calls) = msg.get("tool_calls").and_then(|c| c.as_array()) {
            for c in calls {
                let args = c
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                    .unwrap_or("{}");
                content.push(Content::ToolUse {
                    id: c
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    name: c
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    input: serde_json::from_str(args).unwrap_or_else(|_| json!({})),
                    signature: None,
                });
            }
        }
        Ok(CanonicalResponse {
            model: body
                .get("model")
                .and_then(|m| m.as_str())
                .map(|m| m.to_string())
                .unwrap_or_else(|| req.model.clone()),
            content,
            stop_reason: choice
                .get("finish_reason")
                .and_then(|f| f.as_str())
                .map(stop_reason)
                .unwrap_or(StopReason::EndTurn),
            usage: body
                .get("usage")
                .map(|u| usage_of(u, &self.quirks))
                .unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::SseDecoder;

    fn decode_all(frames: &[&str]) -> Vec<CanonicalEvent> {
        let a = OpenAiChat::openai();
        let req = CanonicalRequest::new("gpt", "x");
        let mut d = a.stream_decoder(&req);
        let mut out = Vec::new();
        for f in frames {
            let frame = SseFrame {
                event: None,
                data: f.to_string(),
                id: None,
            };
            out.extend(d.push_frame(&frame).unwrap());
        }
        out.extend(d.finish().unwrap());
        out
    }

    #[test]
    fn fragmented_tool_arguments_are_assembled() {
        let evs = decode_all(&[
            r#"{"model":"m","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"pa"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ]);
        let mut acc = crate::ir::StreamAccumulator::default();
        for e in evs {
            acc.apply(e);
        }
        let resp = acc.into_response("fallback");
        let uses = resp.tool_uses();
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].1, "read");
        assert_eq!(uses[0].2["path"], serde_json::json!("a.rs"));
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn usage_chunk_with_empty_choices_still_parses() {
        let evs = decode_all(&[
            r#"{"choices":[{"delta":{"content":"hi"}}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":3}}"#,
            "[DONE]",
        ]);
        let usage = evs
            .iter()
            .find_map(|e| match e {
                CanonicalEvent::Usage { usage } => Some(usage.clone()),
                _ => None,
            })
            .expect("usage emitted");
        assert_eq!(usage.input_tokens, 11);
        assert_eq!(usage.output_tokens, 3);
        assert!(evs.contains(&CanonicalEvent::Done));
    }

    #[test]
    fn stream_options_opt_in() {
        let a = OpenAiChat::openai();
        let w = a.build_request(&CanonicalRequest::new("gpt", "x")).unwrap();
        assert_eq!(
            w.body["stream_options"]["include_usage"],
            serde_json::json!(true)
        );
        assert_eq!(w.auth, AuthSpec::Bearer);
    }

    #[test]
    fn max_completion_tokens_quirk() {
        let mut a = OpenAiChat::openai();
        a.quirks.max_tokens_field = MaxTokensField::MaxCompletionTokens;
        let w = a
            .build_request(&CanonicalRequest::new("o-series", "x").with_max_tokens(99))
            .unwrap();
        assert_eq!(w.body["max_completion_tokens"], serde_json::json!(99));
        assert!(w.body.get("max_tokens").is_none());
    }

    #[test]
    fn tool_results_expand_to_one_message_each() {
        let mut req = CanonicalRequest::new("gpt", "x");
        req.messages.push(Message::assistant("ok"));
        req.messages.push(Message::tools(vec![
            Content::ToolResult {
                tool_use_id: "a".into(),
                content: vec![Content::text("1")],
                is_error: false,
            },
            Content::ToolResult {
                tool_use_id: "b".into(),
                content: vec![Content::text("2")],
                is_error: false,
            },
        ]));
        let w = OpenAiChat::openai().build_request(&req).unwrap();
        let msgs = w.body["messages"].as_array().unwrap();
        let tool_msgs: Vec<_> = msgs.iter().filter(|m| m["role"] == "tool").collect();
        assert_eq!(tool_msgs.len(), 2);
        assert_eq!(tool_msgs[1]["tool_call_id"], serde_json::json!("b"));
    }

    #[test]
    fn sse_frame_split_across_chunks() {
        // Proves the decoder survives an HTTP chunk boundary mid-JSON.
        let mut d = SseDecoder::new();
        assert!(d.push("data: {\"choices\":[{\"del").is_empty());
        let frames = d.push("ta\":{\"content\":\"hi\"}}]}\n\n");
        assert_eq!(frames.len(), 1);
        assert!(frames[0].data.contains("hi"));
    }
}
