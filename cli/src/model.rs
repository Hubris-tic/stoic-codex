//! The bridge from `zenithal-agent`'s `Model` trait to a real provider over the network.
//!
//! The agent loop asks for one complete response per turn; this type turns that into a
//! `WireRequest` through the chosen adapter, puts it on a socket, and parses the answer
//! back into the canonical IR. Everything vendor-specific is still the adapter's business —
//! this file never reads a vendor JSON field.

use std::time::Duration;

use serde_json::Value;
use zenithal_agent::{Model, ModelError};
use zenithal_providers::{
    adapter_for, AuthSpec, CanonicalRequest, CanonicalResponse, SseDecoder, WireAdapter,
};

use crate::cmd::run::{reroute, resolve_adapter, resolve_credential, to_http_request};
use crate::config::Config;
use crate::net::{HttpRequest, LiveTransport};
use crate::paths::Paths;

pub struct RemoteModel {
    adapter: Box<dyn WireAdapter>,
    transport: LiveTransport,
    provider: String,
    key: Option<String>,
    auth: AuthSpec,
    base_url: Option<String>,
    model_override: Option<String>,
}

impl RemoteModel {
    /// Resolve everything a request needs *once*, at construction: the adapter, the
    /// credential, and any base-URL override. A squad builds one of these per member.
    pub fn new(
        provider: &str,
        paths: &Paths,
        cfg: &Config,
        model_override: Option<&str>,
    ) -> Result<RemoteModel, String> {
        let adapter = resolve_adapter(provider, paths).map_err(|e| e.to_string())?;
        // The auth shape is decided by the adapter, so ask it with a throwaway request
        // rather than duplicating the rules here.
        let probe = adapter
            .build_request(&CanonicalRequest::new(
                model_override.unwrap_or("probe"),
                "probe",
            ))
            .map_err(|e| e.to_string())?;
        let key = resolve_credential(provider, &probe.auth, cfg).map_err(|e| e.to_string())?;
        Ok(RemoteModel {
            adapter,
            transport: LiveTransport::new(Duration::from_secs(cfg.providers.timeout_secs)),
            provider: provider.to_string(),
            key,
            auth: probe.auth,
            base_url: cfg.providers.base_url.clone(),
            model_override: model_override.map(|m| m.to_string()),
        })
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    fn http(&self, request: &CanonicalRequest) -> Result<HttpRequest, ModelError> {
        let mut request = request.clone();
        if let Some(model) = &self.model_override {
            request.model = model.clone();
        }
        // The agent loop always wants a whole response; streaming would give the loop
        // deltas it cannot act on.
        request.stream = Some(false);
        let wire = self.adapter.build_request(&request).map_err(|e| {
            ModelError::Decode(format!(
                "{} could not build the request: {e}",
                self.adapter.id()
            ))
        })?;
        let mut http = to_http_request(
            &wire.url,
            &wire.headers,
            &wire.body,
            &self.auth,
            self.key.as_deref(),
        )
        .map_err(|e| ModelError::Transport(e.to_string()))?;
        if let Some(base) = &self.base_url {
            http.url = reroute(base, &http.url).map_err(ModelError::Transport)?;
        }
        Ok(http)
    }
}

impl Model for RemoteModel {
    fn complete(&self, request: &CanonicalRequest) -> Result<CanonicalResponse, ModelError> {
        let http = self.http(request)?;
        // The agent's model handle has no config of its own; the standard policy is the same
        // one `run` uses by default, so a mission and a single turn retry alike.
        let (response, body) = crate::retry::buffered(
            &self.transport,
            &http,
            &crate::retry::RetryPolicy::standard(),
            &mut |_| {},
            &mut crate::retry::real_sleep,
        )
        .map_err(|e| ModelError::Transport(format!("{e} ({})", http.url.display_redacted())))?;

        if !response.is_success() {
            let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let error = self.adapter.parse_error(response.status, &value);
            return Err(ModelError::Refused {
                status: response.status,
                detail: error.to_string(),
            });
        }

        // A server is allowed to ignore `stream: false`. Reassemble what we can: text is
        // recoverable from the stream; tool calls are not, and the error says so rather
        // than silently returning an answer with no actions.
        if response.is_event_stream() {
            let mut decoder = self.adapter.stream_decoder(request);
            let mut sse = SseDecoder::new();
            let mut text = String::new();
            let mut usage = zenithal_providers::Usage::default();
            for frame in sse.push(&body) {
                for event in decoder
                    .push_frame(&frame)
                    .map_err(|e| ModelError::Decode(e.to_string()))?
                {
                    match event {
                        zenithal_providers::CanonicalEvent::TextDelta { text: t } => {
                            text.push_str(&t)
                        }
                        zenithal_providers::CanonicalEvent::Usage { usage: u } => {
                            usage.merge_max(&u)
                        }
                        _ => {}
                    }
                }
            }
            for event in decoder
                .finish()
                .map_err(|e| ModelError::Decode(e.to_string()))?
            {
                if let zenithal_providers::CanonicalEvent::Usage { usage: u } = event {
                    usage.merge_max(&u);
                }
            }
            if text.trim().is_empty() {
                return Err(ModelError::Decode(
                    "the server streamed a reply to a non-streaming request and it carried no \
                     text; tool calls arrived as deltas and cannot be replayed, so the agent \
                     stopped instead of acting on a partial answer"
                        .into(),
                ));
            }
            return Ok(CanonicalResponse {
                model: request.model.clone(),
                content: vec![zenithal_providers::Content::text(text)],
                stop_reason: zenithal_providers::StopReason::EndTurn,
                usage,
            });
        }

        let value: Value = serde_json::from_str(&body)
            .map_err(|e| ModelError::Decode(format!("{e}: {}", truncate(&body, 200))))?;
        self.adapter
            .parse_response(request, &value)
            .map_err(|e| ModelError::Decode(e.to_string()))
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}

/// Build one model per member from a shared config. Exposed so `squad` and `agent` commands
/// resolve providers identically.
pub fn for_provider(
    provider: &str,
    paths: &Paths,
    cfg: &Config,
    model: Option<&str>,
) -> Result<RemoteModel, String> {
    RemoteModel::new(provider, paths, cfg, model)
}

/// True when the provider id is one this build ships an adapter for.
pub fn is_known_provider(id: &str) -> bool {
    adapter_for(id).is_ok()
}
