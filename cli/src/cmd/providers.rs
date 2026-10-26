//! `stoic-codex providers …` — inspect and install provider definitions.
//!
//! The provider library is deliberately transport-free: an adapter's job is translation.
//! `providers show` prints the request shape without a network call; `providers models` uses
//! a read-only GET to the adapter's advertised catalogue endpoint; `providers test` sends a
//! completion. Credentials are attached only in the CLI transport and never printed.

use std::io::Write;
use std::time::{Duration, Instant};

use stoic_codex_providers::custom::{load_dir, CustomProvider};
use stoic_codex_providers::{adapter_for, CanonicalRequest, WireAdapter};

use crate::config::Config;
use crate::net::LiveTransport;

use crate::args::Parsed;
use crate::cmd::{CmdError, CmdResult};
use crate::paths::Paths;
use crate::Ctx;

pub fn run(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    match parsed.rest().first().map(String::as_str) {
        Some("list") => list(paths, ctx),
        Some("ids") => ids(ctx),
        Some("models") => models(parsed, paths, ctx),
        Some("add") => add(parsed, paths, ctx),
        Some("show") => show(parsed, paths, ctx),
        Some("test") => test(parsed, paths, ctx),
        Some(other) => Err(CmdError::usage(format!(
            "unknown `providers` subcommand `{other}` (list, ids, models, add, show, test)"
        ))),
        None => Err(CmdError::usage(
            "usage: stoic-codex providers <list|ids|models|add|show|test>",
        )),
    }
}

fn ids(ctx: &mut Ctx) -> CmdResult {
    for id in stoic_codex_providers::adapters::provider_ids() {
        let _ = writeln!(ctx.out, "{id}");
    }
    Ok(())
}

/// Ask a provider's read-only model-catalogue endpoint, then print only what it actually
/// reported. This is a GET, not a completion: it never sends a prompt or generates tokens.
fn models(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let Some(id) = parsed.rest().get(1) else {
        return Err(CmdError::usage(
            "usage: stoic-codex providers models <id> [--free] [--max-attempts N]",
        ));
    };
    let cfg = Config::load_for(paths).map_err(CmdError::failed)?;
    cfg.validate().map_err(CmdError::failed)?;

    let provider = id.as_str();
    let adapter = crate::cmd::run::resolve_adapter(provider, paths)?;
    let wire = adapter.list_models_request().ok_or_else(|| {
        CmdError::usage(format!(
            "provider `{provider}` has no known model-list endpoint; use `providers list` to see models declared in a custom spec"
        ))
    })?;
    let key = crate::cmd::run::resolve_credential(provider, &wire.auth, &cfg)?;
    let mut http = crate::cmd::run::to_http_request(
        &wire.url,
        &wire.headers,
        &wire.body,
        &wire.auth,
        key.as_deref(),
    )?;
    // `to_http_request` is shared with completion requests, whose verb is always POST.
    // Catalogues are GETs, and sending a body is unnecessary and not accepted everywhere.
    http.method = wire.method;
    http.body.clear();
    if let Some(base) = &cfg.providers.base_url {
        http.url = crate::cmd::run::reroute(base, &http.url).map_err(CmdError::failed)?;
    }

    let policy = crate::cmd::run::retry_policy_for(parsed, &cfg)?;
    let transport = LiveTransport::new(Duration::from_secs(cfg.providers.timeout_secs.min(60)));
    let (response, body) = crate::retry::buffered(
        &transport,
        &http,
        &policy,
        &mut |line| {
            let _ = writeln!(ctx.err, "\u{1b}[2m{line}\u{1b}[0m");
        },
        &mut crate::retry::real_sleep,
    )
    .map_err(|e| CmdError::failed(format!("{provider} model list failed: {e}")))?;

    if !response.is_success() {
        let value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
        let error = adapter.parse_error(response.status, &value);
        return Err(CmdError::failed(format!(
            "{provider} answered HTTP {}: {error}",
            response.status
        )));
    }
    let value: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        CmdError::failed(format!(
            "{provider} returned a non-JSON model catalogue ({e})"
        ))
    })?;
    let mut models = adapter
        .parse_models(&value)
        .map_err(|e| CmdError::failed(format!("cannot read {provider} model catalogue: {e}")))?;

    if parsed.switch("free") {
        models.retain(stoic_codex_providers::models::ModelInfo::is_free);
    }

    let _ = writeln!(
        ctx.out,
        "{} model catalogue · {} model{}",
        provider,
        models.len(),
        if models.len() == 1 { "" } else { "s" }
    );
    let _ = writeln!(
        ctx.out,
        "{:<48} {:>10} {:<24} {}",
        "MODEL ID", "CONTEXT", "INPUT / OUTPUT PRICE", "STATUS"
    );
    for model in &models {
        let context = model
            .context
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".to_string());
        let price = match (&model.input_price, &model.output_price) {
            (Some(input), Some(output)) => format!("{input} / {output}"),
            (Some(input), None) => format!("{input} / —"),
            (None, Some(output)) => format!("— / {output}"),
            (None, None) => "not reported".to_string(),
        };
        let status = if model.is_free() {
            "FREE (reported)"
        } else {
            "—"
        };
        let _ = writeln!(
            ctx.out,
            "{:<48} {:>10} {:<24} {}",
            truncate(&model.id, 48),
            context,
            truncate(&price, 24),
            status
        );
    }
    let _ = writeln!(
        ctx.out,
        "\nPrice values are copied from the provider; OpenRouter reports USD per token."
    );
    let _ = writeln!(ctx.out, "FREE is shown only for a `:free` model ID or explicit zero input and output prices. Unreported pricing/free-tier eligibility is unknown, not a guarantee.");
    if parsed.switch("free") && models.is_empty() {
        let _ = writeln!(ctx.out, "No models were explicitly marked free by this catalogue. Google AI Studio and Groq may have free-tier quotas that their model lists do not report.");
    }
    Ok(())
}

