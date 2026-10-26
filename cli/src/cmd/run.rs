//! `zenithal run "<task>"` — one turn, end to end.
//!
//! This is the command where the pieces stop being separable libraries and become a
//! product: recall from memory, compose a prompt, translate it through a provider adapter,
//! put it on a socket, decode the stream, print it, and write the exchange back.
//!
//! Design rules that shape the code below:
//!
//! * **The adapter is the only thing that knows the vendor.** This file builds a
//!   [`CanonicalRequest`] and never looks at a vendor JSON field — the same code drives
//!   Anthropic, OpenAI, Gemini, a local `llama.cpp` server, or a provider the user wrote
//!   themselves in JSON.
//! * **The credential is resolved here and nowhere else.** Adapters emit an [`AuthSpec`];
//!   this module turns it into a header value at the last possible moment, so a key cannot
//!   reach a log line, a memory node, or an error message.
//! * **Streaming is the default.** Local models are fast; users should see the first token
//!   rather than a spinner.
//! * **A failed turn changes nothing.** Memory is written only after a successful
//!   response, so a 429 cannot poison recall.

use std::io::Write;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use zenithal_memory::{EdgeKind, MemoryEdge, MemoryNode, NodeKind, OpenedMemory, Query};
use zenithal_providers::custom::load_dir;
use zenithal_providers::{
    adapter_for, AuthSpec, CanonicalEvent, CanonicalRequest, CanonicalResponse, Registry,
    SseDecoder, StopReason, Usage, WireAdapter,
};

use crate::args::Parsed;
use crate::cmd::{CmdError, CmdResult};
use crate::config::Config;
use crate::net::{HttpRequest, LiveTransport, Transport, Url};
use crate::paths::Paths;
use crate::retry::{self, RetryPolicy};
use crate::session::{self, MemoryBlock};
use crate::Ctx;

/// What a successful turn produced, kept separate from printing so it can be asserted on.
pub struct Outcome {
    pub text: String,
    pub usage: Usage,
    pub stop: StopReason,
    pub streamed: bool,
    pub latency: Duration,
    pub model: String,
    pub provider: String,
}

pub fn run(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let cfg = Config::load_for(paths).map_err(CmdError::failed)?;
    cfg.validate().map_err(CmdError::failed)?;
    let transport = LiveTransport::new(Duration::from_secs(cfg.providers.timeout_secs));
    turn(parsed, paths, &cfg, &transport, ctx)
}

