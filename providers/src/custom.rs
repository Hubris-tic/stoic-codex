//! User-defined providers — the "any URL, any JSON" escape hatch.
//!
//! Every other adapter in this crate encodes a vendor's protocol in Rust. That is the
//! right default, but it means "my lab's inference box", "our internal gateway",
//! "a model I run with llama.cpp" require a code change. This module inverts that: the
//! user writes the provider in **data**.
//!
//! ```json
//! {
//!   "id": "lab-gateway",
//!   "base_url": "http://127.0.0.1:8080",
//!   "path": "/v1/chat/completions",
//!   "auth": { "kind": "header", "name": "x-api-key", "prefix": "" },
//!   "api_key_env": "LAB_API_KEY",
//!   "api_style": "openai",
//!   "stream": { "text_delta": "choices.0.delta.content", "done_marker": "[DONE]" },
//!   "response": { "text": "choices.0.message.content" },
//!   "models": ["my-model"]
//! }
//! ```
//!
//! ## Two levels of escape
//!
//! * `api_style: "openai"` — reuse the tested OpenAI Chat adapter, only overriding the
//!   URL, headers, auth placement and model names. Covers llama.cpp, vLLM, LM Studio,
//!   OpenRouter, Together, and most "OpenAI-compatible" gateways.
//! * `api_style: "custom"` — supply a **request template** and **response paths**. The
//!   template is JSON with `{{placeholders}}`; the paths are dotted lookups like
//!   `choices.0.delta.content`. Anything that speaks HTTP+JSON+SSE can be driven.
//!
//! ## Secrets
//!
//! A spec never contains a key. It names an **environment variable**
//! (`api_key_env`), and the transport resolves it at request time. That keeps keys out
//! of config files that get committed, out of `Debug` output, and out of the IR.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::adapter::{AuthSpec, StreamDecoder, WireAdapter, WireRequest};
use crate::capabilities::Capabilities;
use crate::error::{Error, Result};
use crate::ir::{
    CanonicalEvent, CanonicalRequest, CanonicalResponse, Content, Role, StopReason, ToolChoice,
    Usage,
};

// ─────────────────────────────────────────────────────────────────────────────
// Interning
// ─────────────────────────────────────────────────────────────────────────────

