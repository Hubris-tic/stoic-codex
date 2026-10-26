//! OpenAI Responses API.
//!
//! This is the surface Codex itself uses upstream, so the fork needs it to stay
//! compatible, while the canonical IR keeps everything else honest.
//!
//! Traps handled:
//!   * `input` is a flat list of items, not a messages array: `function_call` and
//!     `function_call_output` are siblings of role-messages.
//!   * Stream deltas are keyed by `item_id`, not by array index, so ids must be
//!     mapped to IR indices ourselves.
//!   * Usage is nested in `response.completed`.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use crate::adapter::{json_of, AuthSpec, StreamDecoder, WireAdapter, WireRequest};
use crate::capabilities::{Capabilities, ToolChoiceSupport};
use crate::error::Result;
use crate::ir::{
    CanonicalEvent, CanonicalRequest, CanonicalResponse, Content, Role, StopReason, ToolChoice,
    Usage,
};
use crate::sse::SseFrame;

pub const NAME: &str = "openai-responses";

#[derive(Debug, Clone)]
pub struct OpenAiResponses {
    pub base_url: String,
}

impl Default for OpenAiResponses {
    fn default() -> Self {
        OpenAiResponses {
            base_url: "https://api.openai.com/v1".into(),
        }
    }
}

impl OpenAiResponses {
    pub fn capabilities() -> Capabilities {
        Capabilities {
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
            stop_sequences: false,
            max_context: 200_000,
            max_output: 32_000,
        }
    }
}

fn item_text(c: &Content) -> Option<Value> {
    match c {
        Content::Text { text } => Some(json!({ "type": "input_text", "text": text })),
        Content::Image { media_type, data } => Some(json!({
            "type": "input_image",
            "image_url": format!("data:{media_type};base64,{data}")
        })),
        _ => None,
    }
}

#[derive(Default)]
pub struct ResponsesStream {
    item_to_index: HashMap<String, usize>,
    next_index: usize,
    finished: bool,
    /// Set when a function_call item is emitted; decides the terminal stop reason.
    saw_tool_call: bool,
}

