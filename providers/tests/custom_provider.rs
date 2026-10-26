//! Tests for user-defined providers.
//!
//! These cover the two advertised escape hatches:
//!   * `api_style: openai` — same body as the built-in OpenAI adapter, different address.
//!   * `api_style: custom` — user-written request template + response paths.
//!
//! No network: a "provider" here is a JSON document plus recorded SSE text.

use stoic_codex_providers::custom::{self, CustomProvider};
use stoic_codex_providers::ir::{CanonicalEvent, Content, ToolSpec, Usage};
use stoic_codex_providers::sse::SseDecoder;
use stoic_codex_providers::{
    AuthSpec, CanonicalRequest, Message, StopReason, StreamAccumulator, WireAdapter,
};

/// A genuinely unusual API: a single `prompt` string in, a nested `output.text` out.
const FANCY: &str = r#"{
  "id": "lab-gateway",
  "label": "Lab gateway",
  "base_url": "http://127.0.0.1:8080",
  "path": "/infer",
  "auth": { "kind": "header", "name": "x-api-key", "prefix": "" },
  "api_key_env": "LAB_API_KEY",
  "api_style": "custom",
  "headers": { "x-route": "nim-fleet" },
  "request": {
    "engine": "nim",
    "input": "{{text}}",
    "system": "{{system}}",
    "max_new": "{{max_tokens}}",
    "streaming": "{{stream}}",
    "tools": "{{tools}}"
  },
  "response": {
    "text": "output.text",
    "model": "output.model",
    "stop": "output.finish",
    "usage": { "input": "output.tokens.in", "output": "output.tokens.out" }
  },
  "stream": {
    "text_delta": "delta.text",
    "done_marker": "[EOS]"
  },
  "stop_reasons": { "cutoff": "max_tokens" },
  "models": ["lab-1"],
  "local": true
}"#;

fn provider(spec: &str) -> CustomProvider {
    CustomProvider::from_json(spec).expect("spec must load")
}

fn req() -> CanonicalRequest {
    CanonicalRequest::new("lab-1", "is the token check correct?")
        .with_system("you are a careful reviewer")
        .with_max_tokens(4096)
}

// ─── spec validation ────────────────────────────────────────────────────────

#[test]
fn a_minimal_openai_compatible_provider_loads() {
    let p = provider(
        r#"{"id":"mybox","base_url":"http://127.0.0.1:11434","api_style":"openai","models":["llama"]}"#,
    );
    assert_eq!(p.id(), "mybox");
    assert_eq!(
        p.spec().endpoint(),
        "http://127.0.0.1:11434/v1/chat/completions"
    );
}

