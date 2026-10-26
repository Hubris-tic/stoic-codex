//! Google Gemini (Generative Language API).
//!
//! The traps, all handled:
//!  1. Roles are only `user` and `model`. Tool results are `functionResponse`
//!     parts inside a **user** turn.
//!  2. Function calls carry an id *and* the response is still matched by *name*.
//!     Gemini 3.x returns `functionCall.id`, and we now use it when present (older
//!     models omit it, so the id is synthesised then). `functionResponse` is still
//!     matched by name, so a canonical `ToolResult` is reverse-mapped through the
//!     transcript.
//!  2b. **`thoughtSignature` must be echoed back.** A Gemini 3.x function call comes
//!     with an opaque signature; replaying the call on the next turn without it is a
//!     hard `400` ("Function call is missing a thought_signature"). Verified against
//!     the live API, not inferred: the same follow-up succeeds with the signature and
//!     fails without it. It is carried in `Content::ToolUse.signature`.
//!  3. Function-call arguments arrive **complete**, not as deltas. Emitting delta
//!     events for them keeps one consumer code path.
//!  4. The system prompt is `systemInstruction`, a separate object.
//!  5. The API key goes in a header, never `?key=` — a key in a URL ends up in
//!     access logs, proxy traces and crash reports.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use crate::adapter::{json_of, AuthSpec, StreamDecoder, WireAdapter, WireRequest};
use crate::capabilities::{Capabilities, ToolChoiceSupport};
use crate::error::{Error, Result};
use crate::ir::{
    CanonicalEvent, CanonicalRequest, CanonicalResponse, Content, Role, StopReason, ToolChoice,
    Usage,
};
use crate::sse::SseFrame;

const NAME: &str = "gemini";

#[derive(Debug, Clone)]
pub struct Gemini {
    pub base_url: String,
}

impl Default for Gemini {
    fn default() -> Self {
        Gemini {
            base_url: "https://generativelanguage.googleapis.com".into(),
        }
    }
}

impl Gemini {
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
            tool_choice: ToolChoiceSupport::AutoOnly,
            stop_sequences: true,
            max_context: 1_000_000,
            max_output: 16_384,
        }
    }
}

/// id -> tool name, harvested from assistant tool calls in the transcript.
fn tool_name_index(req: &CanonicalRequest) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for msg in &req.messages {
        for c in &msg.content {
            if let Content::ToolUse { id, name, .. } = c {
                m.insert(id.clone(), name.clone());
            }
        }
    }
    m
}

fn parts_of(c: &Content, names: &HashMap<String, String>) -> Result<Value> {
    Ok(match c {
        Content::Text { text } => json!({ "text": text }),
        Content::Image { media_type, data } => json!({
            "inlineData": { "mimeType": media_type, "data": data }
        }),
        Content::ToolUse {
            name,
            input,
            signature,
            ..
        } => {
            // The signature goes back on the *same part* it arrived on, or the API
            // rejects the turn.
            let mut part = json!({ "functionCall": { "name": name, "args": input } });
            if let Some(sig) = signature {
                part["thoughtSignature"] = json!(sig);
            }
            part
        }
        Content::ToolResult {
            tool_use_id,
            content,
            ..
        } => {
            let name = names.get(tool_use_id).cloned().ok_or_else(|| Error::Config {
                detail: format!(
                    "gemini: tool result refers to unknown tool_use_id '{tool_use_id}'; \
                     Gemini matches by name so the originating call must appear earlier in the transcript"
                ),
            })?;
            let text: String = content
                .iter()
                .filter_map(|x| x.as_text())
                .collect::<Vec<_>>()
                .join("\n");
            json!({
                "functionResponse": { "name": name, "response": { "content": text } }
            })
        }
        Content::Thinking { .. } => {
            return Err(Error::unsupported(NAME, "replaying thinking blocks"))
        }
    })
}