/// The turn itself, with the transport injected so tests can run it against a stub.
pub fn turn(
    parsed: &Parsed,
    paths: &Paths,
    cfg: &Config,
    transport: &dyn Transport,
    ctx: &mut Ctx,
) -> CmdResult {
    let task = parsed.rest().join(" ").trim().to_string();
    if task.is_empty() {
        return Err(CmdError::usage(
            "usage: zenithal run <task…> [--provider ID] [--model NAME] [--no-stream] [--dry-run]",
        ));
    }

    let no_memory = parsed.switch("no-memory");
    let no_remember = parsed.switch("no-remember");
    let json = parsed.switch("json");
    let dry_run = parsed.switch("dry-run");

    // ── provider + model ────────────────────────────────────────────────────────────
    let provider_id = parsed
        .value("provider")
        .map(str::to_string)
        .or_else(|| std::env::var("ZENITHAL_PROVIDER").ok())
        .or_else(|| cfg.providers.default.clone())
        .ok_or_else(|| {
            CmdError::usage(format!(
                "no provider selected. Pass --provider <id>, or set providers.default in {}.\nbuilt-in ids: {}",
                paths.config_file().display(),
                zenithal_providers::adapters::provider_ids().join(", ")
            ))
        })?;

    let adapter = resolve_adapter(&provider_id, paths)?;
    let registry = Registry::seeded();
    let model = parsed
        .value("model")
        .map(str::to_string)
        .or_else(|| cfg.providers.model.clone())
        .ok_or_else(|| {
            CmdError::usage(
                "no model selected. Pass --model <name>, or set providers.model in config.json",
            )
        })?;
    // The registry is allowed to be wrong about names but it does know which models are
    // local, which changes how cost is reported.
    // By registry id *or* the name the vendor uses: `--model gemini-3.1-flash-lite` is what
    // a user types, and it must still find the price.
    let spec = registry.resolve(&model);
    let wire_model = spec
        .map(|s| s.wire().to_string())
        .unwrap_or_else(|| model.clone());

    // ── memory: recall ──────────────────────────────────────────────────────────────
    let mut opened: Option<OpenedMemory> = None;
    let block = if no_memory {
        MemoryBlock::empty()
    } else {
        let o = crate::cmd::open_store(paths)?;
        let block = match o.store.recall(&Query::new(task.clone())) {
            Ok(hits) if !hits.is_empty() => MemoryBlock {
                text: o.store.context(&hits, cfg.memory.context_budget),
                entries: hits.len(),
                characters: 0,
            },
            Ok(_) => MemoryBlock::empty(),
            Err(e) => return Err(CmdError::failed(format!("memory recall failed: {e}"))),
        };
        opened = Some(o);
        block
    };
    let block = MemoryBlock {
        characters: block.text.chars().count(),
        ..block
    };
    let composed = session::compose(&task, &block);

    // ── build the request ───────────────────────────────────────────────────────────
    let mut request = CanonicalRequest::new(wire_model.clone(), composed.user);
    request.system = Some(composed.system);
    request.max_tokens = parsed
        .value("max-tokens")
        .map(|v| {
            v.parse::<u32>()
                .map_err(|_| CmdError::usage("--max-tokens must be a number"))
        })
        .transpose()?
        .or(cfg.providers.max_tokens);
    request.temperature = parsed
        .value("temperature")
        .map(|v| {
            v.parse::<f32>()
                .map_err(|_| CmdError::usage("--temperature must be a number"))
        })
        .transpose()?
        .or(cfg.providers.temperature);
    let stream = if parsed.switch("no-stream") {
        false
    } else {
        cfg.providers.stream
    };
    request.stream = Some(stream);

    let wire = adapter.build_request(&request).map_err(|e| {
        CmdError::failed(format!("{} could not build the request: {e}", adapter.id()))
    })?;

    let key = resolve_credential(&provider_id, &wire.auth, cfg)?;
    let mut http = to_http_request(
        &wire.url,
        &wire.headers,
        &wire.body,
        &wire.auth,
        key.as_deref(),
    )?;
    if let Some(base) = &cfg.providers.base_url {
        http.url = reroute(base, &http.url).map_err(CmdError::failed)?;
    }

    if dry_run {
        print_request(&http, &wire.auth, ctx)?;
        return Ok(());
    }

    // ── send ────────────────────────────────────────────────────────────────────────
    let started = Instant::now();
    // Retries are a transport concern, so the adapter never learns they happened.
    let policy = retry_policy(cfg);
    let outcome = if stream {
        exchange_streaming(
            transport,
            &http,
            adapter.as_ref(),
            &request,
            json,
            &policy,
            ctx,
        )?
    } else {
        exchange_once(
            transport,
            &http,
            adapter.as_ref(),
            &request,
            json,
            &policy,
            ctx,
        )?
    };
    let latency = started.elapsed();

    // ── report ──────────────────────────────────────────────────────────────────────
    let outcome = Outcome {
        latency,
        model: model.clone(),
        provider: provider_id.clone(),
        ..outcome
    };
    report(&outcome, spec, json, ctx)?;

    // ── memory: remember ────────────────────────────────────────────────────────────
    if !no_remember {
        if let Some(mut opened) = opened.take() {
            write_back(&mut opened, &outcome, &task, &model, &provider_id);
        }
    }
    Ok(())
}

pub(crate) fn resolve_adapter(
    provider_id: &str,
    paths: &Paths,
) -> Result<Box<dyn WireAdapter>, CmdError> {
    // A user spec wins over a built-in with the same id: their file is the more specific
    // statement of intent.
    let custom = load_dir(&paths.providers_dir()).map_err(|e| CmdError::failed(e.to_string()))?;
    if let Some(p) = custom.into_iter().find(|p| p.id() == provider_id) {
        return Ok(Box::new(p));
    }
    adapter_for(provider_id).map_err(|e| {
        CmdError::usage(format!(
            "{e}\ninstall a spec with `zenithal providers add <file.json>` or pick one of: {}",
            zenithal_providers::adapters::provider_ids().join(", ")
        ))
    })
}

