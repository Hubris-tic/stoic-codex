# zenithal-providers

Provider-agnostic wire layer: **one canonical IR, many vendor adapters.**
Pure translation — no async, no HTTP, no secrets. 112 tests, 0 warnings, no network required
for the default suite. Two Gemini tests are live (`--ignored`), because the real API taught us
things no cassette could: see [`LIVE.md`](LIVE.md). No paid Anthropic/OpenAI key is needed.

```text
agent code ──► CanonicalRequest ──► WireAdapter ──► vendor JSON + SSE
agent code ◄── CanonicalEvent   ◄── StreamDecoder ◄── vendor JSON + SSE
```

## Why this shape

Every provider-specific trap lives in exactly one file, so adding a provider never
touches agent code. The library owns *translation*, not I/O: the existing
`http-client` crate executes the requests, which makes the whole thing testable
against recorded cassettes with no keys and no flakes.

| Trap | Provider | Where it is handled |
|---|---|---|
| `system` is a top-level field, not a message | Anthropic | `adapters/anthropic.rs` |
| `tool_result` must live in a **user** turn | Anthropic | `adapters/anthropic.rs` |
| Roles must alternate; harness emits runs | Anthropic | merged during `build_request` |
| Tool args stream as partial JSON strings | Anthropic, OpenAI | `StreamAccumulator` |
| Tool args arrive tagged by `index`, id only on the first fragment | OpenAI chat | `OpenAiChatStream` |
| `usage` only appears with `stream_options.include_usage` | OpenAI chat | `Quirks::needs_stream_options` |
| Some servers want `max_completion_tokens` | o-series, several compatibles | `MaxTokensField` |
| Reasoning text is `reasoning_content` / `reasoning` | DeepSeek, OpenRouter, xAI | `Quirks::reasoning_field` |
| Stream deltas keyed by `item_id`, not index | OpenAI Responses | `ResponsesStream::item_to_index` |
| Tool-call ID may be absent on older models | Gemini | preserve the real id when present; transcript name map is the fallback |
| Function args arrive complete, not as deltas | Gemini | emitted as one delta |
| Roles are only `user` / `model`; results are `functionResponse` | Gemini | `wire_role`, `parts_of` |
| API key must not go in a URL | all | `AuthSpec` + test `no_provider_leaks_a_secret_shape` |

## Secrets never enter this crate

Adapters emit an `AuthSpec` (`Bearer`, `Header("x-api-key")`, `Header("x-goog-api-key")`,
or `None` for local servers). The transport resolves it from the credential store.
Consequences: no key in adapter structs, none in `Debug` output, none in traces, none
in error messages, and a test asserts no URL ever contains `key=`.

## Bundled providers

`anthropic` · `gemini` · `openai` (Responses) · `openai-chat` · `openai-responses`
plus compat presets: `groq`, `mistral`, `openrouter`, `deepseek`, `xai`, `together`,
`fireworks`, `cerebras`, `ollama`, `lmstudio`, `vllm`.

```rust
use zenithal_providers::{adapter_for, CanonicalRequest, SseDecoder, StreamAccumulator};

let adapter = adapter_for("anthropic")?;
let req = CanonicalRequest::new("claude-sonnet-4-5", "explain this repo")
    .with_system("You are a coding agent.")
    .with_cache(zenithal_providers::CacheStrategy::Prefix);

let wire = adapter.build_request(&req)?;   // POST, url, headers, AuthSpec, JSON body
// ... transport executes `wire`, feeds the response body into:
let mut sse = SseDecoder::new();
let mut dec = adapter.stream_decoder(&req);
let mut acc = StreamAccumulator::default();
for frame in sse.push(body_chunk) {
    for ev in dec.push_frame(&frame)? { acc.apply(ev); }
}
let response = acc.into_response("fallback-model");
```

Inspect any provider's wire format without a network:

```bash
cargo run --example show_wire -- anthropic
cargo run --example show_wire -- all
```

## Model catalogues and free-tier signals