/// Rewrite a JSON Schema into the OpenAPI subset Gemini accepts.
///
/// Gemini does not ignore unknown keywords — it fails the whole request with
/// `INVALID_ARGUMENT: Unknown name "additionalProperties"`, which our tool schemas (rightly)
/// emit. Verified live: every tool call 400s without this. So the translation is an
/// **allow-list**: known keywords are kept, the rest are dropped, recursively.
///
/// Constraints that are dropped are constraints the model was going to be asked to respect
/// anyway by the tool's own validation; dropping them costs no correctness, and the
/// alternative is a provider that cannot be given tools at all.
fn declaration_schema(schema: &Value) -> Value {
    const KEEP: &[&str] = &[
        "type",
        "format",
        "description",
        "title",
        "nullable",
        "enum",
        "items",
        "properties",
        "required",
        "anyOf",
        "minimum",
        "maximum",
        "minItems",
        "maxItems",
        "minLength",
        "maxLength",
        "default",
        "example",
    ];
    let Some(obj) = schema.as_object() else {
        return schema.clone();
    };
    let mut out = Map::new();
    for (k, v) in obj {
        if !KEEP.contains(&k.as_str()) {
            continue;
        }
        out.insert(
            k.clone(),
            match k.as_str() {
                "properties" => match v.as_object() {
                    Some(props) => Value::Object(
                        props
                            .iter()
                            .map(|(name, sub)| (name.clone(), declaration_schema(sub)))
                            .collect(),
                    ),
                    None => Value::Object(Map::new()),
                },
                "items" | "anyOf" => match v {
                    Value::Array(items) => {
                        Value::Array(items.iter().map(declaration_schema).collect())
                    }
                    other => declaration_schema(other),
                },
                _ => v.clone(),
            },
        );
    }
    Value::Object(out)
}

fn wire_role(r: Role) -> &'static str {
    match r {
        Role::Assistant => "model",
        _ => "user",
    }
}

pub struct GeminiStream {
    next_index: usize,
    /// Complete calls waiting for the end of the response.
    ///
    /// Gemini does not stream arguments: it delivers a whole `functionCall` at once, and it
    /// may deliver that call's `thoughtSignature` in a *later* chunk or on a chunk of its
    /// own. So the call is buffered and released at `finishReason`, where whatever signature
    /// arrived can still be attached. Emitting eagerly would put a signature-less call on the
    /// wire next turn, which the API rejects with a 400.
    pending: Vec<PendingCall>,
    done: bool,
}

struct PendingCall {
    index: usize,
    id: String,
    name: String,
    args: Value,
    signature: Option<String>,
}

impl GeminiStream {
    fn new() -> Self {
        GeminiStream {
            next_index: 0,
            pending: Vec::new(),
            done: false,
        }
    }

    fn flush(&mut self, out: &mut Vec<CanonicalEvent>) {
        for c in std::mem::take(&mut self.pending) {
            out.push(CanonicalEvent::ToolUseBegin {
                index: c.index,
                id: c.id,
                name: c.name,
            });
            out.push(CanonicalEvent::ToolUseInputDelta {
                index: c.index,
                fragment: c.args.to_string(),
            });
            out.push(CanonicalEvent::ToolUseEnd {
                index: c.index,
                signature: c.signature,
            });
        }
    }
}