/// Find the credential for this provider. `Ok(None)` means "this endpoint wants none",
/// which is normal for a local server.
pub(crate) fn resolve_credential(
    provider_id: &str,
    auth: &AuthSpec,
    cfg: &Config,
) -> Result<Option<String>, CmdError> {
    if matches!(auth, AuthSpec::None) {
        return Ok(None);
    }
    let mut candidates: Vec<String> = Vec::new();
    if let Some(explicit) = &cfg.providers.api_key_env {
        candidates.push(explicit.clone());
    }
    candidates.push(format!(
        "{}_API_KEY",
        provider_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            })
            .collect::<String>()
    ));
    for name in &candidates {
        if let Some(value) = std::env::var_os(name) {
            let value = value.to_string_lossy().to_string();
            if !value.trim().is_empty() {
                return Ok(Some(value));
            }
        }
    }
    Err(CmdError::failed(format!(
        "{} needs a credential and none was found.\nset one of: {}\n(a local endpoint that needs no key can declare `\"auth\": {{\"kind\": \"none\"}}` in its provider spec)",
        provider_id,
        candidates.join(", ")
    )))
}

/// Turn the adapter's description into bytes on a socket. The credential is attached here
/// and never stored anywhere else.
pub(crate) fn to_http_request(
    url: &str,
    headers: &[(String, String)],
    body: &serde_json::Value,
    auth: &AuthSpec,
    key: Option<&str>,
) -> Result<HttpRequest, CmdError> {
    let mut url = Url::parse(url).map_err(|e| CmdError::failed(e.to_string()))?;
    let mut headers = headers.to_vec();
    if let Some(key) = key {
        match auth {
            AuthSpec::Bearer => {
                headers.push(("authorization".into(), format!("Bearer {key}")));
            }
            AuthSpec::Header { name, .. } => {
                headers.push((name.to_ascii_lowercase(), auth.render(key)));
            }
            AuthSpec::Query { name } => {
                let sep = if url.path.contains('?') { '&' } else { '?' };
                url.path = format!("{}{sep}{name}={}", url.path, urlencode(key));
            }
            AuthSpec::None => {}
        }
    }
    let body = serde_json::to_vec(body)
        .map_err(|e| CmdError::failed(format!("cannot serialize body: {e}")))?;
    Ok(HttpRequest {
        method: "POST".to_string(),
        url,
        headers,
        body,
    })
}

/// Point a request at a different origin, keeping the provider's path. This is what makes
/// a local gateway or a test server usable without editing a provider definition.
pub(crate) fn reroute(base: &str, url: &Url) -> Result<Url, String> {
    let mut base = Url::parse(base).map_err(|e| e.to_string())?;
    base.path = format!("{}{}", base.path.trim_end_matches('/'), url.path);
    Ok(base)
}

/// Minimal percent-encoding for a credential in a query string. Everything that is not an
/// unreserved character is escaped, which is over-inclusive but never wrong.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `--dry-run`: exactly what would be sent, with the credential replaced by a marker.
fn print_request(req: &HttpRequest, auth: &AuthSpec, ctx: &mut Ctx) -> CmdResult {
    let _ = writeln!(ctx.out, "{} {}", req.method, req.url.display_redacted());
    for (k, v) in &req.headers {
        // The only header that can carry a secret is the one the adapter named, and the
        // query-style auth never reaches here as a header. Redact defensively anyway.
        let shown = if is_secret_header(k, auth) {
            "<credential>".to_string()
        } else {
            v.clone()
        };
        let _ = writeln!(ctx.out, "{k}: {shown}");
    }
    let body: serde_json::Value =
        serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
    let _ = writeln!(
        ctx.out,
        "\n{}",
        serde_json::to_string_pretty(&body).unwrap_or_default()
    );
    Ok(())
}

fn is_secret_header(name: &str, auth: &AuthSpec) -> bool {
    match auth {
        AuthSpec::Bearer => name.eq_ignore_ascii_case("authorization"),
        AuthSpec::Header {
            name: auth_header, ..
        } => name.eq_ignore_ascii_case(auth_header),
        _ => false,
    }
}