/// Built-in plus user-defined, as a table. User specs are loaded through the same
/// validation as `add`, so `list` doubles as a config check.
fn list(paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let _ = writeln!(
        ctx.out,
        "{:<24} {:<9} {:<44} {}",
        "ID", "STYLE", "BASE URL / SOURCE", "MODELS"
    );
    let mut built_in = 0usize;
    for id in stoic_codex_providers::adapters::provider_ids() {
        // Built-ins expose no metadata API yet; show what we can honestly show.
        let _ = writeln!(ctx.out, "{:<24} {:<9} {:<44} {}", id, "built-in", "-", "-");
        built_in += 1;
    }
    let dir = paths.providers_dir();
    let custom = load_dir(&dir).map_err(|e| CmdError::failed(e.to_string()))?;
    for p in &custom {
        let spec = p.spec();
        let models = if spec.models.is_empty() {
            "-".to_string()
        } else {
            spec.models.join(",")
        };
        let style = match spec.api_style {
            stoic_codex_providers::custom::ApiStyle::Openai => "openai",
            stoic_codex_providers::custom::ApiStyle::Custom => "custom",
        };
        let _ = writeln!(
            ctx.out,
            "{:<24} {:<9} {:<44} {}{}",
            spec.id,
            style,
            spec.endpoint(),
            models,
            if spec.local { "  (local)" } else { "" }
        );
    }
    let _ = writeln!(
        ctx.out,
        "\n{} built-in, {} user-defined (from {})",
        built_in,
        custom.len(),
        dir.display()
    );
    Ok(())
}

/// Validate a spec and copy it into the providers directory, named after its id.
fn add(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let Some(source) = parsed.rest().get(1) else {
        return Err(CmdError::usage(
            "usage: stoic-codex providers add <file.json> [--force]",
        ));
    };
    let text = std::fs::read_to_string(source)
        .map_err(|e| CmdError::failed(format!("cannot read {source}: {e}")))?;
    let provider = CustomProvider::from_json(&text).map_err(|e| CmdError::failed(e.to_string()))?;
    let dir = paths.providers_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| CmdError::failed(format!("cannot create {}: {e}", dir.display())))?;
    let target = dir.join(format!("{}.json", provider.id()));
    if target.exists() && !parsed.switch("force") {
        return Err(CmdError::failed(format!(
            "{} already exists; pass --force to replace it",
            target.display()
        )));
    }
    std::fs::write(&target, text)
        .map_err(|e| CmdError::failed(format!("cannot write {}: {e}", target.display())))?;
    let _ = writeln!(
        ctx.out,
        "validated provider `{}` → {}",
        provider.id(),
        target.display()
    );
    let missing: Vec<&str> = provider
        .spec()
        .api_key_env
        .as_deref()
        .filter(|env| std::env::var_os(env).is_none())
        .into_iter()
        .collect();
    if let Some(env) = missing.first() {
        // Not an error: a local endpoint needs no key, and CI may not have one set. But
        // saying so now beats a confusing 401 later.
        let _ = writeln!(ctx.out, "note: {env} is not set in this environment");
    }
    Ok(())
}