impl StreamDecoder for GeminiStream {
    fn push_frame(&mut self, frame: &SseFrame) -> Result<Vec<CanonicalEvent>> {
        let Some(v) = json_of(NAME, frame)? else {
            return Ok(vec![]);
        };
        let mut out = Vec::new();

        if let Some(u) = v.get("usageMetadata") {
            out.push(CanonicalEvent::Usage { usage: usage_of(u) });
        }
        let cand = v
            .get("candidates")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .cloned();
        let Some(cand) = cand else { return Ok(out) };

        if let Some(parts) = cand
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
        {
            for p in parts {
                let sig = p
                    .get("thoughtSignature")
                    .and_then(|s| s.as_str())
                    .map(str::to_string);
                // Gemini 3.x marks reasoning with `"thought": true`. Emitting that as visible
                // text would put the model's scratchpad in the user's answer.
                let thinking = p.get("thought").and_then(|t| t.as_bool()).unwrap_or(false);
                if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                    if !t.is_empty() {
                        out.push(if thinking {
                            CanonicalEvent::ThinkingDelta {
                                text: t.to_string(),
                            }
                        } else {
                            CanonicalEvent::TextDelta {
                                text: t.to_string(),
                            }
                        });
                    }
                }
                if let Some(fc) = p.get("functionCall") {
                    let name = fc
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let args = fc.get("args").cloned().unwrap_or(json!({}));
                    // Gemini 3.x supplies a real id; older models do not, and downstream
                    // needs *some* stable handle to answer a call with.
                    let id = fc
                        .get("id")
                        .and_then(|i| i.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("call_{}_{}", self.next_index, name));
                    let index = self.next_index;
                    self.next_index += 1;
                    self.pending.push(PendingCall {
                        index,
                        id,
                        name,
                        args,
                        signature: sig,
                    });
                } else if let Some(sig) = sig {
                    // No call in this part: hang it on the call we are holding, if any.
                    if let Some(last) = self.pending.last_mut() {
                        last.signature.get_or_insert(sig);
                    }
                }
            }
        }

        if let Some(fr) = cand.get("finishReason").and_then(|f| f.as_str()) {
            self.flush(&mut out);
            out.push(CanonicalEvent::Stop {
                reason: stop_reason(fr),
            });
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<CanonicalEvent>> {
        let mut out = Vec::new();
        if !self.done {
            self.flush(&mut out);
            self.done = true;
        }
        out.push(CanonicalEvent::Done);
        Ok(out)
    }
}

fn usage_of(u: &Value) -> Usage {
    let n = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    Usage {
        input_tokens: n("promptTokenCount"),
        output_tokens: n("candidatesTokenCount"),
        cache_read_tokens: n("cachedContentTokenCount"),
        cache_write_tokens: 0,
        reasoning_tokens: n("thoughtsTokenCount"),
    }
}

fn stop_reason(s: &str) -> StopReason {
    match s {
        "STOP" => StopReason::EndTurn,
        "MAX_TOKENS" => StopReason::MaxTokens,
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" => StopReason::ContentFilter,
        "MALFORMED_FUNCTION_CALL" => StopReason::Unknown,
        _ => StopReason::Unknown,
    }
}

impl WireAdapter for Gemini {
    fn id(&self) -> &'static str {
        NAME
    }

    fn capabilities(&self) -> Capabilities {
        Self::capabilities()
    }

    fn build_request(&self, req: &CanonicalRequest) -> Result<WireRequest> {
        let names = tool_name_index(req);
        let mut contents: Vec<Value> = Vec::new();
        let mut pending: Option<(&'static str, Vec<Value>)> = None;

        for m in &req.messages {
            if m.role == Role::System {
                continue;
            }
            let role = wire_role(m.role);
            let mut parts: Vec<Value> = Vec::new();
            for c in &m.content {
                parts.push(parts_of(c, &names)?);
            }
            match &mut pending {
                Some((r, p)) if *r == role => p.extend(parts),
                _ => {
                    if let Some((r, p)) = pending.take() {
                        contents.push(json!({ "role": r, "parts": p }));
                    }
                    pending = Some((role, parts));
                }
            }
        }
        if let Some((r, p)) = pending.take() {
            contents.push(json!({ "role": r, "parts": p }));
        }

        let mut body = Map::new();
        body.insert("contents".into(), json!(contents));

        if let Some(sys) = system_of(req) {
            body.insert(
                "systemInstruction".into(),
                json!({ "parts": [{ "text": sys }] }),
            );
        }

        if !req.tools.is_empty() {
            let decls: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    let mut o = Map::new();
                    o.insert("name".into(), json!(t.name));
                    if let Some(d) = &t.description {
                        o.insert("description".into(), json!(d));
                    }
                    o.insert("parameters".into(), declaration_schema(&t.input_schema));
                    Value::Object(o)
                })
                .collect();
            body.insert("tools".into(), json!([{ "functionDeclarations": decls }]));

            let mode = match &req.tool_choice {
                Some(ToolChoice::None) => Some(json!({"mode":"NONE"})),
                Some(ToolChoice::Required) => Some(json!({"mode":"ANY"})),
                Some(ToolChoice::Tool { name }) => {
                    Some(json!({"mode":"ANY","allowedFunctionNames":[name]}))
                }
                Some(ToolChoice::Auto) | None => Some(json!({"mode":"AUTO"})),
            };
            if let Some(m) = mode {
                body.insert("toolConfig".into(), json!({ "functionCallingConfig": m }));
            }
        }