#[test]
fn missing_base_url_is_rejected_with_the_field_named() {
    let err = CustomProvider::from_json(r#"{"id":"x"}"#).unwrap_err();
    assert!(err.to_string().contains("base_url"), "{err}");
}

#[test]
fn a_non_http_scheme_is_rejected() {
    let err = CustomProvider::from_json(r#"{"id":"x","base_url":"ftp://box"}"#).unwrap_err();
    assert!(err.to_string().contains("http"), "{err}");
}

#[test]
fn a_trailing_slash_is_rejected_rather_than_tolerated() {
    // Silently normalising would hide `//v1/chat/completions` typos that 404 confusingly.
    let err =
        CustomProvider::from_json(r#"{"id":"x","base_url":"http://127.0.0.1:8080/"}"#).unwrap_err();
    assert!(err.to_string().contains("must not end with '/'"), "{err}");
}

#[test]
fn an_id_with_illegal_characters_is_rejected() {
    let err = CustomProvider::from_json(r#"{"id":"my lab!","base_url":"http://127.0.0.1:1"}"#)
        .unwrap_err();
    assert!(err.to_string().contains("a-zA-Z0-9_-"), "{err}");
}

#[test]
fn a_credential_shaped_header_is_rejected() {
    // Config files get committed. A key in `headers` is a key in git.
    for header in ["Authorization", "x-api-key", "api_key", "X-Key"] {
        let spec = format!(
            r#"{{"id":"x","base_url":"http://127.0.0.1:1","headers":{{"{header}":"secret"}}}}"#
        );
        let err = CustomProvider::from_json(&spec).unwrap_err();
        assert!(
            err.to_string().contains("api_key_env"),
            "header {header} must be refused with a pointer to api_key_env: {err}"
        );
    }
}

#[test]
fn custom_style_without_a_template_is_rejected() {
    let err = CustomProvider::from_json(
        r#"{"id":"x","base_url":"http://127.0.0.1:1","api_style":"custom"}"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("request"), "{err}");
}

#[test]
fn debug_output_never_contains_credentials() {
    let p = provider(
        r#"{"id":"x","base_url":"http://127.0.0.1:1","auth":{"kind":"bearer"},"api_key_env":"MY_SECRET_VAR"}"#,
    );
    std::env::set_var("MY_SECRET_VAR", "sk-should-never-appear");
    let dbg = format!("{p:?}");
    assert!(!dbg.contains("sk-should-never-appear"), "{dbg}");
    assert!(
        dbg.contains("x"),
        "the id should still be visible for debugging: {dbg}"
    );
    std::env::remove_var("MY_SECRET_VAR");
}

// ─── request construction ───────────────────────────────────────────────────

#[test]
fn openai_style_reuses_the_built_in_body_exactly() {
    // The whole point of `api_style: openai`: no second, subtly different implementation.
    let custom =
        provider(r#"{"id":"compat","base_url":"http://127.0.0.1:8080","api_style":"openai"}"#);
    let builtin = stoic_codex_providers::adapter_for("openai-chat").unwrap();
    let r = req();
    assert_eq!(
        custom.build_request(&r).unwrap().body,
        builtin.build_request(&r).unwrap().body,
        "a custom OpenAI-compatible provider must emit the same body as the built-in adapter"
    );
}

#[test]
fn openai_style_points_at_the_users_url_and_auth() {
    let p = provider(
        r#"{"id":"compat","base_url":"http://127.0.0.1:8080","path":"/v1/chat/completions","auth":{"kind":"header","name":"x-api-key"},"headers":{"x-route":"fleet"}}"#,
    );
    let w = p.build_request(&req()).unwrap();
    assert_eq!(w.method, "POST");
    assert_eq!(w.url, "http://127.0.0.1:8080/v1/chat/completions");
    assert_eq!(w.auth, AuthSpec::header("x-api-key"));
    assert_eq!(w.header("x-route"), Some("fleet"));
    assert_eq!(w.header("content-type"), Some("application/json"));
}

#[test]
fn auth_kinds_map_to_the_transport_contract() {
    let bearer = provider(r#"{"id":"a","base_url":"http://127.0.0.1:1","auth":{"kind":"bearer"}}"#);
    assert_eq!(bearer.build_request(&req()).unwrap().auth, AuthSpec::Bearer);
    assert_eq!(AuthSpec::Bearer.render("k"), "Bearer k");

    let headed = provider(
        r#"{"id":"b","base_url":"http://127.0.0.1:1","auth":{"kind":"header","name":"authorization","prefix":"Key "}}"#,
    );
    let auth = headed.build_request(&req()).unwrap().auth;
    // The transport, not the adapter, formats the credential.
    assert_eq!(auth.render("k"), "Key k");

    let queried = provider(
        r#"{"id":"c","base_url":"http://127.0.0.1:1","auth":{"kind":"query","name":"key"}}"#,
    );
    assert_eq!(
        queried.build_request(&req()).unwrap().auth.query_name(),
        Some("key")
    );

    let none = provider(r#"{"id":"d","base_url":"http://127.0.0.1:1","auth":{"kind":"none"}}"#);
    assert_eq!(none.build_request(&req()).unwrap().auth, AuthSpec::None);
}

#[test]
fn a_custom_template_renders_typed_values_not_strings() {
    let p = provider(FANCY);
    let body = p.build_request(&req()).unwrap().body;
    assert_eq!(body["engine"], "nim");
    assert_eq!(
        body["max_new"], 4096,
        "a whole-string placeholder must keep its JSON type"
    );
    assert_eq!(body["streaming"], true);
    assert_eq!(body["system"], "you are a careful reviewer");
    assert_eq!(
        body["input"],
        "you are a careful reviewer\n\nis the token check correct?\n"
    );
    assert!(
        body["tools"].is_array(),
        "tool definitions must splice in as an array"
    );
}

#[test]
fn tool_definitions_reach_a_custom_provider() {
    let p = provider(FANCY);
    let r = req().with_tools(vec![ToolSpec {
        name: "read_file".into(),
        description: Some("read a file".into()),
        input_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
    }]);
    let body = p.build_request(&r).unwrap().body;
    assert_eq!(body["tools"][0]["function"]["name"], "read_file");
    assert_eq!(
        body["tools"][0]["function"]["parameters"]["properties"]["path"]["type"],
        "string"
    );
}

#[test]
fn an_unknown_placeholder_is_an_error_not_an_empty_string() {
    let p = provider(
        r#"{"id":"x","base_url":"http://127.0.0.1:1","api_style":"custom","request":{"q":"{{temprature}}"}}"#,
    );
    let err = p.build_request(&req()).unwrap_err();
    assert!(
        err.to_string().contains("unknown template placeholder"),
        "{err}"
    );
    assert!(
        err.to_string().contains("temprature"),
        "the typo must be named: {err}"
    );
}

#[test]
fn an_unclosed_placeholder_is_reported() {
    let p = provider(
        r#"{"id":"x","base_url":"http://127.0.0.1:1","api_style":"custom","request":{"q":"{{model"}}"#,
    );
    let err = p.build_request(&req()).unwrap_err();
    assert!(err.to_string().contains("unclosed"), "{err}");
}

#[test]
fn template_and_path_lookup_are_independently_testable() {
    let r = req();
    let v = custom::render(&serde_json::json!({"m":"{{model}}","t":"{{text}}"}), &r).unwrap();
    assert_eq!(v["m"], "lab-1");
    assert!(v["t"].as_str().unwrap().contains("token check"));

    let doc = serde_json::json!({"a":{"b":[{"c":"deep"}]}});
    assert_eq!(
        custom::resolve(&doc, "a.b.0.c").and_then(|v| v.as_str()),
        Some("deep")
    );
    assert!(custom::resolve(&doc, "a.b.9.c").is_none());
    assert!(custom::resolve(&doc, "a.x.c").is_none());
    assert!(custom::resolve(&doc, "").is_none());
    assert_eq!(custom::string_at(&doc, "a.b.0.c").as_deref(), Some("deep"));
}

// ─── responses ──────────────────────────────────────────────────────────────

#[test]
fn a_custom_response_is_mapped_through_the_spec_paths() {
    let p = provider(FANCY);
    let body = serde_json::json!({
        "output": {
            "text": "yes, but check the expiry comparison",
            "model": "lab-1-quant",
            "finish": "cutoff",
            "tokens": { "in": 1200, "out": 84 }
        }
    });
    let resp = p.parse_response(&req(), &body).unwrap();
    assert_eq!(resp.text(), "yes, but check the expiry comparison");
    assert_eq!(resp.model, "lab-1-quant");
    assert_eq!(
        resp.stop_reason,
        StopReason::MaxTokens,
        "stop_reasons must translate vendor wording"
    );
    assert_eq!(
        resp.usage,
        Usage {
            input_tokens: 1200,
            output_tokens: 84,
            ..Default::default()
        }
    );
}

#[test]
fn a_response_with_no_text_at_the_configured_path_says_which_path() {
    let p = provider(FANCY);
    let err = p
        .parse_response(&req(), &serde_json::json!({"output":{"other":"x"}}))
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("output.text"),
        "the configured path must appear in the error: {msg}"
    );
}

#[test]
fn tool_calls_are_parsed_from_the_configured_path() {
    let p = provider(
        r#"{
            "id": "tc", "base_url": "http://127.0.0.1:1", "api_style": "custom",
            "request": { "q": "{{text}}" },
            "response": { "text": "say", "tool_calls": "calls" }
        }"#,
    );
    let body = serde_json::json!({
        "say": "let me look",
        "calls": [
            { "id": "c1", "function": { "name": "read_file", "arguments": "{\"path\":\"src/lib.rs\"}" } },
            { "function": { "name": "run_tests" } }
        ]
    });
    let resp = p.parse_response(&req(), &body).unwrap();
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    let uses = resp.tool_uses();
    assert_eq!(uses.len(), 2);
    assert_eq!(uses[0].1, "read_file");
    assert_eq!(uses[0].2["path"], "src/lib.rs");
    assert_eq!(
        uses[1].0,
        "c1".replace('c', "call_"),
        "a missing id must be synthesised"
    );
    assert_eq!(
        *uses[1].2,
        serde_json::json!({}),
        "a missing arguments blob becomes an empty object"
    );
}

#[test]
fn remote_errors_are_typed_with_the_vendor_message() {
    let p = provider(FANCY);
    let err = p.parse_error(
        429,
        &serde_json::json!({"error":{"type":"rate_limit","message":"slow down"}}),
    );
    let msg = err.to_string();
    assert!(msg.contains("429"), "{msg}");
    assert!(msg.contains("rate_limit"), "{msg}");
    assert!(msg.contains("slow down"), "{msg}");
    assert!(err.is_retryable(), "429 must be retryable");
}

// ─── streaming ──────────────────────────────────────────────────────────────

fn decode(p: &CustomProvider, chunks: &[&str]) -> Vec<CanonicalEvent> {
    let mut sse = SseDecoder::new();
    let mut dec = p.stream_decoder(&req());
    let mut out = Vec::new();
    for chunk in chunks {
        for f in sse.push(chunk) {
            out.extend(dec.push_frame(&f).unwrap());
        }
    }
    out.extend(dec.finish().unwrap());
    out
}

#[test]
fn a_custom_stream_is_decoded_through_the_configured_paths() {
    let p = provider(FANCY);
    let events = decode(
        &p,
        &[
            "data: {\"delta\":{\"text\":\"yes\"}}\n\n",
            "data: {\"delta\":{\"text\":\", but\"}}\n\n",
            "data: {\"delta\":{\"text\":\" check expiry\"}}\n\n",
            "data: [EOS]\n\n",
        ],
    );
    let mut acc = StreamAccumulator::default();
    for e in events.clone() {
        acc.apply(e);
    }
    let resp = acc.into_response("lab-1");
    assert_eq!(resp.text(), "yes, but check expiry");
    assert!(matches!(
        events.first(),
        Some(CanonicalEvent::MessageStart { .. })
    ));
    assert!(matches!(events.last(), Some(CanonicalEvent::Done)));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, CanonicalEvent::Done))
            .count(),
        1,
        "Done must be emitted exactly once"
    );
}

#[test]
fn a_stream_error_becomes_an_error_event_and_ends_the_stream() {
    let p = provider(FANCY);
    let events = decode(
        &p,
        &[
            "data: {\"delta\":{\"text\":\"par\"}}\n\n",
            "data: {\"error\":{\"type\":\"overloaded\",\"message\":\"try later\"}}\n\n",
            "data: {\"delta\":{\"text\":\"should be ignored\"}}\n\n",
        ],
    );
    let errors: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            CanonicalEvent::Error { kind, message } => Some((kind.clone(), message.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        errors,
        vec![("overloaded".to_string(), "try later".to_string())]
    );
    assert!(matches!(events.last(), Some(CanonicalEvent::Done)));
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            CanonicalEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "par", "nothing after an error may be appended");
}

#[test]
fn a_stream_that_never_sends_a_marker_still_terminates_cleanly() {
    let p = provider(FANCY);
    let events = decode(&p, &["data: {\"delta\":{\"text\":\"partial\"}}\n\n"]);
    assert!(matches!(events.last(), Some(CanonicalEvent::Done)));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CanonicalEvent::Stop { .. })),
        "finish() must synthesise a stop reason: {events:?}"
    );
}

