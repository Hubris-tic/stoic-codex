//! Cassette conformance: every provider must produce the SAME canonical result
//! from its own recorded wire format.
//!
//! This is the test that makes BYOK real. Each cassette is a recorded SSE body from
//! a provider expressing one identical interaction: "say two sentences, then call
//! read_file(path=src/main.rs, line=42)". If a new adapter cannot satisfy this, it
//! is not done. No network, no keys, no flakes.

use stoic_codex_providers::{adapter_for, SseDecoder, StopReason, StreamAccumulator};

const EXPECTED_TEXT: &str = "I'll open that file.";
const EXPECTED_TOOL: &str = "read_file";
const EXPECTED_PATH: &str = "src/main.rs";
const EXPECTED_LINE: i64 = 42;
const EXPECTED_INPUT_TOKENS: u64 = 1024;
const EXPECTED_OUTPUT_TOKENS: u64 = 57;

fn replay(provider: &str, cassette: &str) -> StreamAccumulator {
    let adapter = adapter_for(provider).unwrap_or_else(|e| panic!("{provider}: {e}"));
    let req = stoic_codex_providers::CanonicalRequest::new("test-model", "open src/main.rs line 42");
    let mut decoder = adapter.stream_decoder(&req);
    let mut sse = SseDecoder::new();
    let mut acc = StreamAccumulator::default();

    let path = format!("{}/tests/cassettes/{cassette}", env!("CARGO_MANIFEST_DIR"));
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("missing cassette {path}: {e}"));

    // Feed in awkward slices to prove boundary handling, not just whole-file luck.
    for chunk in body.as_bytes().chunks(7) {
        let s = std::str::from_utf8(chunk).expect("cassettes are ASCII");
        for frame in sse.push(s) {
            for ev in decoder.push_frame(&frame).expect("decode frame") {
                acc.apply(ev);
            }
        }
    }
    if let Some(f) = sse.finish() {
        for ev in decoder.push_frame(&f).expect("decode trailing frame") {
            acc.apply(ev);
        }
    }
    for ev in decoder.finish().expect("decoder finish") {
        acc.apply(ev);
    }
    acc
}