/// Streaming path.
///
/// The body is tee'd: every chunk goes into the SSE decoder *and* into `raw`. Servers that
/// ignore `stream: true` and answer with one JSON document therefore still work — the SSE
/// decoder simply produces nothing, and the raw body is parsed as a completion. That one
/// tee is why this command works with a local `llama.cpp` server, a hosted vendor, and a
/// user-written provider spec without branching on the vendor.
fn exchange_streaming(
    transport: &dyn Transport,
    http: &HttpRequest,
    adapter: &dyn WireAdapter,
    request: &CanonicalRequest,
    json: bool,
    policy: &RetryPolicy,
    ctx: &mut Ctx,
) -> Result<Outcome, CmdError> {
    let mut decoder = adapter.stream_decoder(request);
    let mut sse = SseDecoder::new();
    let mut text = String::new();
    let mut raw = String::new();
    let mut usage = Usage::default();
    let mut stop = StopReason::EndTurn;
    let mut first_error: Option<String> = None;

    // The error document of a failed attempt is captured by the retry layer, not forwarded,
    // so it is returned alongside the response instead of arriving in `raw`.
    let (response, error_body) = {
        let out = &mut ctx.out;
        let err = &mut ctx.err;
        let mut sink = |chunk: &str| {
            raw.push_str(chunk);
            // A decoding failure is recorded and stops further processing, but the
            // transport keeps draining the socket so the connection closes cleanly.
            if first_error.is_some() {
                return;
            }
            for frame in sse.push(chunk) {
                match decoder.push_frame(&frame) {
                    Ok(events) => {
                        for event in events {
                            match event {
                                CanonicalEvent::TextDelta { text: t } => {
                                    text.push_str(&t);
                                    if !json {
                                        let _ = write!(out, "{t}");
                                        let _ = out.flush();
                                    }
                                }
                                CanonicalEvent::Usage { usage: u } => usage.merge_max(&u),
                                CanonicalEvent::Stop { reason } => stop = reason,
                                CanonicalEvent::Error { kind, message } => {
                                    first_error =
                                        Some(format!("provider reported {kind}: {message}"))
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => first_error = Some(e.to_string()),
                }
            }
        };
        // Every attempt after the first announces itself: a silent pause looks like a hang.
        retry::streamed(
            transport,
            http,
            &mut sink,
            policy,
            &mut |line| {
                let _ = writeln!(err, "{}", dim(line));
            },
            &mut retry::real_sleep,
        )
        .map_err(|e| CmdError::failed(format!("{e} ({})", http.url.display_redacted())))?
    };

    if !response.is_success() {
        let detail = if error_body.is_empty() {
            &raw
        } else {
            &error_body
        };
        return Err(error_for_status(adapter, response.status, detail));
    }
    if let Some(e) = first_error {
        return Err(CmdError::failed(e));
    }
    if !json && !text.is_empty() {
        let _ = writeln!(ctx.out);
    }

    // No SSE frames at all: the server answered with a whole JSON document.
    if text.is_empty() && !response.is_event_stream() {
        let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            CmdError::failed(format!(
                "expected a JSON completion or an event stream, got neither ({e}): {}",
                truncate(&raw, 300)
            ))
        })?;
        let parsed = adapter
            .parse_response(request, &value)
            .map_err(|e| CmdError::failed(e.to_string()))?;
        let body = parsed.text();
        if !json {
            let _ = writeln!(ctx.out, "{body}");
        }
        return Ok(Outcome {
            text: body,
            usage: parsed.usage,
            stop: parsed.stop_reason,
            streamed: false,
            latency: Duration::ZERO,
            model: request.model.clone(),
            provider: adapter.id().to_string(),
        });
    }

    for event in decoder
        .finish()
        .map_err(|e| CmdError::failed(e.to_string()))?
    {
        if let CanonicalEvent::Usage { usage: u } = event {
            usage.merge_max(&u);
        }
    }
    Ok(Outcome {
        text,
        usage,
        stop,
        streamed: true,
        latency: Duration::ZERO,
        model: request.model.clone(),
        provider: adapter.id().to_string(),
    })
}

fn exchange_once(
    transport: &dyn Transport,
    http: &HttpRequest,
    adapter: &dyn WireAdapter,
    request: &CanonicalRequest,
    json: bool,
    policy: &RetryPolicy,
    ctx: &mut Ctx,
) -> Result<Outcome, CmdError> {
    let (response, body) = retry::buffered(
        transport,
        http,
        policy,
        &mut |line| {
            let _ = writeln!(ctx.err, "{}", dim(line));
        },
        &mut retry::real_sleep,
    )
    .map_err(|e| CmdError::failed(format!("{} ({})", e, http.url.display_redacted())))?;
    if !response.is_success() {
        return Err(error_for_status(adapter, response.status, &body));
    }
    let value: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        CmdError::failed(format!(
            "expected a JSON completion, got something else ({e}): {}",
            truncate(&body, 300)
        ))
    })?;
    let parsed: CanonicalResponse = adapter
        .parse_response(request, &value)
        .map_err(|e| CmdError::failed(e.to_string()))?;
    let text = parsed.text();
    if !json {
        let _ = writeln!(ctx.out, "{text}");
    }
    Ok(Outcome {
        text,
        usage: parsed.usage,
        stop: parsed.stop_reason,
        streamed: false,
        latency: Duration::ZERO,
        model: request.model.clone(),
        provider: adapter.id().to_string(),
    })
}

