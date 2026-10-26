//! Canonical intermediate representation.
//!
//! Everything the harness wants to say to a model is expressed here exactly once;
//! every adapter translates IR <-> its vendor wire format. Providers never see each
//! other's JSON, and agent code never sees vendor JSON.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    /// Tool results. Collapsed into `user` by Anthropic, `functionResponse` by Gemini.
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text {
        text: String,
    },
    Image {
        media_type: String,
        /// Base64, no data-URI prefix.
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        /// Opaque vendor token that must be echoed back with this call on the next turn.
        ///
        /// Gemini 3.x rejects the *follow-up* request with HTTP 400 if a function call is
        /// replayed without the `thoughtSignature` it came with ("Function call is missing a
        /// thought_signature"). It is not a secret and it is not text: it is a handle to the
        /// model's reasoning at that step, and losing it silently breaks every tool loop.
        /// Providers that do not use one leave this `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<Content>,
        #[serde(default)]
        is_error: bool,
    },
    /// Extended-thinking / reasoning block. Round-tripped for Anthropic signatures.
    Thinking {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
}

impl Content {
    pub fn text(s: impl Into<String>) -> Self {
        Content::Text { text: s.into() }
    }
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text { text } => Some(text),
            _ => None,
        }
    }

    /// A tool call with no vendor signature. The common case; use
    /// [`with_signature`](Self::with_signature) when a provider hands one back.
    pub fn tool_use(id: impl Into<String>, name: impl Into<String>, input: Value) -> Self {
        Content::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
            signature: None,
        }
    }

    /// Attach a provider signature to a call, if this is a call and this provider uses them.
    pub fn with_signature(mut self, sig: Option<String>) -> Self {
        if let Content::ToolUse { signature, .. } = &mut self {
            *signature = sig;
        }
        self
    }

    pub fn signature(&self) -> Option<&str> {
        match self {
            Content::ToolUse { signature, .. } => signature.as_deref(),
            Content::Thinking { signature, .. } => signature.as_deref(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
}

impl Message {
    pub fn user(s: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: vec![Content::text(s)],
        }
    }
    pub fn assistant(s: impl Into<String>) -> Self {
        Message {
            role: Role::Assistant,
            content: vec![Content::text(s)],
        }
    }
    pub fn tools(results: Vec<Content>) -> Self {
        Message {
            role: Role::Tool,
            content: results,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the arguments object.
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Tool { name: String },
}

/// Prompt-cache intent. Adapters that support explicit breakpoints honour this;
/// providers that cache automatically ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheStrategy {
    /// No explicit caching directives.
    Off,
    /// Cache the stable prefix: system prompt + tool definitions.
    Prefix,
    /// Prefix plus the conversation up to the last user turn.
    Conversation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalRequest {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    /// Ask for a strict JSON object as the assistant's output.
    #[serde(default)]
    pub json_mode: bool,
    #[serde(default = "CacheStrategy::off")]
    pub cache: CacheStrategy,
    /// Reasoning budget hint: none|low|medium|high, provider-mapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Opaque passthrough for vendor-specific knobs; adapters forward under their own key.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl CacheStrategy {
    fn off() -> Self {
        CacheStrategy::Off
    }
}

impl CanonicalRequest {
    /// Minimal constructor: single user turn.
    pub fn new(model: impl Into<String>, user: impl Into<String>) -> Self {
        CanonicalRequest {
            model: model.into(),
            system: None,
            messages: vec![Message::user(user)],
            tools: Vec::new(),
            tool_choice: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: Vec::new(),
            json_mode: false,
            cache: CacheStrategy::Off,
            reasoning: None,
            stream: Some(true),
            extra: Map::new(),
        }
    }

    pub fn with_system(mut self, s: impl Into<String>) -> Self {
        self.system = Some(s.into());
        self
    }
    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }
    pub fn with_cache(mut self, c: CacheStrategy) -> Self {
        self.cache = c;
        self
    }

    /// Flatten every text block into one string. Used by providers with no
    /// block structure (some OpenAI-compatible servers) as a lossy fallback.
    pub fn flat_text(&self) -> String {
        let mut out = String::new();
        if let Some(s) = &self.system {
            out.push_str(s);
            out.push_str("\n\n");
        }
        for m in &self.messages {
            for c in &m.content {
                if let Some(t) = c.as_text() {
                    out.push_str(t);
                    out.push('\n');
                }
            }
        }
        out
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// Reasoning tokens, where billed/reported separately.
    #[serde(default)]
    pub reasoning_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
    /// Merge a later report into this one, taking maxima so partial stream updates
    /// (Anthropic sends input at start, output at end) converge instead of summing.
    pub fn merge_max(&mut self, other: &Usage) {
        self.input_tokens = self.input_tokens.max(other.input_tokens);
        self.output_tokens = self.output_tokens.max(other.output_tokens);
        self.cache_read_tokens = self.cache_read_tokens.max(other.cache_read_tokens);
        self.cache_write_tokens = self.cache_write_tokens.max(other.cache_write_tokens);
        self.reasoning_tokens = self.reasoning_tokens.max(other.reasoning_tokens);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    ToolUse,
    StopSequence,
    ContentFilter,
    /// Provider paused a long-running task (e.g. Anthropic `pause_turn`).
    Paused,
    Unknown,
}

/// Streaming events, normalized. A decoder must emit `Done` exactly once, last.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CanonicalEvent {
    MessageStart {
        model: String,
    },
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ToolUseBegin {
        index: usize,
        id: String,
        name: String,
    },
    /// Partial JSON for a tool's arguments; concatenate then parse at `ToolUseEnd`.
    ToolUseInputDelta {
        index: usize,
        fragment: String,
    },
    /// The call's arguments are complete. `signature` carries a vendor handle that must be
    /// replayed with the call (see [`Content::ToolUse`]).
    ToolUseEnd {
        index: usize,
        signature: Option<String>,
    },
    Usage {
        usage: Usage,
    },
    Stop {
        reason: StopReason,
    },
    Done,
    Error {
        kind: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalResponse {
    pub model: String,
    pub content: Vec<Content>,
    pub stop_reason: StopReason,
    pub usage: Usage,
}

impl CanonicalResponse {
    pub fn text(&self) -> String {
        let mut s = String::new();
        for c in &self.content {
            if let Some(t) = c.as_text() {
                s.push_str(t);
            }
        }
        s
    }
    pub fn tool_uses(&self) -> Vec<(&str, &str, &Value)> {
        self.content
            .iter()
            .filter_map(|c| match c {
                Content::ToolUse {
                    id, name, input, ..
                } => Some((id.as_str(), name.as_str(), input)),
                _ => None,
            })
            .collect()
    }
}

/// Accumulates a stream into a final response. Shared by all adapters so the
/// harness has one code path for "streaming request, non-streaming consumption".
#[derive(Debug, Default)]
pub struct StreamAccumulator {
    pub model: String,
    pub text: String,
    pub thinking: String,
    pub tool_calls: Vec<ToolCallBuf>,
    pub usage: Usage,
    pub stop: Option<StopReason>,
    pub done: bool,
    pub error: Option<(String, String)>,
}

#[derive(Debug, Default, Clone)]
pub struct ToolCallBuf {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub closed: bool,
    /// Vendor handle to replay with the call on the next turn (Gemini `thoughtSignature`).
    pub signature: Option<String>,
}

impl StreamAccumulator {
    pub fn apply(&mut self, ev: CanonicalEvent) {
        match ev {
            CanonicalEvent::MessageStart { model } => self.model = model,
            CanonicalEvent::TextDelta { text } => self.text.push_str(&text),
            CanonicalEvent::ThinkingDelta { text } => self.thinking.push_str(&text),
            CanonicalEvent::ToolUseBegin { index, id, name } => {
                self.ensure(index);
                self.tool_calls[index].id = id;
                self.tool_calls[index].name = name;
            }
            CanonicalEvent::ToolUseInputDelta { index, fragment } => {
                self.ensure(index);
                self.tool_calls[index].arguments.push_str(&fragment);
            }
            CanonicalEvent::ToolUseEnd { index, signature } => {
                self.ensure(index);
                self.tool_calls[index].closed = true;
                if signature.is_some() {
                    self.tool_calls[index].signature = signature;
                }
            }
            CanonicalEvent::Usage { usage } => self.usage.merge_max(&usage),
            CanonicalEvent::Stop { reason } => self.stop = Some(reason),
            CanonicalEvent::Done => self.done = true,
            CanonicalEvent::Error { kind, message } => self.error = Some((kind, message)),
        }
    }

    fn ensure(&mut self, index: usize) {
        while self.tool_calls.len() <= index {
            self.tool_calls.push(ToolCallBuf::default());
        }
    }

    /// Materialize the assistant message. Partial JSON in a tool call is parsed
    /// best-effort: a truncated `{}` becomes an empty object rather than an error,
    /// so a cancelled stream still yields a usable transcript.
    pub fn into_response(self, fallback_model: &str) -> CanonicalResponse {
        let mut content = Vec::new();
        if !self.thinking.is_empty() {
            content.push(Content::Thinking {
                text: self.thinking,
                signature: None,
            });
        }
        if !self.text.is_empty() {
            content.push(Content::text(self.text));
        }
        for tc in self.tool_calls {
            if tc.name.is_empty() {
                continue;
            }
            let input: Value =
                serde_json::from_str(&tc.arguments).unwrap_or_else(|_| Value::Object(Map::new()));
            content.push(Content::ToolUse {
                id: tc.id,
                name: tc.name,
                input,
                signature: None,
            });
        }
        let stop_reason = self.stop.unwrap_or_else(|| {
            if content.iter().any(|c| matches!(c, Content::ToolUse { .. })) {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            }
        });
        CanonicalResponse {
            model: if self.model.is_empty() {
                fallback_model.to_string()
            } else {
                self.model
            },
            content,
            stop_reason,
            usage: self.usage,
        }
    }
}