The CLI's `zenithal providers models <id> [--free]` sends a read-only catalogue GET, not a
completion. Built-in adapters expose Gemini `/v1beta/models`, OpenAI-compatible `/models`
(including Groq and OpenRouter), and Anthropic `/v1/models`; OpenAI-compatible custom specs
can derive `/v1/models` from their `/v1/chat/completions` path. The parser reports only fields
the service actually returns: id, display name, context, and any input/output price.

`--free` filters only a `:free` id or an explicit zero for both prices. A missing price is
**unknown**, not free. In particular, Google AI Studio and Groq free quota/eligibility may be
account- or model-specific and may not appear in their catalogue. The command cannot confirm
your billing tier; use it to discover IDs, then check the provider's own free-quota page.

## Routing and cost

```rust
use zenithal_providers::{Registry, Router, TaskClass, Requirements};
let router = Router::new(Registry::seeded());   // or Registry::from_json(&config)
let sel = router.select(TaskClass::Plan, &Requirements { prefer_local: true, ..Default::default() })?;
println!("{} via {} — {}", sel.model, sel.provider, sel.rationale.join("; "));
```

`select` is deterministic, gates on hard requirements (context, tools, cost ceiling,
tier ceiling) and returns a human-readable rationale for every choice, because a router
you can't audit is a router you can't debug.

Costs are integer micro-USD with cache-aware multipliers; `Ledger` tracks a session and
reports `cache_hit_ratio()`, which is the number that actually drives savings. This is a
rate-card estimate, not an account invoice or a statement that a free-tier call was billed.

## Verified against the real wire

[`LIVE.md`](LIVE.md) is the record of the first pass against the live Gemini API with a real
key. It is worth reading before touching `adapters/gemini.rs`, because two of the rules in
there exist only because the service enforced them:

- a Gemini 3.x function call must be replayed **with its `thoughtSignature`**, or the follow-up
  turn is a `400` — which means the *second* turn of any tool loop, and only the second;
- its function-declaration schema is a restricted OpenAPI subset that **rejects** unknown
  keywords, including the `additionalProperties` our own tool schemas emit.

Both are pinned by offline tests built from captured payloads, plus two `#[ignore]`d live tests.

## Tests

```bash
cargo test    # 112 tests: adapters, model catalogues, cassettes, SSE, routing, cost
```

- `tests/cassette_conformance.rs` — all four wire formats feed one identical
  interaction (two sentences + `read_file(path, line)`) and must produce the same
  canonical result, including token accounting. Fed in 7-byte chunks to prove
  boundary handling rather than whole-file luck.
- `tests/sse_spec.rs` — CRLF split across chunks, multi-line `data:`, field names
  split mid-word, keep-alive comments, `[DONE]`, 10k-frame throughput.
- `tests/router.rs`, `tests/cost.rs` — selection and billing.

## Adding a provider

1. OpenAI-compatible? Add one entry to `PRESETS` in `adapters/compat.rs`. Done.
2. Different protocol? New file in `adapters/`, implement `WireAdapter` + `StreamDecoder`.
3. Record a cassette with the canonical interaction and add one line to
   `cassette_conformance.rs`. If it passes, the adapter is done — no other code changes.

## Capabilities

`WireAdapter::capabilities()` is how the harness learns what a wire format can carry before it
sends anything: tools, parallel tools, streaming usage, reasoning, cache style, context limits.
A user-defined provider declares its own in its JSON spec and, if it says nothing, gets a
conservative inference from `api_style` (OpenAI-shaped ⇒ tools + streaming; templated ⇒
streaming only).

## Caveats

- `Registry::seeded()` model names and prices are **placeholders**. Model lineups and
  pricing change monthly; load them from config or the vendor's `/models` endpoint.
- No async/retry/backoff here by design — that is transport policy and belongs in
  `http-client` so every provider inherits the same behaviour.
- `Content::Thinking` replay requires a signature on Anthropic; an unsigned block is a
  clear `Config` error rather than a 400.