/// `WireAdapter::id` and `Error` both carry `&'static str` so that idioms like
/// `Error::Remote { provider: "anthropic", .. }` stay allocation-free. A provider defined
/// in a config file has a runtime name, so it is interned once at construction.
///
/// This is a bounded, one-shot leak: at most a handful of strings per configured
/// provider, for the life of the process — the same lifetime a compiled-in literal has.
fn intern(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

// ─────────────────────────────────────────────────────────────────────────────
// Spec
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApiStyle {
    /// OpenAI Chat Completions shape, via the built-in adapter.
    #[default]
    Openai,
    /// Template + path mapping, fully user-defined.
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthKind {
    #[default]
    None,
    Bearer,
    /// Credential goes in a header. `prefix` is prepended verbatim, e.g. `"Key "`.
    Header {
        name: String,
        #[serde(default)]
        prefix: String,
    },
    /// Credential goes in a query parameter. Avoid: URLs land in logs.
    Query {
        name: String,
    },
}

impl AuthKind {
    pub fn to_auth_spec(&self) -> AuthSpec {
        match self {
            AuthKind::None => AuthSpec::None,
            AuthKind::Bearer => AuthSpec::Bearer,
            AuthKind::Header { name, prefix } => {
                AuthSpec::header_prefixed(name.clone(), prefix.clone())
            }
            AuthKind::Query { name } => AuthSpec::query(name.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomSpec {
    /// Provider id. Must be unique; appears in `WireRequest`, errors and telemetry.
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    /// Origin only, e.g. `http://127.0.0.1:8080`. No path, no trailing slash.
    pub base_url: String,
    /// Path appended to `base_url`, e.g. `/v1/chat/completions`.
    #[serde(default = "default_path")]
    pub path: String,
    #[serde(default)]
    pub auth: AuthKind,
    /// Extra headers, sent as-is. Never put a key here — use `api_key_env`.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Environment variable holding the key.
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_style: ApiStyle,
    /// Request body template. Only used when `api_style = custom`.
    #[serde(default)]
    pub request: Map<String, Value>,
    #[serde(default)]
    pub response: ResponseMap,
    #[serde(default)]
    pub stream: StreamMap,
    /// Vendor stop reason -> canonical, e.g. `{"length": "max_tokens"}`.
    #[serde(default)]
    pub stop_reasons: BTreeMap<String, String>,
    /// Models this provider serves, for the registry/UI.
    #[serde(default)]
    pub models: Vec<String>,
    /// Runs on the user's machine: free, private, no key needed.
    #[serde(default)]
    pub local: bool,
    /// What this endpoint can actually do, when the default inference is wrong.
    ///
    /// A user-defined provider is a claim about somebody else's server, and the honest
    /// position is "unknown unless declared". Left out, it is inferred from `api_style`
    /// (an OpenAI-shaped endpoint gets tools and streaming; a templated one gets the
    /// conservative minimum). Declare `"capabilities": {"tools": false}` when the server
    /// chokes on tool schemas — the router will then stop sending them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Capabilities>,
}

fn default_path() -> String {
    "/v1/chat/completions".to_string()
}

impl CustomSpec {
    /// Parse and validate. Every user error surfaces here, at load time, with the
    /// offending field named — not as a mystery 404 during a session.
    pub fn from_json(text: &str) -> Result<Self> {
        let mut spec: CustomSpec = serde_json::from_str(text).map_err(|e| Error::Config {
            detail: format!("provider spec is not valid JSON: {e}"),
        })?;
        spec.validate()?;
        spec.apply_style_defaults();
        Ok(spec)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Fill in what the chosen `api_style` implies, leaving anything the user set alone.
    ///
    /// Without this a spec that says `"api_style": "openai"` inherited the OpenAI *body*
    /// but not its usage or stop paths, so a correct turn reported `0 in / 0 out tokens` —
    /// the numbers are there in the payload, we simply were not looking for them.
    pub fn apply_style_defaults(&mut self) {
        if self.api_style != ApiStyle::Openai {
            return;
        }
        let usage_defaults = [("usage.prompt_tokens", "usage.completion_tokens")];
        let (input, output) = usage_defaults[0];
        if self.stream.usage.input.is_none() {
            self.stream.usage.input = Some(input.to_string());
        }
        if self.stream.usage.output.is_none() {
            self.stream.usage.output = Some(output.to_string());
        }
        if self.stream.stop.is_none() {
            self.stream.stop = Some("choices.0.finish_reason".to_string());
        }
        if self.response.usage.input.is_none() {
            self.response.usage.input = Some(input.to_string());
        }
        if self.response.usage.output.is_none() {
            self.response.usage.output = Some(output.to_string());
        }
        if self.response.stop.is_none() {
            self.response.stop = Some("choices.0.finish_reason".to_string());
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(Error::Config {
                detail: "provider spec needs a non-empty `id`".into(),
            });
        }
        if !self
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(Error::Config {
                detail: format!("provider id `{}` must be [a-zA-Z0-9_-]", self.id),
            });
        }
        if self.base_url.trim().is_empty() {
            return Err(Error::Config {
                detail: format!("provider `{}`: `base_url` is required", self.id),
            });
        }
        if !(self.base_url.starts_with("http://") || self.base_url.starts_with("https://")) {
            return Err(Error::Config {
                detail: format!(
                    "provider `{}`: `base_url` must start with http:// or https:// (got `{}`)",
                    self.id, self.base_url
                ),
            });
        }
        if self.base_url.ends_with('/') {
            return Err(Error::Config {
                detail: format!(
                    "provider `{}`: `base_url` must not end with '/' (got `{}`)",
                    self.id, self.base_url
                ),
            });
        }
        if let AuthKind::Header { name, prefix } = &self.auth {
            if name.trim().is_empty() {
                return Err(Error::Config {
                    detail: format!("provider `{}`: auth header needs a `name`", self.id),
                });
            }
            // `Authorization` is fine when it carries a scheme prefix
            // (`Authorization: Key <key>`); a bare credential in Authorization is what
            // the `bearer` kind is for, and saying so beats guessing.
            if name.eq_ignore_ascii_case("authorization") && prefix.trim().is_empty() {
                return Err(Error::Config {
                    detail: format!(
                        "provider `{}`: use auth kind `bearer` for a bare Authorization header, \
                         or give this one a `prefix` such as \"Key \"",
                        self.id
                    ),
                });
            }
        }
        // A key in the spec would end up in a committed file and in `Debug` output.
        for key in self.headers.keys() {
            let k = key.to_ascii_lowercase();
            if k.contains("authorization")
                || k.contains("api-key")
                || k.contains("api_key")
                || k == "x-key"
            {
                return Err(Error::Config {
                    detail: format!(
                        "provider `{}`: header `{key}` looks like a credential. Put the key in an \
                         environment variable and reference it with `api_key_env`.",
                        self.id
                    ),
                });
            }
        }
        if let Some(env) = &self.api_key_env {
            if env.trim().is_empty() {
                return Err(Error::Config {
                    detail: format!("provider `{}`: `api_key_env` is empty", self.id),
                });
            }
        }
        // `base_url` is an origin, `path` is the endpoint. A spec that puts `/v1` in both
        // silently requests `/v1/v1/chat/completions`, which every server answers with a
        // 404 and nobody enjoys debugging.
        let trimmed_base = self.base_url.trim_end_matches('/');
        if let Some(last) = trimmed_base.rsplit('/').next() {
            if last.starts_with('v')
                && last[1..].chars().all(|c| c.is_ascii_digit())
                && !last[1..].is_empty()
                && self.path.starts_with(&format!("/{last}"))
            {
                return Err(Error::Config {
                    detail: format!(
                        "provider `{}`: `base_url` already ends in `/{last}` and `path` also starts with `/{last}`, so the request would go to /{last}/{last}/… — drop the version from `base_url`",
                        self.id
                    ),
                });
            }
        }
        if self.api_style == ApiStyle::Custom && self.request.is_empty() {
            return Err(Error::Config {
                detail: format!(
                    "provider `{}`: `api_style: custom` needs a `request` template \
                     (see providers/examples/custom_provider.json)",
                    self.id
                ),
            });
        }
        Ok(())
    }

    pub fn endpoint(&self) -> String {
        format!(
            "{}{}",
            self.base_url,
            if self.path.starts_with('/') {
                self.path.clone()
            } else {
                format!("/{}", self.path)
            }
        )
    }
}

/// Load every `*.json` provider spec in a directory (typically `~/.zenithal/providers/`).
///
/// Sorted by file name so load order — and therefore the router's tie-breaking — is
/// deterministic. A malformed file is reported with its path; it is never skipped
/// silently, because "my provider just does not show up" is the worst failure mode here.
pub fn load_dir(dir: &std::path::Path) -> Result<Vec<CustomProvider>> {
    let mut paths: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(Error::Config {
                detail: format!("cannot read provider directory {}: {e}", dir.display()),
            })
        }
    };
    paths.sort();
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let text = std::fs::read_to_string(&path).map_err(|e| Error::Config {
            detail: format!("cannot read {}: {e}", path.display()),
        })?;
        let provider = CustomProvider::from_json(&text).map_err(|e| Error::Config {
            detail: format!("{}: {e}", path.display()),
        })?;
        out.push(provider);
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// Response / stream mapping
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageMap {
    #[serde(default)]
    pub input: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub cache_read: Option<String>,
    #[serde(default)]
    pub cache_write: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
}

impl UsageMap {
    fn extract(&self, body: &Value) -> Usage {
        Usage {
            input_tokens: self
                .input
                .as_deref()
                .and_then(|p| u64_at(body, p))
                .unwrap_or(0),
            output_tokens: self
                .output
                .as_deref()
                .and_then(|p| u64_at(body, p))
                .unwrap_or(0),
            cache_read_tokens: self
                .cache_read
                .as_deref()
                .and_then(|p| u64_at(body, p))
                .unwrap_or(0),
            cache_write_tokens: self
                .cache_write
                .as_deref()
                .and_then(|p| u64_at(body, p))
                .unwrap_or(0),
            reasoning_tokens: self
                .reasoning
                .as_deref()
                .and_then(|p| u64_at(body, p))
                .unwrap_or(0),
        }
    }
}

fn default_text_path() -> Option<String> {
    Some("choices.0.message.content".to_string())
}
fn default_model_path() -> Option<String> {
    Some("model".to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseMap {
    #[serde(default = "default_text_path")]
    pub text: Option<String>,
    #[serde(default = "default_model_path")]
    pub model: Option<String>,
    #[serde(default)]
    pub stop: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<String>,
    #[serde(default)]
    pub usage: UsageMap,
}

impl Default for ResponseMap {
    fn default() -> Self {
        ResponseMap {
            text: default_text_path(),
            model: default_model_path(),
            stop: None,
            tool_calls: Some("choices.0.message.tool_calls".to_string()),
            usage: UsageMap::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolDeltaMap {
    #[serde(default)]
    pub index: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

fn default_error_path() -> Option<String> {
    Some("error.type".to_string())
}

fn default_error_message_path() -> Option<String> {
    Some("error.message".to_string())
}

fn default_done_marker() -> Option<String> {
    Some("[DONE]".to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamMap {
    /// Path to the incremental text of this event.
    #[serde(default)]
    pub text_delta: Option<String>,
    #[serde(default)]
    pub thinking_delta: Option<String>,
    /// Where a stream reports a failure. Defaults are filled in per field, so a spec
    /// that customises `text_delta` only still gets error handling.
    #[serde(default = "default_error_path")]
    pub error: Option<String>,
    #[serde(default = "default_error_message_path")]
    pub error_message: Option<String>,
    #[serde(default)]
    pub stop: Option<String>,
    #[serde(default)]
    pub tool_call: Option<ToolDeltaMap>,
    #[serde(default)]
    pub usage: UsageMap,
    /// Raw `data:` payload that terminates the stream.
    #[serde(default = "default_done_marker")]
    pub done_marker: Option<String>,
}

impl Default for StreamMap {
    fn default() -> Self {
        StreamMap {
            text_delta: Some("choices.0.delta.content".to_string()),
            thinking_delta: None,
            error: default_error_path(),
            error_message: default_error_message_path(),
            stop: None,
            tool_call: None,
            usage: UsageMap::default(),
            done_marker: default_done_marker(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Path resolution
// ─────────────────────────────────────────────────────────────────────────────

/// Dotted lookup: `choices.0.delta.content`. Array indices are decimal segments.
/// Returns `None` for any missing link rather than panicking — provider payloads drift,
/// and a missing field must be a "no data" outcome, not a crash.
pub fn resolve<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return None;
    }
    let mut cur = value;
    for seg in path.split('.') {
        if seg.is_empty() {
            return None;
        }
        cur = match cur {
            Value::Object(m) => m.get(seg)?,
            Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

pub fn string_at(value: &Value, path: &str) -> Option<String> {
    match resolve(value, path)? {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        // Numbers/booleans as strings: some APIs send `"finish_reason": 2`.
        other => Some(other.to_string().trim_matches('"').to_string()),
    }
}

pub fn u64_at(value: &Value, path: &str) -> Option<u64> {
    match resolve(value, path)? {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Request templating
// ─────────────────────────────────────────────────────────────────────────────

/// Values a template can interpolate. Kept explicit so an unknown placeholder can be
/// rejected instead of silently rendering to an empty string.
struct Template<'a> {
    req: &'a CanonicalRequest,
}

impl<'a> Template<'a> {
    fn value(&self, name: &str) -> Option<Value> {
        let req = self.req;
        Some(match name {
            "model" => Value::String(req.model.clone()),
            "system" => Value::String(req.system.clone().unwrap_or_default()),
            "text" => Value::String(req.flat_text()),
            "messages" => default_messages(req),
            "tools" => default_tools(req),
            "tool_choice" => default_tool_choice(req),
            "max_tokens" => match req.max_tokens {
                Some(n) => Value::from(n),
                None => Value::Null,
            },
            "temperature" => match req.temperature {
                Some(t) => Value::from(t),
                None => Value::Null,
            },
            "top_p" => match req.top_p {
                Some(t) => Value::from(t),
                None => Value::Null,
            },
            "stop" => Value::Array(req.stop.iter().cloned().map(Value::String).collect()),
            "stream" => Value::Bool(req.stream.unwrap_or(true)),
            "json_mode" => Value::Bool(req.json_mode),
            _ => return None,
        })
    }
}

/// The message shape a custom provider sees. Deliberately the OpenAI shape, because it
/// is the de-facto lingua franca and therefore the least surprising default; a provider
/// that wants something else writes its own template with `{{text}}`.
fn default_messages(req: &CanonicalRequest) -> Value {
    let mut out: Vec<Value> = Vec::new();
    if let Some(system) = &req.system {
        out.push(serde_json::json!({ "role": "system", "content": system }));
    }
    for m in &req.messages {
        let role = match m.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let mut texts: Vec<String> = Vec::new();
        let mut parts: Vec<Value> = Vec::new();
        for c in &m.content {
            match c {
                Content::Text { text } => {
                    texts.push(text.clone());
                    parts.push(serde_json::json!({ "type": "text", "text": text }));
                }
                Content::Thinking { text, .. } => {
                    // Thinking blocks are not portable; they are dropped rather than
                    // leaked into a provider that did not produce them.
                    let _ = text;
                }
                Content::Image { media_type, data } => {
                    let url = format!("data:{media_type};base64,{data}");
                    parts.push(
                        serde_json::json!({ "type": "image_url", "image_url": { "url": url } }),
                    );
                }
                Content::ToolUse {
                    id, name, input, ..
                } => {
                    parts.push(serde_json::json!({
                        "type": "tool_use", "id": id, "name": name, "input": input
                    }));
                    texts.push(format!("[tool_use {name} {id}] {input}"));
                }
                Content::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let body = content
                        .iter()
                        .filter_map(|c| c.as_text())
                        .collect::<Vec<_>>()
                        .join("\n");
                    parts.push(serde_json::json!({
                        "type": "tool_result", "tool_use_id": tool_use_id,
                        "content": body, "is_error": is_error
                    }));
                    texts.push(body);
                }
            }
        }
        let plain = texts.join("\n");
        // Multi-part content only when there is something other than text in it.
        let content = if parts.len() > 1 {
            Value::Array(parts)
        } else {
            Value::String(plain)
        };
        out.push(serde_json::json!({ "role": role, "content": content }));
    }
    Value::Array(out)
}

fn default_tools(req: &CanonicalRequest) -> Value {
    Value::Array(
        req.tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description.clone().unwrap_or_default(),
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect(),
    )
}

fn default_tool_choice(req: &CanonicalRequest) -> Value {
    match &req.tool_choice {
        None => Value::Null,
        Some(ToolChoice::Auto) => Value::String("auto".into()),
        Some(ToolChoice::None) => Value::String("none".into()),
        Some(ToolChoice::Required) => Value::String("required".into()),
        Some(ToolChoice::Tool { name }) => {
            serde_json::json!({ "type": "function", "function": { "name": name } })
        }
    }
}

/// Render a JSON template. Rule, stated once so it is predictable:
///
/// * a string that is **exactly** `{{name}}` is replaced by that value's raw JSON
///   (so `"max_tokens": "{{max_tokens}}"` becomes a number, not `"4096"`);
/// * a placeholder **inside** a longer string is replaced by its text form;
/// * an unknown name is an error, because a typo that renders to "" sends a broken
///   request that is very hard to diagnose from a 400.
pub fn render(template: &Value, req: &CanonicalRequest) -> Result<Value> {
    let t = Template { req };
    render_value(template, &t)
}

fn render_value(template: &Value, ctx: &Template<'_>) -> Result<Value> {
    match template {
        Value::String(s) => render_string(s, ctx),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(render_value(item, ctx)?);
            }
            Ok(Value::Array(out))
        }
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                out.insert(k.clone(), render_value(v, ctx)?);
            }
            Ok(Value::Object(out))
        }
        other => Ok(other.clone()),
    }
}

fn render_string(template: &str, ctx: &Template<'_>) -> Result<Value> {
    // Whole-string placeholder: substitute the raw JSON value.
    if let Some(name) = whole_placeholder(template) {
        return ctx.value(name).ok_or_else(|| Error::Config {
            detail: format!("unknown template placeholder `{{{{{name}}}}}`"),
        });
    }
    if !template.contains("{{") {
        return Ok(Value::String(template.to_string()));
    }
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(Error::Config {
                detail: format!("unclosed `{{{{` in template: `{template}`"),
            });
        };
        let name = &after[..end];
        let v = ctx.value(name.trim()).ok_or_else(|| Error::Config {
            detail: format!("unknown template placeholder `{{{{{name}}}}}`"),
        })?;
        out.push_str(&as_text(&v));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(Value::String(out))
}

fn whole_placeholder(s: &str) -> Option<&str> {
    let inner = s.strip_prefix("{{")?.strip_suffix("}}")?;
    if inner.contains("{{") || inner.contains("}}") {
        return None;
    }
    Some(inner.trim())
}

fn as_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The provider
// ─────────────────────────────────────────────────────────────────────────────

pub struct CustomProvider {
    spec: CustomSpec,
    /// Interned once so `id()` and every `Error` can borrow a `&'static str`.
    id: &'static str,
}

impl CustomSpec {
    /// The capabilities this endpoint has: declared, or inferred from its shape.
    pub fn capabilities(&self) -> Capabilities {
        if let Some(c) = self.capabilities.clone() {
            return c;
        }
        match self.api_style {
            // An OpenAI-shaped endpoint is the compatible case: tools and streaming are
            // near-universal, but usage in the stream is opt-in and often missing, so it
            // stays off rather than producing a silent zero-token response.
            ApiStyle::Openai => Capabilities {
                tools: true,
                streaming: true,
                stream_usage: false,
                stop_sequences: true,
                ..Capabilities::default()
            },
            // A template we have never seen: promise nothing, ask for nothing.
            ApiStyle::Custom => Capabilities {
                streaming: true,
                ..Capabilities::default()
            },
        }
    }
}

impl std::fmt::Debug for CustomProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print headers or keys — only what identifies the endpoint.
        f.debug_struct("CustomProvider")
            .field("id", &self.spec.id)
            .field("endpoint", &self.spec.endpoint())
            .field("api_style", &self.spec.api_style)
            .finish()
    }
}

impl CustomProvider {
    pub fn new(spec: CustomSpec) -> Result<Self> {
        spec.validate()?;
        let id = intern(&spec.id);
        Ok(CustomProvider { spec, id })
    }

    pub fn from_json(text: &str) -> Result<Self> {
        CustomProvider::new(CustomSpec::from_json(text)?)
    }

    pub fn spec(&self) -> &CustomSpec {
        &self.spec
    }

    fn stop_reason(&self, raw: Option<&str>) -> StopReason {
        let Some(raw) = raw else {
            return StopReason::Unknown;
        };
        if let Some(mapped) = self.spec.stop_reasons.get(raw) {
            return canonical_stop(mapped);
        }
        canonical_stop(raw)
    }
}

fn canonical_stop(raw: &str) -> StopReason {
    match raw {
        "end_turn" | "stop" | "stop_sequence" | "eos" => StopReason::EndTurn,
        "max_tokens" | "length" => StopReason::MaxTokens,
        "tool_use" | "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" | "safety" => StopReason::ContentFilter,
        "paused" | "pause_turn" => StopReason::Paused,
        _ => StopReason::Unknown,
    }
}

impl WireAdapter for CustomProvider {
    fn id(&self) -> &'static str {
        self.id
    }

    fn capabilities(&self) -> Capabilities {
        self.spec.capabilities()
    }

    fn build_request(&self, req: &CanonicalRequest) -> Result<WireRequest> {
        let body = if self.spec.api_style == ApiStyle::Openai {
            // Reuse the tested OpenAI Chat body builder rather than a second,
            // subtly-different implementation. Only the body is borrowed: URL, auth and
            // headers come from this spec, so the user controls where it goes.
            let mut chat = crate::adapters::openai_chat::OpenAiChat::openai();
            chat.id = self.id;
            chat.base_url = self.spec.base_url.clone();
            chat.build_request(req)?.body
        } else {
            render(&Value::Object(self.spec.request.clone()), req)?
        };

        let mut headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("accept".to_string(), "text/event-stream".to_string()),
        ];
        for (k, v) in &self.spec.headers {
            headers.push((k.clone(), v.clone()));
        }

        Ok(WireRequest {
            method: "POST".to_string(),
            url: self.spec.endpoint(),
            headers,
            auth: self.spec.auth.to_auth_spec(),
            body,
        })
    }

    fn stream_decoder(&self, req: &CanonicalRequest) -> Box<dyn StreamDecoder> {
        Box::new(CustomStreamDecoder {
            text: self.spec.stream.text_delta.clone(),
            thinking: self.spec.stream.thinking_delta.clone(),
            error: self.spec.stream.error.clone(),
            error_message: self.spec.stream.error_message.clone(),
            stop: self.spec.stream.stop.clone(),
            tool: self.spec.stream.tool_call.clone(),
            usage: self.spec.stream.usage.clone(),
            done_marker: self.spec.stream.done_marker.clone(),
            stop_map: self.spec.stop_reasons.clone(),
            model: req.model.clone(),
            sent_start: false,
            done: false,
            open_tools: Vec::new(),
            stop_reason: None,
        })
    }

    fn list_models_request(&self) -> Option<WireRequest> {
        // For an OpenAI-compatible `/v1/chat/completions` endpoint, the matching
        // catalogue is `/v1/models`. Do not guess a route for arbitrary custom APIs.
        if self.spec.api_style != ApiStyle::Openai {
            return None;
        }
        let path = self.spec.path.trim_end_matches('/');
        let prefix = path.strip_suffix("/chat/completions")?;
        let url = format!(
            "{}{}{}",
            self.spec.base_url.trim_end_matches('/'),
            prefix,
            "/models"
        );
        let mut headers = vec![("accept".to_string(), "application/json".to_string())];
        headers.extend(
            self.spec
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        Some(WireRequest {
            method: "GET".to_string(),
            url,
            headers,
            auth: self.spec.auth.to_auth_spec(),
            body: Value::Null,
        })
    }

    fn parse_response(&self, req: &CanonicalRequest, body: &Value) -> Result<CanonicalResponse> {
        let map = &self.spec.response;
        let text = map.text.as_deref().and_then(|p| string_at(body, p));
        let mut content = Vec::new();
        if let Some(text) = text {
            if !text.is_empty() {
                content.push(Content::text(text));
            }
        }
        if let Some(path) = &map.tool_calls {
            if let Some(Value::Array(calls)) = resolve(body, path) {
                for (i, call) in calls.iter().enumerate() {
                    let name = string_at(call, "function.name")
                        .or_else(|| string_at(call, "name"))
                        .unwrap_or_default();
                    if name.is_empty() {
                        continue;
                    }
                    let id = string_at(call, "id").unwrap_or_else(|| format!("call_{i}"));
                    let args_raw = string_at(call, "function.arguments")
                        .or_else(|| resolve(call, "function.arguments").map(|v| v.to_string()))
                        .or_else(|| resolve(call, "input").map(|v| v.to_string()))
                        .unwrap_or_else(|| "{}".to_string());
                    let input = serde_json::from_str(&args_raw)
                        .unwrap_or_else(|_| Value::Object(Map::new()));
                    content.push(Content::ToolUse {
                        id,
                        name,
                        input,
                        signature: None,
                    });
                }
            }
        }
        if content.is_empty() {
            return Err(Error::protocol(
                self.id,
                format!(
                    "no text found at `{}` — check the spec's `response.text` path",
                    map.text.as_deref().unwrap_or("<unset>")
                ),
                &body.to_string(),
            ));
        }
        let raw_stop = map.stop.as_deref().and_then(|p| string_at(body, p));
        let stop_reason = if content.iter().any(|c| matches!(c, Content::ToolUse { .. })) {
            StopReason::ToolUse
        } else {
            self.stop_reason(raw_stop.as_deref())
        };
        Ok(CanonicalResponse {
            model: map
                .model
                .as_deref()
                .and_then(|p| string_at(body, p))
                .unwrap_or_else(|| req.model.clone()),
            content,
            stop_reason,
            usage: map.usage.extract(body),
        })
    }

    fn parse_error(&self, status: u16, body: &Value) -> Error {
        let kind = self
            .spec
            .stream
            .error
            .as_deref()
            .and_then(|p| string_at(body, p))
            .or_else(|| string_at(body, "error.type"))
            .or_else(|| string_at(body, "error.code"))
            .unwrap_or_else(|| "error".to_string());
        let message = self
            .spec
            .stream
            .error_message
            .as_deref()
            .and_then(|p| string_at(body, p))
            .or_else(|| string_at(body, "error.message"))
            .or_else(|| string_at(body, "message"))
            .unwrap_or_default();
        Error::Remote {
            provider: self.id,
            status,
            kind,
            message,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Streaming
// ─────────────────────────────────────────────────────────────────────────────

pub struct CustomStreamDecoder {
    text: Option<String>,
    thinking: Option<String>,
    error: Option<String>,
    error_message: Option<String>,
    stop: Option<String>,
    tool: Option<ToolDeltaMap>,
    usage: UsageMap,
    done_marker: Option<String>,
    stop_map: BTreeMap<String, String>,
    model: String,
    sent_start: bool,
    done: bool,
    open_tools: Vec<usize>,
    stop_reason: Option<StopReason>,
}

impl CustomStreamDecoder {
    fn stop_reason(&self, raw: &str) -> StopReason {
        match self.stop_map.get(raw) {
            Some(m) => canonical_stop(m),
            None => canonical_stop(raw),
        }
    }
}

impl StreamDecoder for CustomStreamDecoder {
    fn push_frame(&mut self, frame: &crate::sse::SseFrame) -> Result<Vec<CanonicalEvent>> {
        let mut out = Vec::new();
        if self.done {
            // Anything after Done is ignored rather than decoded into a second terminator.
            return Ok(out);
        }
        if let Some(marker) = &self.done_marker {
            if frame.data.trim() == marker {
                self.done = true;
                out.push(CanonicalEvent::Done);
                return Ok(out);
            }
        }
        let payload: Value = if frame.data.trim().is_empty() {
            Value::Null
        } else {
            match serde_json::from_str(&frame.data) {
                Ok(v) => v,
                // Some gateways send bare text deltas with no JSON wrapper.
                Err(_) => Value::String(frame.data.clone()),
            }
        };

        if !self.sent_start {
            self.sent_start = true;
            out.push(CanonicalEvent::MessageStart {
                model: self.model.clone(),
            });
        }

        // Errors first: a stream that carries an error must not also emit content.
        if let Some(path) = &self.error {
            if let Some(kind) = string_at(&payload, path) {
                if !kind.is_empty() {
                    let message = self
                        .error_message
                        .as_deref()
                        .and_then(|p| string_at(&payload, p))
                        .unwrap_or_default();
                    out.push(CanonicalEvent::Error { kind, message });
                    self.done = true;
                    out.push(CanonicalEvent::Done);
                    return Ok(out);
                }
            }
        }

        if let Some(path) = &self.thinking {
            if let Some(t) = string_at(&payload, path) {
                if !t.is_empty() {
                    out.push(CanonicalEvent::ThinkingDelta { text: t });
                }
            }
        }

        if let Some(path) = &self.text {
            if let Some(t) = string_at(&payload, path) {
                if !t.is_empty() {
                    out.push(CanonicalEvent::TextDelta { text: t });
                }
            }
        }

        if let Some(map) = &self.tool {
            let index = map
                .index
                .as_deref()
                .and_then(|p| u64_at(&payload, p))
                .map(|n| n as usize)
                .unwrap_or(0);
            let id = map.id.as_deref().and_then(|p| string_at(&payload, p));
            let name = map.name.as_deref().and_then(|p| string_at(&payload, p));
            if let Some(name) = name.filter(|n| !n.is_empty()) {
                if !self.open_tools.contains(&index) {
                    self.open_tools.push(index);
                    out.push(CanonicalEvent::ToolUseBegin {
                        index,
                        id: id.clone().unwrap_or_else(|| format!("call_{index}")),
                        name,
                    });
                }
            }
            if let Some(args) = map
                .arguments
                .as_deref()
                .and_then(|p| string_at(&payload, p))
            {
                if !args.is_empty() {
                    if !self.open_tools.contains(&index) {
                        self.open_tools.push(index);
                        out.push(CanonicalEvent::ToolUseBegin {
                            index,
                            id: id.clone().unwrap_or_else(|| format!("call_{index}")),
                            name: String::new(),
                        });
                    }
                    out.push(CanonicalEvent::ToolUseInputDelta {
                        index,
                        fragment: args,
                    });
                }
            }
        }

        let usage = self.usage.extract(&payload);
        if usage.total() > 0 || usage.cache_read_tokens > 0 || usage.cache_write_tokens > 0 {
            out.push(CanonicalEvent::Usage { usage });
        }

        if let Some(path) = &self.stop {
            if let Some(raw) = string_at(&payload, path) {
                if !raw.is_empty() {
                    let reason = self.stop_reason(&raw);
                    self.stop_reason = Some(reason);
                    out.push(CanonicalEvent::Stop { reason });
                }
            }
        }

        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<CanonicalEvent>> {
        let mut out = Vec::new();
        for index in std::mem::take(&mut self.open_tools) {
            out.push(CanonicalEvent::ToolUseEnd {
                index,
                signature: None,
            });
        }
        if !self.done {
            let reason = self.stop_reason.unwrap_or(StopReason::EndTurn);
            out.push(CanonicalEvent::Stop { reason });
            out.push(CanonicalEvent::Done);
            self.done = true;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("zenithal-providers-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_directory_is_empty_not_an_error() {
        let dir = std::env::temp_dir().join("zenithal-providers-does-not-exist");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_dir(&dir).unwrap().is_empty());
    }

    #[test]
    fn loads_every_json_spec_in_name_order() {
        let dir = temp_dir("load");
        std::fs::write(
            dir.join("b.json"),
            r#"{"id":"second","base_url":"http://127.0.0.1:2"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("a.json"),
            r#"{"id":"first","base_url":"http://127.0.0.1:1"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let loaded = load_dir(&dir).unwrap();
        let ids: Vec<&str> = loaded.iter().map(|p| p.id()).collect();
        assert_eq!(
            ids,
            vec!["first", "second"],
            "load order must be deterministic"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_spec_names_its_file() {
        let dir = temp_dir("broken");
        std::fs::write(dir.join("bad.json"), "{ not json").unwrap();
        let err = load_dir(&dir).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("bad.json"),
            "the failing file must be named: {msg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