/// Print the exact HTTP request this provider would send for a sample prompt.
fn show(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let Some(id) = parsed.rest().get(1) else {
        return Err(CmdError::usage(
            "usage: stoic-codex providers show <id> [--prompt TEXT] [--model M]",
        ));
    };
    let prompt = parsed.value("prompt").unwrap_or("hello");
    let model = parsed.value("model").unwrap_or("sample-model");

    // A user-defined provider wins over a built-in with the same id: the user's config
    // is the more specific statement of intent.
    let adapter: Box<dyn WireAdapter> = match load_dir(&paths.providers_dir())
        .map_err(|e| CmdError::failed(e.to_string()))?
        .into_iter()
        .find(|p| p.id() == id.as_str())
    {
        Some(custom) => Box::new(custom),
        None => adapter_for(id).map_err(|e| CmdError::usage(e.to_string()))?,
    };

    let req = CanonicalRequest::new(model, prompt);
    let wire = adapter
        .build_request(&req)
        .map_err(|e| CmdError::failed(e.to_string()))?;

    let _ = writeln!(ctx.out, "{} {}", wire.method, wire.url);
    for (k, v) in &wire.headers {
        let _ = writeln!(ctx.out, "{k}: {v}");
    }
    // Never print a credential; describe how it would be attached instead.
    match &wire.auth {
        stoic_codex_providers::AuthSpec::None => {
            let _ = writeln!(ctx.out, "(no credential)");
        }
        stoic_codex_providers::AuthSpec::Bearer => {
            let _ = writeln!(ctx.out, "authorization: <bearer credential>");
        }
        auth => {
            let name = auth
                .header_name()
                .or_else(|| auth.query_name())
                .unwrap_or("?");
            let _ = writeln!(ctx.out, "{name}: {}", auth.render("<credential>"));
        }
    }
    let body = serde_json::to_string_pretty(&wire.body).unwrap_or_else(|_| "{}".into());
    let _ = writeln!(ctx.out, "\n{body}");
    Ok(())
}