impl StreamDecoder for ResponsesStream {
    fn push_frame(&mut self, frame: &SseFrame) -> Result<Vec<CanonicalEvent>> {
        let Some(v) = json_of(NAME, frame)? else {
            return Ok(vec![]);
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
        let mut out = Vec::new();
        match ty {
            "response.created" => {
                let model = v
                    .get("response")
                    .and_then(|r| r.get("model"))
                    .and_then(|m| m.as_str())
                    .unwrap_or_default();
                out.push(CanonicalEvent::MessageStart {
                    model: model.to_string(),
                });
            }
            "response.output_item.added" => {
                let item = v.get("item").cloned().unwrap_or(Value::Null);
                if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                    let item_id = item
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let idx = self.next_index;
                    self.next_index += 1;
                    self.item_to_index.insert(item_id, idx);
                    self.saw_tool_call = true;
                    out.push(CanonicalEvent::ToolUseBegin {
                        index: idx,
                        id: item
                            .get("call_id")
                            .and_then(|c| c.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        name: item
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    });
                }
            }
            "response.output_text.delta" => {
                if let Some(d) = v.get("delta").and_then(|d| d.as_str()) {
                    out.push(CanonicalEvent::TextDelta {
                        text: d.to_string(),
                    });
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(d) = v.get("delta").and_then(|d| d.as_str()) {
                    out.push(CanonicalEvent::ThinkingDelta {
                        text: d.to_string(),
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                let item_id = v
                    .get("item_id")
                    .and_then(|i| i.as_str())
                    .unwrap_or_default();
                if let Some(idx) = self.item_to_index.get(item_id).copied() {
                    let d = v.get("delta").and_then(|d| d.as_str()).unwrap_or_default();
                    out.push(CanonicalEvent::ToolUseInputDelta {
                        index: idx,
                        fragment: d.to_string(),
                    });
                }
            }
            "response.output_item.done" => {
                let item = v.get("item").cloned().unwrap_or(Value::Null);
                if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                    let item_id = item.get("id").and_then(|i| i.as_str()).unwrap_or_default();
                    if let Some(idx) = self.item_to_index.get(item_id).copied() {
                        out.push(CanonicalEvent::ToolUseEnd {
                            index: idx,
                            signature: None,
                        });
                    }
                }
            }
            "response.completed" => {
                let r = v.get("response").cloned().unwrap_or(Value::Null);
                if let Some(u) = r.get("usage") {
                    out.push(CanonicalEvent::Usage { usage: usage_of(u) });
                }
                let sr = r
                    .get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or("completed");
                let reason = if self.saw_tool_call {
                    StopReason::ToolUse
                } else if sr == "incomplete" {
                    StopReason::MaxTokens
                } else {
                    StopReason::EndTurn
                };
                out.push(CanonicalEvent::Stop { reason });
                out.push(CanonicalEvent::Done);
                self.finished = true;
            }
            "response.failed" | "error" => {
                let err = v
                    .get("error")
                    .or_else(|| v.get("response").and_then(|r| r.get("error")))
                    .cloned();
                let err = err.unwrap_or(Value::Null);
                out.push(CanonicalEvent::Error {
                    kind: err
                        .get("code")
                        .and_then(|c| c.as_str())
                        .unwrap_or("error")
                        .to_string(),
                    message: err
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
        if self.finished {
            Ok(vec![])
        } else {
            Ok(vec![CanonicalEvent::Done])
        }
    }
}

fn usage_of(u: &Value) -> Usage {
    let n = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    Usage {
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cache_read_tokens: u
            .get("input_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cache_write_tokens: 0,
        reasoning_tokens: u
            .get("output_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    }
}

impl WireAdapter for OpenAiResponses {
    fn id(&self) -> &'static str {
        NAME
    }

    fn capabilities(&self) -> Capabilities {
        Self::capabilities()
    }

    fn build_request(&self, req: &CanonicalRequest) -> Result<WireRequest> {
        let mut input: Vec<Value> = Vec::new();
        for m in &req.messages {
            match m.role {
                Role::Tool => {
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
                            input.push(json!({
                                "type": "function_call_output",
                                "call_id": tool_use_id,
                                "output": text
                            }));
                        }
                    }
                }
                Role::Assistant => {
                    let text: String = m
                        .content
                        .iter()
                        .filter_map(|c| c.as_text())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.is_empty() {
                        input.push(json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": text }]
                        }));
                    }
                    for c in &m.content {
                        if let Content::ToolUse {
                            id,
                            name,
                            input: args,
                            signature: None,
                        } = c
                        {
                            input.push(json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": args.to_string()
                            }));
                        }
                    }
                }
                Role::User | Role::System => {
                    if m.role == Role::System {
                        continue; // handled as `instructions`
                    }
                    let parts: Vec<Value> = m.content.iter().filter_map(item_text).collect();
                    input.push(json!({ "type": "message", "role": "user", "content": parts }));
                }
            }
        }

        let mut body = Map::new();
        body.insert("model".into(), json!(req.model));
        body.insert("input".into(), json!(input));
        if let Some(sys) = &req.system {
            body.insert("instructions".into(), json!(sys));
        }
        if let Some(n) = req.max_tokens {
            body.insert("max_output_tokens".into(), json!(n));
        }
        if let Some(t) = req.temperature {
            body.insert("temperature".into(), json!(t));
        }
        if let Some(p) = req.top_p {
            body.insert("top_p".into(), json!(p));
        }
        if !req.tools.is_empty() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    let mut o = Map::new();
                    o.insert("type".into(), json!("function"));
                    o.insert("name".into(), json!(t.name));
                    if let Some(d) = &t.description {
                        o.insert("description".into(), json!(d));
                    }
                    o.insert("parameters".into(), t.input_schema.clone());
                    Value::Object(o)
                })
                .collect();
            body.insert("tools".into(), json!(tools));
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
                    body.insert("tool_choice".into(), json!({"type":"function","name":name}));
                }
                None => {}
            }
        }
        if let Some(r) = &req.reasoning {
            body.insert("reasoning".into(), json!({ "effort": r }));
        }
        if req.json_mode {
            body.insert(
                "text".into(),
                json!({ "format": { "type": "json_object" } }),
            );
        }
        let stream = req.stream.unwrap_or(true);
        body.insert("stream".into(), json!(stream));
        for (k, v) in &req.extra {
            body.insert(k.clone(), v.clone());
        }