#[test]
fn streamed_tool_calls_are_assembled_across_frames() {
    let p = provider(
        r#"{
            "id": "tc", "base_url": "http://127.0.0.1:1", "api_style": "custom",
            "request": { "q": "{{text}}" },
            "stream": {
                "text_delta": "d.text",
                "tool_call": { "index": "d.tool.i", "id": "d.tool.id", "name": "d.tool.name", "arguments": "d.tool.args" },
                "done_marker": "[EOS]"
            }
        }"#,
    );
    let events = decode(
        &p,
        &[
            "data: {\"d\":{\"tool\":{\"i\":0,\"id\":\"c9\",\"name\":\"read_file\"}}}\n\n",
            "data: {\"d\":{\"tool\":{\"i\":0,\"args\":\"{\\\"path\\\":\"}}}\n\n",
            "data: {\"d\":{\"tool\":{\"i\":0,\"args\":\"\\\"a.rs\\\"}\"}}}\n\n",
            "data: [EOS]\n\n",
        ],
    );
    let mut acc = StreamAccumulator::default();
    for e in events {
        acc.apply(e);
    }
    let resp = acc.into_response("lab-1");
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    let uses = resp.tool_uses();
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].0, "c9");
    assert_eq!(uses[0].1, "read_file");
    assert_eq!(
        uses[0].2["path"], "a.rs",
        "fragments must be concatenated before parsing"
    );
}

