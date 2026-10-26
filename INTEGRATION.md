# Integrating `stoic-codex-providers` into Stoic Codex

Stoic Codex is a hard fork, so you *can* edit upstream files freely. This document is still
worth following literally: every change below is placed at a **seam**, so that a later
rebase (or a change of heart about the fork strategy) is mechanical instead of a rewrite.
The seam list is the map of which files to touch and what breaks if you get it wrong.

## 0. Where the crate goes

```
codex-rs/
  providers/               <- copy of this crate, added to [workspace] members
  model-provider-info/     <- PATCH: provider registry gains a `wire` kind
  model-provider/          <- PATCH: capabilities come from stoic_codex_providers
  http-client/             <- PATCH: execute WireRequest, resolve AuthSpec
  config/                  <- PATCH: [[providers]] section
  core/                    <- PATCH: turn loop consumes CanonicalEvent
```

Add to `codex-rs/Cargo.toml` `[workspace] members`: `"providers"`.

## 1. `model-provider-info` — declare the wire kind

Upstream's `ModelProviderInfo` already carries a base URL, env key name and wire API.
Add one field so the registry can name an adapter:

```rust
// codex-rs/model-provider-info/src/model_provider_info.rs
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WireApi {
    Responses,
    Chat,          // existing variants, if any, keep as-is
}

// NEW
impl ModelProviderInfo {
    /// Which stoic_codex_providers adapter speaks this provider's protocol.
    pub fn wire_adapter_id(&self) -> &'static str {
        match self.wire_api {
            WireApi::Responses => "openai-responses",
            WireApi::Chat => "openai-chat",
        }
    }
}
```

Keep the existing `WireApi` semantics untouched; only add a mapping function. This is the
one place that knows the relationship between "config value" and "adapter id".

## 2. `model-provider` — capabilities become data

Upstream has `capabilities.rs` with per-provider assumptions. Replace those assumptions
with `stoic_codex_providers::Capabilities`:

```rust
// codex-rs/model-provider/src/capabilities.rs
pub use stoic_codex_providers::capabilities::{Capabilities, Feature, Needs};

pub fn capabilities_for(provider_id: &str, model: &str) -> Capabilities {
    // 1. adapter default   2. registry override from config   3. explicit user override
    stoic_codex_providers::adapter_for(provider_id)
        .map(|a| /* adapter's declared caps */)
        .unwrap_or_default()
}
```

Expose the adapter's declared capabilities on `WireAdapter` if you want them per-adapter
(the trait currently has no `capabilities()` method — add one; the compat presets already
carry them in `Preset::to_adapter`).

## 3. `http-client` — the only place that touches a key

```rust
// codex-rs/http-client/src/wire.rs
use stoic_codex_providers::{AuthSpec, WireRequest};

pub async fn execute(client: &reqwest::Client, req: WireRequest, auth: &dyn AuthStore)
    -> Result<impl Stream<Item = Result<Bytes>>>
{
    let mut rb = client.request(reqwest::Method::from_bytes(req.method.as_bytes())?, &req.url);
    for (k, v) in &req.headers { rb = rb.header(k, v); }
    match req.auth {
        AuthSpec::Bearer          => rb = rb.bearer_auth(auth.get()?),
        AuthSpec::Header(name)    => rb = rb.header(name, auth.get()?),
        AuthSpec::Query(param)    => rb = rb.query(&[(param, auth.get()?)]),
        AuthSpec::None            => {}
    }
    let resp = rb.json(&req.body).send().await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        return Err(adapter.parse_error(status, &body));   // typed, retryable-aware
    }
    Ok(resp.bytes_stream())
}
```

Wire `auth.get()` to the existing credential plumbing (`login`, `secrets`,
`keyring-store`, `auth.json`). Do **not** let adapters see the key — that invariant is
worth enforcing in review, and there is a test for it.

Retry policy belongs here too: `Error::is_retryable()` (429/5xx) → exponential backoff
with jitter; `Error::is_fatal_for_provider()` (401/403/404) → switch provider, don't
retry forever.

## 4. `config` — user-facing BYOK