        let mut gc = Map::new();
        if let Some(n) = req.max_tokens {
            gc.insert("maxOutputTokens".into(), json!(n));
        }
        if let Some(t) = req.temperature {
            gc.insert("temperature".into(), json!(t));
        }
        if let Some(p) = req.top_p {
            gc.insert("topP".into(), json!(p));
        }
        if !req.stop.is_empty() {
            gc.insert("stopSequences".into(), json!(req.stop));
        }
        if req.json_mode {
            gc.insert("responseMimeType".into(), json!("application/json"));
        }
        if !gc.is_empty() {
            body.insert("generationConfig".into(), Value::Object(gc));
        }
        for (k, v) in &req.extra {
            body.insert(k.clone(), v.clone());
        }

        let stream = req.stream.unwrap_or(true);
        let base = self.base_url.trim_end_matches('/');
        let url = if stream {
            format!(
                "{base}/v1beta/models/{}:streamGenerateContent?alt=sse",
                req.model
            )
        } else {
            format!("{base}/v1beta/models/{}:generateContent", req.model)
        };

        Ok(WireRequest {
            method: "POST".to_string(),
            url,
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
            auth: AuthSpec::header("x-goog-api-key"),
            body: Value::Object(body),
        })
    }

    fn stream_decoder(&self, _req: &CanonicalRequest) -> Box<dyn StreamDecoder> {
        Box::new(GeminiStream::new())
    }

    /// `GET /v1beta/models` — the endpoint that told us `gemini-2.5-flash` was refused for
    /// new keys, back when we were still trusting a seeded table.
    fn list_models_request(&self) -> Option<crate::adapter::WireRequest> {
        Some(crate::adapter::WireRequest {
            method: "GET".to_string(),
            url: format!("{}/v1beta/models", self.base_url.trim_end_matches('/')),
            headers: vec![("accept".into(), "application/json".into())],
            auth: AuthSpec::header("x-goog-api-key"),
            body: serde_json::Value::Null,
        })
    }

    fn parse_models(&self, body: &Value) -> crate::error::Result<Vec<crate::models::ModelInfo>> {
        crate::models::parse("gemini", body)
    }

    fn parse_response(&self, req: &CanonicalRequest, body: &Value) -> Result<CanonicalResponse> {
        let cand = body
            .get("candidates")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| Error::protocol(NAME, "no candidates", &body.to_string()))?;
        let mut content = Vec::new();
        let mut n = 0usize;
        if let Some(parts) = cand
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
        {
            for p in parts {
                let sig = p
                    .get("thoughtSignature")
                    .and_then(|s| s.as_str())
                    .map(str::to_string);
                let thinking = p.get("thought").and_then(|t| t.as_bool()).unwrap_or(false);
                if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                    if !t.is_empty() {
                        content.push(if thinking {
                            Content::Thinking {
                                text: t.to_string(),
                                signature: sig.clone(),
                            }
                        } else {
                            Content::text(t)
                        });
                    }
                }
                if let Some(fc) = p.get("functionCall") {
                    let name = fc
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let id = fc
                        .get("id")
                        .and_then(|i| i.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("call_{n}_{name}"));
                    content.push(
                        Content::tool_use(id, name, fc.get("args").cloned().unwrap_or(json!({})))
                            .with_signature(sig),
                    );
                    n += 1;
                }
            }
        }
        let wants_tool = content.iter().any(|c| matches!(c, Content::ToolUse { .. }));
        Ok(CanonicalResponse {
            model: req.model.clone(),
            content,
            // A response that asks for a tool is a tool turn whatever `finishReason` says:
            // Gemini reports `STOP` for a function call, which would otherwise read as a
            // finished answer and strand the call.
            stop_reason: if wants_tool {
                StopReason::ToolUse
            } else {
                cand.get("finishReason")
                    .and_then(|f| f.as_str())
                    .map(stop_reason)
                    .unwrap_or(StopReason::EndTurn)
            },
            usage: body.get("usageMetadata").map(usage_of).unwrap_or_default(),
        })
    }
}