#[test]
fn usage_reported_mid_stream_is_captured() {
    let p = provider(
        r#"{
            "id": "u", "base_url": "http://127.0.0.1:1", "api_style": "custom",
            "request": { "q": "{{text}}" },
            "stream": { "text_delta": "d.text", "usage": { "input": "u.in", "output": "u.out" } }
        }"#,
    );
    let events = decode(
        &p,
        &[
            "data: {\"d\":{\"text\":\"hi\"},\"u\":{\"in\":42,\"out\":7}}\n\n",
            "data: [DONE]\n\n",
        ],
    );
    let mut acc = StreamAccumulator::default();
    for e in events {
        acc.apply(e);
    }
    assert_eq!(acc.usage.input_tokens, 42);
    assert_eq!(acc.usage.output_tokens, 7);
}

#[test]
fn the_default_spec_matches_the_openai_stream_shape() {
    // A spec with no `stream` block must still decode OpenAI-style frames, because that
    // is what most "just point me at my endpoint" users will be hitting.
    let p = provider(r#"{"id":"o","base_url":"http://127.0.0.1:1"}"#);
    let events = decode(
        &p,
        &[
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
            "data: [DONE]\n\n",
        ],
    );
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            CanonicalEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "hello");
    assert!(matches!(events.last(), Some(CanonicalEvent::Done)));
}