/// Turn an error body into a message the user can act on. Providers are inconsistent here,
/// so the adapter's typed parser gets first refusal and the raw body is the fallback.
fn error_for_status(adapter: &dyn WireAdapter, status: u16, body: &str) -> CmdError {
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let detail = match parsed {
        Some(value) => {
            let e = adapter.parse_error(status, &value);
            e.to_string()
        }
        None => format!("HTTP {status}: {}", truncate(body, 300)),
    };
    CmdError::failed(detail)
}

fn cost_label(local: bool, cost: Option<f64>) -> String {
    if local {
        "local model, no cost".to_string()
    } else if let Some(usd) = cost {
        format!("rate-card estimate ${usd:.6} (not billing data)")
    } else {
        "cost unknown (not in the registry)".to_string()
    }
}

fn report(
    outcome: &Outcome,
    spec: Option<&zenithal_providers::ModelSpec>,
    json: bool,
    ctx: &mut Ctx,
) -> CmdResult {
    let cost =
        spec.map(|s| zenithal_providers::cost::Pricing::cost_usd(&s.pricing, &outcome.usage));
    let local = spec.map(|s| s.local).unwrap_or(false);

    if json {
        let payload = serde_json::json!({
            "provider": outcome.provider,
            "model": outcome.model,
            "text": outcome.text,
            "stop_reason": format!("{:?}", outcome.stop),
            "streamed": outcome.streamed,
            "latency_ms": outcome.latency.as_millis() as u64,
            "usage": {
                "input_tokens": outcome.usage.input_tokens,
                "output_tokens": outcome.usage.output_tokens,
                "cache_read_tokens": outcome.usage.cache_read_tokens,
                "cache_write_tokens": outcome.usage.cache_write_tokens,
            },
            "cost_usd": cost,
            "cost_basis": if local {
                "local"
            } else if cost.is_some() {
                "registry_rate_estimate"
            } else {
                "unknown"
            },
            "local": local,
        });
        let _ = writeln!(
            ctx.out,
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
        return Ok(());
    }

    // The answer is on stdout; the accounting goes to stderr so `zenithal run … > out.txt`
    // yields a clean file.
    let mut line = format!(
        "{} · {} · in {} out {} tok · {} ms",
        outcome.provider,
        outcome.model,
        outcome.usage.input_tokens,
        outcome.usage.output_tokens,
        outcome.latency.as_millis()
    );
    line.push_str(" · ");
    line.push_str(&cost_label(local, cost));
    let _ = writeln!(ctx.err, "{line}");
    Ok(())
}

/// Persist the exchange. Deliberately after the response: a failed turn leaves memory
/// untouched.
fn write_back(
    opened: &mut OpenedMemory,
    outcome: &Outcome,
    task: &str,
    model: &str,
    provider: &str,
) -> Option<()> {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let id = format!("turn:{secs}");
    // The label is the *question*: a future task resembles a question far more often than
    // it resembles the phrasing of an old answer, and the label is what a human scans in
    // `zenithal memory recall`.
    let label = truncate(task.trim(), 60).replace('\n', " ");
    // The body keeps both sides, because "what did I ask, and what came back" is the pair
    // that is worth retrieving.
    let node = MemoryNode::new(id.clone(), NodeKind::Task, label)
        .with_text(format!("Q: {}\n\nA: {}", task.trim(), outcome.text.trim()))
        .with_provenance(format!("{provider}/{model}"))
        .with_importance(0.4);
    opened.store.remember(node).ok()?;

    // Link the turn to the memories it was given, so the graph grows along the same edges
    // the ranking used.
    let recalled: Vec<String> = opened
        .store
        .recall(&Query::new(task.to_string()))
        .ok()?
        .into_iter()
        .map(|h| h.node.id)
        .filter(|other| *other != id)
        .take(5)
        .collect();
    for other in recalled {
        let _ = opened
            .store
            .link(MemoryEdge::new(id.clone(), other, EdgeKind::DerivedFrom));
    }
    Some(())
}

/// The retry policy for this workspace.
///
/// `providers.max_attempts` exists because a free tier and a paid tier want different
/// patience: 4 attempts rides out a burst, 1 is for a script that would rather fail fast.
pub fn retry_policy(cfg: &Config) -> RetryPolicy {
    let attempts = cfg.providers.max_attempts.unwrap_or(4).clamp(1, 10);
    RetryPolicy::standard().with_attempts(attempts)
}

/// Dim text for notices that are not the answer.
fn dim(s: &str) -> String {
    format!("\u{1b}[2m{s}\u{1b}[0m")
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_cost_is_labelled_as_a_rate_card_estimate() {
        let label = cost_label(false, Some(0.000058));
        assert!(label.contains("rate-card estimate"), "{label}");
        assert!(label.contains("not billing data"), "{label}");
        assert_eq!(
            cost_label(false, Some(0.0)),
            "rate-card estimate $0.000000 (not billing data)"
        );
    }

    #[test]
    fn a_bearer_credential_becomes_an_authorization_header() {
        let req = to_http_request(
            "https://api.example.com/v1/messages",
            &[("content-type".into(), "application/json".into())],
            &serde_json::json!({"a":1}),
            &AuthSpec::Bearer,
            Some("sk-secret"),
        )
        .unwrap();
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "authorization" && v == "Bearer sk-secret"));
        assert_eq!(req.url.host, "api.example.com");
    }

    #[test]
    fn a_header_credential_uses_its_prefix() {
        let req = to_http_request(
            "https://api.example.com/",
            &[],
            &serde_json::json!({}),
            &AuthSpec::header_prefixed("x-api-key", "Key "),
            Some("abc"),
        )
        .unwrap();
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == "Key abc"));
    }

    #[test]
    fn a_query_credential_is_encoded_into_the_url() {
        let req = to_http_request(
            "https://api.example.com/v1?beta=1",
            &[],
            &serde_json::json!({}),
            &AuthSpec::query("key"),
            Some("a b&c"),
        )
        .unwrap();
        assert_eq!(req.url.path, "/v1?beta=1&key=a%20b%26c");
        // ...and must not survive into the redacted form used in messages.
        assert!(!req.url.display_redacted().contains("key="));
    }

    #[test]
    fn a_provider_that_wants_no_key_needs_none() {
        let cfg = Config::default();
        assert_eq!(
            resolve_credential("local", &AuthSpec::None, &cfg).unwrap(),
            None
        );
    }

    #[test]
    fn a_missing_credential_names_the_variable_to_set() {
        let cfg = Config::default();
        let err = resolve_credential("definitely-not-a-provider", &AuthSpec::Bearer, &cfg)
            .unwrap_err()
            .to_string();
        assert!(err.contains("DEFINITELY_NOT_A_PROVIDER_API_KEY"), "{err}");
        assert!(
            err.contains("auth"),
            "the escape hatch must be mentioned: {err}"
        );
    }

    #[test]
    fn a_base_url_override_keeps_the_provider_path() {
        let original = Url::parse("https://api.openai.com/v1/chat/completions").unwrap();
        let moved = reroute("http://127.0.0.1:8080", &original).unwrap();
        assert_eq!(moved.authority(), "127.0.0.1:8080");
        assert_eq!(moved.path, "/v1/chat/completions");
        assert!(!moved.secure);

        // A prefix on the override is preserved rather than replaced.
        let moved = reroute("http://gateway.local/openai", &original).unwrap();
        assert_eq!(moved.path, "/openai/v1/chat/completions");
    }

    #[test]
    fn urls_encode_what_they_must() {
        assert_eq!(urlencode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(urlencode("a/b c"), "a%2Fb%20c");
    }

    #[test]
    fn truncation_is_char_based_not_byte_based() {
        assert_eq!(truncate("héllo", 3), "hél…");
    }
}