        Ok(WireRequest {
            method: "POST".to_string(),
            url: format!("{}/responses", self.base_url.trim_end_matches('/')),
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
            auth: AuthSpec::Bearer,
            body: Value::Object(body),
        })
    }

    fn stream_decoder(&self, _req: &CanonicalRequest) -> Box<dyn StreamDecoder> {
        Box::new(ResponsesStream::default())
    }

    fn list_models_request(&self) -> Option<crate::adapter::WireRequest> {
        Some(crate::adapter::WireRequest {
            method: "GET".to_string(),
            url: format!("{}/models", self.base_url.trim_end_matches('/')),
            headers: vec![("accept".into(), "application/json".into())],
            auth: AuthSpec::Bearer,
            body: serde_json::Value::Null,
        })
    }

    fn parse_response(&self, req: &CanonicalRequest, body: &Value) -> Result<CanonicalResponse> {
        let mut content = Vec::new();
        if let Some(items) = body.get("output").and_then(|o| o.as_array()) {
            for it in items {
                match it.get("type").and_then(|t| t.as_str()).unwrap_or_default() {
                    "message" => {
                        if let Some(parts) = it.get("content").and_then(|c| c.as_array()) {
                            for p in parts {
                                if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                                    content.push(Content::text(t));
                                }
                            }
                        }
                    }
                    "function_call" => {
                        let args = it.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}");
                        content.push(Content::ToolUse {
                            id: it
                                .get("call_id")
                                .and_then(|c| c.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            name: it
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            input: serde_json::from_str(args).unwrap_or_else(|_| json!({})),
                            signature: None,
                        });
                    }
                    _ => {}
                }
            }
        }
        Ok(CanonicalResponse {
            model: body
                .get("model")
                .and_then(|m| m.as_str())
                .map(|m| m.to_string())
                .unwrap_or_else(|| req.model.clone()),
            content,
            stop_reason: match body.get("status").and_then(|s| s.as_str()) {
                Some("incomplete") => StopReason::MaxTokens,
                _ => StopReason::EndTurn,
            },
            usage: body.get("usage").map(usage_of).unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::StreamAccumulator;

    #[test]
    fn function_calls_are_siblings_not_messages() {
        let mut req = CanonicalRequest::new("gpt", "go");
        req.messages.push(crate::ir::Message {
            role: Role::Assistant,
            content: vec![Content::ToolUse {
                id: "call_9".into(),
                name: "sh".into(),
                input: json!({"cmd":"ls"}),
                signature: None,
            }],
        });
        req.messages
            .push(crate::ir::Message::tools(vec![Content::ToolResult {
                tool_use_id: "call_9".into(),
                content: vec![Content::text("out")],
                is_error: false,
            }]));
        let w = OpenAiResponses::default().build_request(&req).unwrap();
        let input = w.body["input"].as_array().unwrap();
        assert_eq!(input[1]["type"], json!("function_call"));
        assert_eq!(input[1]["arguments"], json!("{\"cmd\":\"ls\"}"));
        assert_eq!(input[2]["type"], json!("function_call_output"));
        assert_eq!(input[2]["call_id"], json!("call_9"));
    }

    #[test]
    fn item_id_maps_to_index_across_stream() {
        let a = OpenAiResponses::default();
        let req = CanonicalRequest::new("gpt", "x");
        let mut d = a.stream_decoder(&req);
        let mk = |s: &str| SseFrame {
            event: None,
            data: s.to_string(),
            id: None,
        };
        d.push_frame(&mk(r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"item_A","call_id":"c1","name":"read"}}"#)).unwrap();
        let evs = d
            .push_frame(&mk(r#"{"type":"response.function_call_arguments.delta","item_id":"item_A","delta":"{\"a\":1}"}"#))
            .unwrap();
        assert_eq!(
            evs,
            vec![CanonicalEvent::ToolUseInputDelta {
                index: 0,
                fragment: "{\"a\":1}".into()
            }]
        );
    }

    #[test]
    fn completed_event_terminates_stream_once() {
        let a = OpenAiResponses::default();
        let req = CanonicalRequest::new("gpt", "x");
        let mut d = a.stream_decoder(&req);
        let frame = SseFrame {
            event: None,
            data: r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":5,"output_tokens":7}}}"#.into(),
            id: None,
        };
        let evs = d.push_frame(&frame).unwrap();
        let mut acc = StreamAccumulator::default();
        for e in evs {
            acc.apply(e);
        }
        assert!(acc.done);
        assert_eq!(acc.usage.input_tokens, 5);
        assert_eq!(acc.usage.output_tokens, 7);
        assert!(
            d.finish().unwrap().is_empty(),
            "Done must not be emitted twice"
        );
    }
}