#[test]
fn a_spec_round_trips_through_json() {
    // The config file is the interface; it must be writable as well as readable.
    let p = provider(FANCY);
    let text = p.spec().to_json();
    let again = CustomProvider::from_json(&text).expect("round-trip must load");
    assert_eq!(
        again.build_request(&req()).unwrap().url,
        p.build_request(&req()).unwrap().url
    );
}

#[test]
fn messages_keep_their_roles_and_images() {
    let p = provider(FANCY);
    let mut r = req();
    r.messages.push(Message {
        role: stoic_codex_providers::Role::User,
        content: vec![Content::Image {
            media_type: "image/png".into(),
            data: "AAAA".into(),
        }],
    });
    let body = p.build_request(&r).unwrap().body;
    let text = body["input"].as_str().unwrap();
    // Flattened for a single-string API: the image itself cannot survive, but the turn
    // must not be dropped silently either.
    assert!(text.contains("is the token check correct"), "{text}");
}

#[test]
fn the_shipped_example_specs_are_valid() {
    // A config example that does not load is a documentation bug that costs a user an
    // hour, so the examples are part of the test suite.
    for name in [
        "examples/custom_provider.json",
        "examples/custom_provider_weird.json",
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let spec = CustomProvider::from_json(&text)
            .unwrap_or_else(|e| panic!("{} is not a valid provider spec: {e}", path.display()));
        assert!(!spec.id().is_empty());
        // And it must be able to build a request, not merely parse.
        let body = spec
            .build_request(&req())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(body.url.starts_with("http"));
    }
}