/// `providers test <id>` — one real request, so a hand-written spec can be checked without
/// running a whole turn. Prints what came back, and what it cost in time and tokens.
fn test(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let Some(id) = parsed.rest().get(1) else {
        return Err(CmdError::usage(
            "usage: stoic-codex providers test <id> [--model M] [--prompt TEXT] [--max-tokens N] [--max-attempts N]",
        ));
    };
    let cfg = Config::load_for(paths).map_err(CmdError::failed)?;
    cfg.validate().map_err(CmdError::failed)?;

    let provider = id.as_str();
    let adapter = crate::cmd::run::resolve_adapter(provider, paths)?;
    let model = parsed
        .value("model")
        .map(str::to_string)
        .or_else(|| cfg.providers.model.clone())
        .ok_or_else(|| {
            CmdError::usage("no model given: pass --model <name> or set providers.model")
        })?;
    let prompt = parsed
        .value("prompt")
        .unwrap_or("Reply with the single word: pong");
    let max_tokens = match parsed.value("max-tokens") {
        Some(v) => v
            .parse::<u32>()
            .map_err(|_| CmdError::usage("--max-tokens must be a number"))?,
        None => 32,
    };

    let mut request = CanonicalRequest::new(model.clone(), prompt);
    request.max_tokens = Some(max_tokens);
    // Not streaming: a ping should produce one small response, and a single JSON reply is
    // easier to report on than a partial stream.
    request.stream = Some(false);

    let wire = adapter.build_request(&request).map_err(|e| {
        CmdError::failed(format!("{} could not build the request: {e}", adapter.id()))
    })?;
    let key = crate::cmd::run::resolve_credential(provider, &wire.auth, &cfg)?;
    let mut http = crate::cmd::run::to_http_request(
        &wire.url,
        &wire.headers,
        &wire.body,
        &wire.auth,
        key.as_deref(),
    )?;
    if let Some(base) = &cfg.providers.base_url {
        http.url = crate::cmd::run::reroute(base, &http.url).map_err(CmdError::failed)?;
    }

    let policy = crate::cmd::run::retry_policy_for(parsed, &cfg)?;
    let transport = LiveTransport::new(Duration::from_secs(cfg.providers.timeout_secs.min(60)));
    let started = Instant::now();
    // A model listing is idempotent, so retrying it is always safe — and a free tier will
    // rate-limit it exactly like a completion.
    let (response, body) = crate::retry::buffered(
        &transport,
        &http,
        &policy,
        &mut |line| {
            let _ = writeln!(ctx.err, "\u{1b}[2m{line}\u{1b}[0m");
        },
        &mut crate::retry::real_sleep,
    )
    .map_err(|e| CmdError::failed(format!("{e} ({})", http.url.display_redacted())))?;
    let latency = started.elapsed();

    if !response.is_success() {
        let value: serde_json::Value =
            serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
        let e = adapter.parse_error(response.status, &value);
        return Err(CmdError::failed(format!(
            "{} answered {} in {} ms: {e}",
            provider,
            response.status,
            latency.as_millis()
        )));
    }

    // A server is free to ignore `stream: false`; if it answered with an event stream,
    // decode it rather than failing on "not JSON".
    if response.is_event_stream() {
        let mut decoder = adapter.stream_decoder(&request);
        let mut sse = stoic_codex_providers::SseDecoder::new();
        let mut text = String::new();
        let mut usage = stoic_codex_providers::Usage::default();
        let mut collect = |events: Vec<stoic_codex_providers::CanonicalEvent>| {
            for event in events {
                match event {
                    stoic_codex_providers::CanonicalEvent::TextDelta { text: delta } => {
                        text.push_str(&delta)
                    }
                    stoic_codex_providers::CanonicalEvent::Usage { usage: update } => {
                        usage.merge_max(&update)
                    }
                    _ => {}
                }
            }
        };
        for frame in sse.push(&body) {
            let events = decoder
                .push_frame(&frame)
                .map_err(|e| CmdError::failed(format!("cannot decode the stream: {e}")))?;
            collect(events);
        }
        if let Some(frame) = sse.finish() {
            let events = decoder
                .push_frame(&frame)
                .map_err(|e| CmdError::failed(format!("cannot decode the stream: {e}")))?;
            collect(events);
        }
        let events = decoder
            .finish()
            .map_err(|e| CmdError::failed(format!("cannot finish the stream: {e}")))?;
        collect(events);
        let _ = writeln!(ctx.out, "provider   {provider}");
        let _ = writeln!(ctx.out, "model      {model}");
        let _ = writeln!(
            ctx.out,
            "status     200 · {} ms · streamed",
            latency.as_millis()
        );
        let _ = writeln!(
            ctx.out,
            "tokens     {}",
            if usage.total() > 0 {
                format!("in {} out {}", usage.input_tokens, usage.output_tokens)
            } else {
                "not reported by this server".to_string()
            }
        );
        let _ = writeln!(ctx.out, "reply      {}", truncate(text.trim(), 200));
        return Ok(());
    }

    let value: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        // A 200 that is not JSON usually means the request reached the wrong endpoint: an
        // HTML page, a login screen, a proxy error.
        CmdError::failed(format!(
            "{} answered 200 but the body is not JSON ({e}); is the endpoint right? {}",
            provider,
            truncate(&body, 200)
        ))
    })?;
    let parsed_response = adapter
        .parse_response(&request, &value)
        .map_err(|e| CmdError::failed(format!("cannot decode the reply: {e}")))?;

    let _ = writeln!(ctx.out, "provider   {provider}");
    let _ = writeln!(ctx.out, "model      {}", parsed_response.model);
    let _ = writeln!(ctx.out, "status     200 · {} ms", latency.as_millis());
    if parsed_response.usage.total() > 0 {
        let _ = writeln!(
            ctx.out,
            "tokens     in {} out {}",
            parsed_response.usage.input_tokens, parsed_response.usage.output_tokens
        );
    } else {
        let _ = writeln!(ctx.out, "tokens     not reported by this server");
    }
    let text = parsed_response.text();
    let _ = writeln!(ctx.out, "reply      {}", truncate(text.trim(), 200));
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}