```toml
# ~/.stoic-codex/config.toml
[model_providers.anthropic]
wire        = "anthropic"            # adapter id; see `provider_ids()`
base_url    = "https://api.anthropic.com"
env_key     = "ANTHROPIC_API_KEY"    # or "keyring:anthropic", "file:~/.secrets/anthropic"

[model_providers.local-llama]
wire        = "vllm"
base_url    = "http://localhost:8000/v1"
env_key     = ""                     # local servers need no credential

[routing]
plan      = "anthropic:claude-sonnet"
patch     = "openai:gpt-5-codex"
classify  = "gemini:flash"
prefer_local_for = ["**/secrets/**", "**/.env*"]
```

`[routing]` maps straight onto `stoic_codex_providers::Router` + `TaskClass`; the
`prefer_local_for` glob list is the privacy feature that makes BYOK worth having.

## 5. `core` — consume canonical events

The turn loop currently streams vendor events into `protocol::ResponseItem`. Insert the
adapter boundary and keep producing the same items, so nothing downstream changes:

```rust
let adapter = stoic_codex_providers::adapter_for(provider.wire_id())?;
let wire = adapter.build_request(&canonical_from_history(&history, &turn))?;
let body = http_client::execute(&client, wire, &auth).await?;

let mut sse = SseDecoder::new();
let mut dec = adapter.stream_decoder(&canonical);
let mut acc = StreamAccumulator::default();

for chunk in body {
    for frame in sse.push(std::str::from_utf8(&chunk)?) {
        for ev in dec.push_frame(&frame)? {
            acc.apply(ev.clone());
            emit_protocol_item(ev, &mut tx).await?;   // your existing item mapping
        }
    }
}
let final_response = acc.into_response(&model);
```

Mapping (verify variant names against `codex-rs/protocol`):

| `CanonicalEvent` | `protocol::ResponseItem` |
|---|---|
| `TextDelta` | `Message` with `Role::Assistant` (accumulate, then emit) |
| `ThinkingDelta` | `Reasoning` |
| `ToolUseBegin` + `ToolUseInputDelta` + `ToolUseEnd` | `FunctionCall { name, arguments, call_id }` |
| `Usage` | your token-accounting event |
| `Stop` / `Done` | end of turn |
| `Error` | error item; consult `is_retryable()` |

Keep `StreamAccumulator` as the single source of truth for the final assistant message —
do not reconstruct it in the TUI, or the TUI and the web UI will disagree about
transcripts.

## 6. Failure modes to test before shipping

| Scenario | Expected |
|---|---|
| Provider returns 429 | retry with backoff, not provider switch |
| Provider returns 401 | `is_fatal_for_provider()`, fall back to next provider, tell the user |
| Model lacks tool support but harness needs tools | router filters it out (`Requirements::needs.tools`) |
| Stream ends without `[DONE]` | `finish()` still emits exactly one `Done` |
| Tool args truncated by cancellation | `into_response` yields `{}` args, transcript stays usable |
| Anthropic receives thinking block without signature | clear `Config` error, not a 400 |
| Gemini receives a tool result for an unknown call | clear `Config` error naming the reason |
| User sets both `max_tokens` and `max_completion_tokens` quirks | only the correct key is sent |

## 7. What is NOT done yet (honest list)

- `WireAdapter` has no `capabilities()` accessor; per-adapter caps live in `compat::Preset`
  and the `Anthropic::capabilities()` / `Gemini::capabilities()` helpers. Add the trait
  method when you wire §2.
- No tokenizer integration: `Usage` comes from the provider. Local models often report
  nothing, so `Ledger` will under-count on `ollama`/`vllm`. Plan a local tokenizer behind
  a feature flag; do not guess tokens with `len()/4` in a product that publishes costs.
- No provider-side prompt-cache accounting for OpenAI's automatic caching beyond
  `cached_tokens` passthrough.
- Bedrock and Vertex need SigV4/GCP auth. The adapters exist upstream (AWS/GCP crates);
  the clean route is a `AuthSpec::Custom("aws-sigv4")` variant resolved in `http-client`,
  which is a small, well-scoped addition — not done here.
- Pricing in `Registry::seeded()` is illustrative. Replace before you publish cost numbers.
