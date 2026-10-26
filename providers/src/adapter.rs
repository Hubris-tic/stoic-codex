//! The one interface the harness talks to.
//!
//! `WireAdapter` is intentionally synchronous and transport-free: it turns a
//! canonical request into an HTTP request *shape* and parses response bytes.
//! Actual I/O stays in the existing `http-client` crate, which means adapters are
//! fully unit-testable with recorded cassettes and no network, no keys, no async.

use serde_json::Value;

use crate::error::Result;
use crate::ir::{CanonicalEvent, CanonicalRequest, CanonicalResponse};
use crate::sse::SseFrame;

/// How the transport should attach the credential.
///
/// Adapters never see the key. They emit an `AuthSpec` and the transport resolves
/// it from the credential store, so tokens cannot leak into adapter state, `Debug`
/// output, traces, or error messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthSpec {
    /// `Authorization: Bearer <key>` (OpenAI and most compatibles).
    Bearer,
    /// A header carrying the credential, e.g. `x-api-key` (Anthropic) or
    /// `x-goog-api-key` (Gemini).
    ///
    /// `prefix` is prepended verbatim, so a user-defined provider can express
    /// `Authorization: Key <key>` without the transport knowing the scheme. Owned
    /// rather than `&'static str` because a config-file provider supplies its own
    /// header name at runtime.
    Header {
        name: String,
        prefix: String,
    },
    /// Value passed as `?key=` — avoided where possible because keys in URLs leak
    /// into access logs and proxy traces. Only for servers that require it.
    Query {
        name: String,
    },
    None,
}

impl AuthSpec {
    /// Raw credential in `name`: the common case.
    pub fn header(name: impl Into<String>) -> Self {
        AuthSpec::Header {
            name: name.into(),
            prefix: String::new(),
        }
    }
    /// Credential with a scheme prefix, e.g. `header_prefixed("authorization", "Key ")`.
    pub fn header_prefixed(name: impl Into<String>, prefix: impl Into<String>) -> Self {
        AuthSpec::Header {
            name: name.into(),
            prefix: prefix.into(),
        }
    }
    pub fn query(name: impl Into<String>) -> Self {
        AuthSpec::Query { name: name.into() }
    }
    /// Header name for `Header`, else `None`.
    pub fn header_name(&self) -> Option<&str> {
        match self {
            AuthSpec::Header { name, .. } => Some(name),
            _ => None,
        }
    }
    /// Prefix for `Header` (empty when the raw credential is expected).
    pub fn prefix(&self) -> &str {
        match self {
            AuthSpec::Header { prefix, .. } => prefix,
            _ => "",
        }
    }
    /// Query parameter name for `Query`.
    pub fn query_name(&self) -> Option<&str> {
        match self {
            AuthSpec::Query { name } => Some(name),
            _ => None,
        }
    }
    /// How the transport must render the credential, e.g. `Bearer abc`.
    pub fn render(&self, key: &str) -> String {
        match self {
            AuthSpec::Bearer => format!("Bearer {key}"),
            AuthSpec::Header { prefix, .. } => format!("{prefix}{key}"),
            AuthSpec::Query { .. } => key.to_string(),
            AuthSpec::None => String::new(),
        }
    }
}

/// A fully described HTTP request, ready for any transport to execute.
#[derive(Debug, Clone, PartialEq)]
pub struct WireRequest {
    pub method: String,
    pub url: String,
    /// Headers excluding the credential.
    pub headers: Vec<(String, String)>,
    pub auth: AuthSpec,
    /// Serialized JSON body.
    pub body: Value,
}

impl WireRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Per-request stateful decoder. One instance per streaming request.
pub trait StreamDecoder: Send {
    /// Consume one SSE frame, produce zero or more canonical events.
    fn push_frame(&mut self, frame: &SseFrame) -> Result<Vec<CanonicalEvent>>;

    /// Called when the response body ends. Emits trailing events (e.g. `Done`).
    fn finish(&mut self) -> Result<Vec<CanonicalEvent>>;
}

pub trait WireAdapter: Send + Sync {
    /// Stable id, matches the provider id in config (`anthropic`, `openai`, ...).
    fn id(&self) -> &'static str;

    /// What this adapter's wire format can actually carry.
    ///
    /// The router gates on this instead of guessing, so adding gauntlet features (tools,
    /// streaming usage, reasoning) degrades to "not supported here" rather than to a
    /// malformed request. It is an instance method even though the answer is per *adapter
    /// family*, because a user-defined compatible (see `custom.rs`) can differ from the
    /// family it imitates.
    fn capabilities(&self) -> crate::capabilities::Capabilities;

    /// Translate IR into the provider's HTTP request.
    fn build_request(&self, req: &CanonicalRequest) -> Result<WireRequest>;

    /// Fresh decoder for a streaming response.
    fn stream_decoder(&self, req: &CanonicalRequest) -> Box<dyn StreamDecoder>;

    /// Parse a non-streaming completion body.
    fn parse_response(&self, req: &CanonicalRequest, body: &Value) -> Result<CanonicalResponse>;

    /// How to ask this provider what it serves today, or `None` if it has no catalogue
    /// endpoint. `zenithal providers models` uses this so the tool never has to believe a
    /// hardcoded list — which is exactly how `gemini-2.5-flash` came to be a 404.
    ///
    /// The returned request is a `GET` with no body; the credential is attached by the
    /// caller from the same `AuthSpec` the completion path uses.
    fn list_models_request(&self) -> Option<WireRequest> {
        None
    }

    /// Parse that catalogue. The default is the OpenAI shape, because most providers copy it.
    fn parse_models(&self, body: &Value) -> Result<Vec<crate::models::ModelInfo>> {
        crate::models::parse(self.id(), body)
    }

    /// Parse an error body into a typed `Error::Remote`.
    fn parse_error(&self, status: u16, body: &Value) -> crate::error::Error {
        let kind = body
            .get("error")
            .and_then(|e| e.get("type"))
            .and_then(|t| t.as_str())
            .or_else(|| {
                body.get("error")
                    .and_then(|e| e.get("status"))
                    .and_then(|t| t.as_str())
            })
            .unwrap_or("error")
            .to_string();
        let message = body
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .or_else(|| body.get("message").and_then(|m| m.as_str()))
            .unwrap_or("")
            .to_string();
        crate::error::Error::Remote {
            provider: self.id(),
            status,
            kind,
            message,
        }
    }
}

/// Helper: parse a JSON `data:` payload, tolerating empty frames and sentinels.
pub(crate) fn json_of<'a>(provider: &'static str, frame: &'a SseFrame) -> Result<Option<Value>> {
    let d = frame.data.trim();
    if d.is_empty() || d == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str::<Value>(d).map(Some).map_err(|e| {
        crate::error::Error::protocol(provider, format!("invalid JSON in event: {e}"), d)
    })
}