// ─── api_style defaults ─────────────────────────────────────────────────────────

/// A spec that says `api_style: openai` should get OpenAI's usage and stop paths for free.
/// Before this, a perfectly good turn reported `0 in / 0 out tokens` because the payload
/// was there and the mapping was empty.
#[test]
fn an_openai_style_spec_inherits_usage_and_stop_paths() {
    let provider = CustomProvider::from_json(
        r#"{"id":"lab","base_url":"http://127.0.0.1:9","api_style":"openai"}"#,
    )
    .unwrap();
    let spec = provider.spec();
    assert_eq!(
        spec.stream.usage.input.as_deref(),
        Some("usage.prompt_tokens")
    );
    assert_eq!(
        spec.stream.usage.output.as_deref(),
        Some("usage.completion_tokens")
    );
    assert_eq!(spec.stream.stop.as_deref(), Some("choices.0.finish_reason"));
    assert_eq!(
        spec.response.usage.input.as_deref(),
        Some("usage.prompt_tokens")
    );
}

/// ...but an explicit path is the user's decision and must win.
#[test]
fn an_explicit_usage_path_is_not_overwritten_by_the_style_default() {
    let provider = CustomProvider::from_json(
        r#"{"id":"lab","base_url":"http://127.0.0.1:9","api_style":"openai",
            "stream":{"usage":{"input":"meta.tokens_in"}}}"#,
    )
    .unwrap();
    let spec = provider.spec();
    assert_eq!(spec.stream.usage.input.as_deref(), Some("meta.tokens_in"));
    assert_eq!(
        spec.stream.usage.output.as_deref(),
        Some("usage.completion_tokens")
    );
}

/// The mapping actually fires when the payload arrives.
#[test]
fn openai_style_usage_is_reported_from_a_streamed_reply() {
    use stoic_codex_providers::{CanonicalEvent, CanonicalRequest, SseDecoder, WireAdapter};
    let provider = CustomProvider::from_json(
        r#"{"id":"lab","base_url":"http://127.0.0.1:9","api_style":"openai"}"#,
    )
    .unwrap();
    let req = CanonicalRequest::new("m", "hi");
    let mut decoder = provider.stream_decoder(&req);
    let mut sse = SseDecoder::new();
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
                data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":4}}\n\n\
                data: [DONE]\n\n";
    let mut usage = stoic_codex_providers::Usage::default();
    let mut stop = None;
    for frame in sse.push(body) {
        for event in decoder.push_frame(&frame).unwrap() {
            match event {
                CanonicalEvent::Usage { usage: u } => usage.merge_max(&u),
                CanonicalEvent::Stop { reason } => stop = Some(reason),
                _ => {}
            }
        }
    }
    assert_eq!(usage.input_tokens, 9);
    assert_eq!(usage.output_tokens, 4);
    assert_eq!(stop, Some(stoic_codex_providers::StopReason::EndTurn));
}

/// The most common spec mistake: the version in `base_url` *and* in `path`, which
/// produced `/v1/v1/chat/completions` and a 404 that says nothing useful.
#[test]
fn a_duplicated_version_path_is_refused() {
    let err = CustomProvider::from_json(
        r#"{"id":"lab","base_url":"http://127.0.0.1:8099/v1","api_style":"openai"}"#,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("/v1/v1"), "{err}");
    assert!(err.contains("base_url"), "{err}");

    // The same spec without the version in the base is fine.
    assert!(CustomProvider::from_json(
        r#"{"id":"lab","base_url":"http://127.0.0.1:8099","api_style":"openai"}"#
    )
    .is_ok());
}