fn assert_identical_interaction(provider: &str, cassette: &str, expect_tool: bool) {
    let acc = replay(provider, cassette);
    assert!(acc.done, "{provider}: stream never reached Done");
    assert!(
        acc.error.is_none(),
        "{provider}: stream error {:?}",
        acc.error
    );

    let text = acc.text.trim();
    assert_eq!(text, EXPECTED_TEXT, "{provider}: assistant text mismatch");

    assert_eq!(
        acc.usage.input_tokens, EXPECTED_INPUT_TOKENS,
        "{provider}: input token accounting mismatch (got {:?})",
        acc.usage
    );
    assert_eq!(
        acc.usage.output_tokens, EXPECTED_OUTPUT_TOKENS,
        "{provider}: output token accounting mismatch (got {:?})",
        acc.usage
    );

    if expect_tool {
        let resp = acc.into_response("fallback");
        assert_eq!(
            resp.stop_reason,
            StopReason::ToolUse,
            "{provider}: stop reason"
        );
        let uses: Vec<_> = resp
            .content
            .iter()
            .filter_map(|c| match c {
                stoic_codex_providers::Content::ToolUse { name, input, .. } => {
                    Some((name.clone(), input.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            uses.len(),
            1,
            "{provider}: expected exactly one tool call, got {uses:?}"
        );
        assert_eq!(uses[0].0, EXPECTED_TOOL, "{provider}: tool name");
        assert_eq!(
            uses[0].1.get("path").and_then(|v| v.as_str()),
            Some(EXPECTED_PATH),
            "{provider}: tool argument path was {:?}",
            uses[0].1
        );
        assert_eq!(
            uses[0].1.get("line").and_then(|v| v.as_i64()),
            Some(EXPECTED_LINE),
            "{provider}: line arg"
        );
    }
}

#[test]
fn anthropic_matches_canonical_interaction() {
    assert_identical_interaction("anthropic", "anthropic.sse", true);
}

#[test]
fn openai_chat_matches_canonical_interaction() {
    assert_identical_interaction("openai-chat", "openai_chat.sse", true);
}

#[test]
fn openai_responses_matches_canonical_interaction() {
    assert_identical_interaction("openai", "openai_responses.sse", true);
}

#[test]
fn gemini_carries_usage_metadata() {
    let acc = replay("gemini", "gemini.sse");
    assert!(acc.done);
    // Gemini reports prompt/candidates counts, and separately thoughts + cached.
    assert_eq!(acc.usage.input_tokens, EXPECTED_INPUT_TOKENS);
    assert_eq!(acc.usage.output_tokens, EXPECTED_OUTPUT_TOKENS);
    assert_eq!(
        acc.usage.cache_read_tokens, 900,
        "cachedContentTokenCount must land in cache_read"
    );
    assert_eq!(
        acc.usage.reasoning_tokens, 12,
        "thoughtsTokenCount must land in reasoning"
    );
    assert!(acc.text.contains("open"), "gemini text: {:?}", acc.text);
    let resp = acc.into_response("fallback");
    assert_eq!(
        resp.tool_uses().len(),
        1,
        "gemini functionCall must survive as a tool use"
    );
    assert_eq!(resp.tool_uses()[0].1, EXPECTED_TOOL);
}

/// The point of the IR: the same transcript must be expressible everywhere.
#[test]
fn every_adapter_accepts_the_same_transcript() {
    use stoic_codex_providers::{Content, Message, Role, ToolSpec};

    let mut req = stoic_codex_providers::CanonicalRequest::new("model", "start")
        .with_system("You are a coding agent.")
        .with_max_tokens(2048)
        .with_tools(vec![ToolSpec {
            name: "read_file".into(),
            description: Some("Read a file".into()),
            input_schema: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
        }]);
    req.messages.push(Message {
        role: Role::Assistant,
        content: vec![Content::ToolUse {
            id: "call_1".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path":"a.rs"}),
            signature: None,
        }],
    });
    req.messages.push(Message::tools(vec![Content::ToolResult {
        tool_use_id: "call_1".into(),
        content: vec![Content::text("fn main(){}")],
        is_error: false,
    }]));

    let mut ok = 0;
    for id in stoic_codex_providers::adapters::provider_ids() {
        let a = adapter_for(id).unwrap();
        match a.build_request(&req) {
            Ok(w) => {
                ok += 1;
                // Invariants that must hold for every provider, forever.
                assert!(w.url.starts_with("http"), "{id}: url");
                assert!(
                    !w.url.to_lowercase().contains("key="),
                    "{id}: credential leaked into URL"
                );
                assert!(
                    w.header("authorization").is_none() && w.header("x-api-key").is_none(),
                    "{id}: credential must be injected by the transport, not the adapter"
                );
                assert!(w.body.is_object(), "{id}: body must be a JSON object");
                // Providers differ on where the model lives: most put it in the body,
                // Gemini addresses it in the path (POST /models/{model}:stream...).
                let model_in_body = w.body["model"].is_string();
                let model_in_url = w.url.contains(&req.model);
                assert!(
                    model_in_body || model_in_url,
                    "{id}: model appears nowhere in the request"
                );
            }
            Err(e) => panic!("{id}: canonical transcript rejected: {e}"),
        }
    }
    assert!(ok >= 15, "expected all bundled providers to work, got {ok}");
}

#[test]
fn no_provider_leaks_a_secret_shape() {
    // Defence in depth: even if someone wires a key through `extra`, the adapter
    // must not smuggle it into a URL query string.
    use serde_json::json;
    let mut req = stoic_codex_providers::CanonicalRequest::new("m", "x");
    req.extra
        .insert("api_key".into(), json!("sk-should-not-be-here"));
    for id in stoic_codex_providers::adapters::provider_ids() {
        let w = adapter_for(id).unwrap().build_request(&req).unwrap();
        assert!(!w.url.contains("sk-should-not-be-here"), "{id}: key in URL");
    }
}