fn system_of(req: &CanonicalRequest) -> Option<String> {
    let mut parts = Vec::new();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Message;

    #[test]
    fn tool_result_is_matched_by_name_via_transcript() {
        let mut req = CanonicalRequest::new("gemini-flash", "go");
        req.messages.push(Message {
            role: Role::Assistant,
            content: vec![Content::ToolUse {
                id: "abc".into(),
                name: "read_file".into(),
                input: json!({"p":"x"}),
                signature: None,
            }],
        });
        req.messages.push(Message::tools(vec![Content::ToolResult {
            tool_use_id: "abc".into(),
            content: vec![Content::text("contents")],
            is_error: false,
        }]));
        let w = Gemini::default().build_request(&req).unwrap();
        let contents = w.body["contents"].as_array().unwrap();
        let last = contents.last().unwrap();
        assert_eq!(
            last["role"],
            json!("user"),
            "functionResponse must be a user turn"
        );
        assert_eq!(
            last["parts"][0]["functionResponse"]["name"],
            json!("read_file")
        );
    }

    #[test]
    fn unknown_tool_id_is_a_clear_error_not_a_bad_request() {
        let mut req = CanonicalRequest::new("gemini-flash", "go");
        req.messages.push(Message::tools(vec![Content::ToolResult {
            tool_use_id: "ghost".into(),
            content: vec![Content::text("x")],
            is_error: false,
        }]));
        let err = Gemini::default().build_request(&req).unwrap_err();
        assert!(matches!(err, Error::Config { .. }));
        assert!(err.to_string().contains("matches by name"));
    }

    /// Captured from the live API (`gemini-3.1-flash-lite`, 2026-10). The parts are
    /// verbatim: a function call with a real id and a thought signature on the same part.
    const LIVE_CALL: &str = r#"{
      "candidates": [{
        "content": {
          "parts": [
            {
              "functionCall": {"name": "read_file", "args": {"path": "src/auth.rs"}, "id": "call_856357"},
              "thoughtSignature": "EnMKcQFpFH0Tt0u7RUMu"
            }
          ],
          "role": "model"
        },
        "finishReason": "STOP",
        "index": 0
      }],
      "usageMetadata": {"promptTokenCount": 77, "candidatesTokenCount": 12, "totalTokenCount": 89},
      "modelVersion": "gemini-3.1-flash-lite"
    }"#;

    #[test]
    fn a_live_function_call_keeps_its_id_and_signature() {
        let req = CanonicalRequest::new("gemini-3.1-flash-lite", "go");
        let body: Value = serde_json::from_str(LIVE_CALL).unwrap();
        let resp = Gemini::default().parse_response(&req, &body).unwrap();

        let call = resp.content.iter().find_map(|c| match c {
            Content::ToolUse { id, signature, .. } => Some((id.clone(), signature.clone())),
            _ => None,
        });
        let (id, signature) = call.expect("a function call");
        assert_eq!(
            id, "call_856357",
            "the vendor id must be kept, not invented"
        );
        assert_eq!(signature.as_deref(), Some("EnMKcQFpFH0Tt0u7RUMu"));
        // Gemini says STOP for a call; the loop must still see a tool turn.
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn a_signed_call_is_replayed_with_its_signature_attached() {
        let mut req = CanonicalRequest::new("gemini-3.1-flash-lite", "go");
        req.messages.push(Message {
            role: Role::Assistant,
            content: vec![Content::tool_use(
                "call_856357",
                "read_file",
                json!({"path": "src/auth.rs"}),
            )
            .with_signature(Some("SIG".into()))],
        });
        req.messages.push(Message::tools(vec![Content::ToolResult {
            tool_use_id: "call_856357".into(),
            content: vec![Content::text("contents")],
            is_error: false,
        }]));

        let w = Gemini::default().build_request(&req).unwrap();
        let contents = w.body["contents"].as_array().unwrap();
        let call_turn = contents
            .iter()
            .find(|c| c["parts"][0].get("functionCall").is_some())
            .expect("the assistant turn");
        assert_eq!(call_turn["parts"][0]["thoughtSignature"], json!("SIG"));
        assert_eq!(
            call_turn["parts"][0]["functionCall"]["name"],
            json!("read_file")
        );
        let result_turn = contents
            .iter()
            .find(|c| c["parts"][0].get("functionResponse").is_some())
            .expect("the tool turn");
        assert_eq!(
            result_turn["parts"][0]["functionResponse"]["name"],
            json!("read_file")
        );
    }

    #[test]
    fn a_call_from_an_older_model_replays_without_a_signature_field() {
        let mut req = CanonicalRequest::new("gemini-2.5-flash", "go");
        req.messages.push(Message {
            role: Role::Assistant,
            content: vec![Content::tool_use(
                "call_0_read_file",
                "read_file",
                json!({"path": "x"}),
            )],
        });
        let w = Gemini::default().build_request(&req).unwrap();
        let part = &w.body["contents"][0]["parts"][0];
        assert!(
            part.get("thoughtSignature").is_none(),
            "no empty signature field: {part}"
        );
    }

    #[test]
    fn a_streamed_call_is_released_with_a_signature_that_arrives_later() {
        // Two frames, shaped like the live stream: the call first, its signature in the
        // chunk that also ends the response. The call must not be emitted before then.
        let call = r#"{"candidates":[{"content":{"parts":[
            {"functionCall":{"name":"read_file","args":{"path":"a.rs"},"id":"call_9"}}],"role":"model"},"index":0}]}"#;
        let tail = r#"{"candidates":[{"content":{"parts":[
            {"text":"","thoughtSignature":"LATE"}],"role":"model"},"finishReason":"STOP","index":0}],
            "usageMetadata":{"promptTokenCount":77,"candidatesTokenCount":12,"totalTokenCount":89}}"#;

        let mut dec = Gemini::default().stream_decoder(&CanonicalRequest::new("m", "x"));
        let mut events = Vec::new();
        for data in [call, tail] {
            let frame = SseFrame {
                event: None,
                data: data.to_string(),
                id: None,
            };
            events.extend(dec.push_frame(&frame).unwrap());
        }
        events.extend(dec.finish().unwrap());

        let end = events
            .iter()
            .find_map(|e| match e {
                CanonicalEvent::ToolUseEnd { index, signature } => {
                    Some((*index, signature.clone()))
                }
                _ => None,
            })
            .expect("the call is emitted at all");
        assert_eq!(
            end.1.as_deref(),
            Some("LATE"),
            "the signature must reach the call: {events:?}"
        );

        let begin = events
            .iter()
            .find_map(|e| match e {
                CanonicalEvent::ToolUseBegin { id, .. } => Some(id.clone()),
                _ => None,
            })
            .expect("a begin event");
        assert_eq!(begin, "call_9", "the vendor id survives streaming too");

        // A call emitted before the signature arrived could not have carried it.
        let begin_at = events
            .iter()
            .position(|e| matches!(e, CanonicalEvent::ToolUseBegin { .. }))
            .unwrap();
        let end_at = events
            .iter()
            .position(|e| matches!(e, CanonicalEvent::ToolUseEnd { .. }))
            .unwrap();
        assert!(
            begin_at > 0,
            "the call was emitted in the first frame, before its signature"
        );
        assert!(begin_at < end_at);
    }

    #[test]
    fn thought_parts_are_reasoning_not_answer_text() {
        let chunk = r#"{"candidates":[{"content":{"parts":[
            {"text":"weighing options","thought":true},{"text":"answer"}],"role":"model"},"index":0}]}"#;
        let mut dec = Gemini::default().stream_decoder(&CanonicalRequest::new("m", "x"));
        let events = dec
            .push_frame(&SseFrame {
                event: None,
                data: chunk.to_string(),
                id: None,
            })
            .unwrap();
        let thinking: Vec<&String> = events
            .iter()
            .filter_map(|e| match e {
                CanonicalEvent::ThinkingDelta { text } => Some(text),
                _ => None,
            })
            .collect();
        let text: Vec<&String> = events
            .iter()
            .filter_map(|e| match e {
                CanonicalEvent::TextDelta { text } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(thinking, vec!["weighing options"]);
        assert_eq!(
            text,
            vec!["answer"],
            "the scratchpad leaked into the answer"
        );
    }

    #[test]
    fn tool_schemas_are_rewritten_into_the_openapi_subset_gemini_accepts() {
        // The shape our registry really emits (and the API really rejects).
        let schema = json!({
            "type": "object",
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "additionalProperties": false,
            "properties": {
                "path": { "type": "string", "description": "repo-relative path" },
                "limit": { "type": "integer", "minimum": 1, "additionalProperties": false }
            },
            "required": ["path"]
        });
        let out = declaration_schema(&schema);
        assert!(
            out.get("additionalProperties").is_none(),
            "unknown keyword survived: {out}"
        );
        assert!(out.get("$schema").is_none());
        assert_eq!(
            out["properties"]["path"]["description"],
            json!("repo-relative path")
        );
        assert_eq!(out["properties"]["limit"]["minimum"], json!(1));
        assert!(out["properties"]["limit"]
            .get("additionalProperties")
            .is_none());
        assert_eq!(out["required"], json!(["path"]));
    }

    #[test]
    fn a_tool_declaration_carries_no_keywords_gemini_will_refuse() {
        let mut req = CanonicalRequest::new("gemini-3.1-flash-lite", "go");
        req.tools.push(crate::ir::ToolSpec {
            name: "read_file".into(),
            description: Some("Read a file".into()),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        });
        let w = Gemini::default().build_request(&req).unwrap();
        let params = &w.body["tools"][0]["functionDeclarations"][0]["parameters"];
        assert!(params.get("additionalProperties").is_none(), "{params}");
        assert_eq!(params["properties"]["path"]["type"], json!("string"));
    }

    #[test]
    fn key_is_header_not_query() {
        let w = Gemini::default()
            .build_request(&CanonicalRequest::new("m", "x"))
            .unwrap();
        assert_eq!(w.auth, AuthSpec::header("x-goog-api-key"));
        assert!(!w.url.contains("key="), "key must never be in the URL");
        assert!(w.url.contains("alt=sse"));
    }

    #[test]
    fn system_goes_to_system_instruction() {
        let w = Gemini::default()
            .build_request(&CanonicalRequest::new("m", "x").with_system("be brief"))
            .unwrap();
        assert_eq!(
            w.body["systemInstruction"]["parts"][0]["text"],
            json!("be brief")
        );
        let roles: Vec<String> = w.body["contents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["role"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(roles, vec!["user"]);
    }
}
