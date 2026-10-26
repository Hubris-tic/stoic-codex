# The live provider pass (2026-10-26)

Everything before this document was proven against recorded cassettes and loopback stubs. That
is a real form of proof — the protocol, the SSE reassembly, the retry paths — but it cannot
tell you that a vendor *changed its wire format last month*, because the cassettes were
written from the same assumptions as the adapter.

So: one afternoon against the real Gemini API, with a real key. It found two bugs that no
cassette ever would, both of which made tool calling **completely unusable** on the newest
models. Neither is mentioned in the documentation the adapter was written from. This was
Google AI Studio under the user's free-tier-only constraint; Anthropic and OpenAI were not
called. Dollar figures below are rate-card estimates, not billing-console amounts.

## What was run

| Step | Result |
|---|---|
| `GET /v1beta/models` | 200 — 47 models, including ids we had never seen (`gemini-3.8-flash`, `gemini-4-argon`, `gemini-3.1-flash-lite`) |
| `generateContent` | 200 — 1.6 s, `usageMetadata` with `promptTokenCount: 8, candidatesTokenCount: 1` |
| `streamGenerateContent?alt=sse` | 200 — 0.8 s, three `data:` frames, usage in every frame, `finishReason` only in the last |
| `zenithal run "…"` (our binary, our adapter, our transport) | 200 — streamed answer, `152 ms`–`13.7 s` depending on the moment, `$0.000045` for 154 in / 4 out |
| `zenithal agent run "read src/auth.rs…"` (two turns, one tool) | **success** — the model called `read_file`, got the result, answered |
| `zenithal agent run "edit hash()…"` (four turns, three tools) | **success** — read → `edit_file` → read back → answer, file correct on disk |

The last two are the milestone: the first time this project has done real, agentic, multi-turn
work against a live vendor, with a real tool loop, through the shipped binary.

## Bug 1 — `thoughtSignature` is mandatory on replayed calls

A Gemini 3.x function call comes back with an opaque `thoughtSignature` attached to the same
part:

```json
{"functionCall": {"name": "read_file", "args": {"path": "src/auth.rs"}, "id": "call_856357"},
 "thoughtSignature": "EnMKcQFpFH0Tt0u7RUMu…"}
```

Replaying that call on the next turn **without** the signature is a hard failure:

```
HTTP 400  INVALID_ARGUMENT
Function call is missing a thought_signature in functionCall parts. This is required for
tools to work correctly, and missing thought_signature may lead to degraded model performance.
```

Measured directly: the same follow-up request returns 200 with the signature and 400 without
it, twice, from a clean script. Our adapter was dropping it, so the *second* turn of every tool
loop against a 3.x model would have died — the first turn alone looks perfect.

**Fix.** The signature belongs to the call, so it lives on the call: `Content::ToolUse` gained
`signature: Option<String>`, and Gemini writes it back on the same part. The same handle is
carried by the streaming path (`ToolUseEnd { index, signature }` → `ToolCallBuf.signature`).

The streaming case has a wrinkle worth writing down: Gemini does not stream arguments, and it
may deliver the signature in a *later* chunk than the call, or on a chunk of its own. Emitting
the call the moment it arrives would therefore guarantee a signature-less replay. `GeminiStream`
now buffers complete calls and releases them at `finishReason`, where whatever signature arrived
can still be attached. That ordering is asserted by a test built from two real frames.

**Also learned from the same response:** Gemini now returns a real `functionCall.id`
(`call_856357`). The adapter had "there is no tool-call id" in its header comment, synthesised
`call_{n}_{name}`, and matched results by name. Ids are now used when present, synthesised only
for older models.

## Bug 2 — the function schemas were illegal

Every tool call failed:

```
HTTP 400  INVALID_ARGUMENT: Invalid JSON payload received. Unknown name "additionalProperties"
at 'tools[0].function_declarations[0].parameters': Cannot find field.
```

Gemini's `parameters` is a restricted OpenAPI subset, and it does not *ignore* unknown
keywords — it rejects the request. Our tool registry emits ordinary JSON Schema, which includes
`additionalProperties: false` on object parameters, so the request was refused before the model
ever saw a tool.

**Fix.** `declaration_schema()` rewrites a schema into the subset with an **allow-list**:
`type`, `format`, `description`, `enum`, `items`, `properties`, `required`, `anyOf`,
`minimum`, `maximum`, `minItems`, `maxItems`, `default` survive; everything else is dropped,
recursively. Fail-closed is deliberate — a keyword we do not know is a keyword that may break
the request.

## What else changed while we were in there

- **`WireAdapter::capabilities()`** — the gap `README.md` listed. Every adapter now reports
  what its wire format can carry, so the router gates on fact instead of hope. A user-defined
  provider (`custom.rs`) can *declare* its own capabilities and otherwise gets a conservative
  inference from its `api_style`: OpenAI-shaped means tools and streaming, templated means
  streaming only.
- **Thinking parts.** Gemini 3.x marks reasoning with `"thought": true`. Those parts were being
  emitted as answer text. They are now `ThinkingDelta` / `Content::Thinking`, so the model's
  scratchpad stops appearing in the user's answer.
- **`stop_reason`.** Gemini reports `finishReason: "STOP"` for a function call. A response that
  contains a call now maps to `StopReason::ToolUse` regardless, or a loop would treat a tool
  turn as a finished answer and strand the call.
- **The model registry was stale, and this is now visible in the cost line.** An earlier
  completion attempt with `gemini-2.5-flash` returned 404 for the key used in that test, so
  the seed was replaced with `gemini-3.1-flash-lite` and `gemini-3.8-flash`, priced from
  [ai.google.dev/gemini-api/docs/pricing](https://ai.google.dev/gemini-api/docs/pricing) on
  2026-10-26. A later read-only model-catalogue GET did list `gemini-2.5-flash`; listing an ID
  is **not** proof that every key/account can generate with it or use it in a free quota. The
  current catalog returned 46 chat-capable IDs:

  | Model | Input $/M | Cached input $/M | Output $/M |
  |---|---|---|---|
  | `gemini-3.1-flash-lite` | 0.25 | 0.025 | 1.50 |
  | `gemini-3.8-flash` | 0.75 | 0.075 | 3.75 |

  Note for whoever reads this next year: 3.8 Flash is on an **introductory** rate through
  2026-12-31 and doubles to 1.50 / 7.50 on 2027-01-01. A cost report is only true for the
  date in this file.
- **`Registry::resolve()`** looks a model up by registry id *or* by wire name, because
  `--model gemini-3.1-flash-lite` is what a person types and `gemini:flash-lite` is what the
  registry calls it. Before this, naming the model the way the vendor does silently degraded
  to "cost unknown".

## Token calibration

The tokenizer in `tokens.rs` is a linear estimator, and it exists because `chars / 4` is
wrong by up to 4× on real input. The constants were fitted here: seven samples, sent one at a
time as the entire `contents`, `maxOutputTokens: 1`, and the reported `promptTokenCount` read
back.

| Sample | chars | **live tokens** | chars/token | estimate |
|---|---|---|---|---|
| English prose, 3 sentences | 134 | 31 | 4.3 | 32 |
| Rust, one line | 86 | 31 | 2.8 | 30 |
| JSON object | 82 | 33 | 2.5 | 35 |
| digits, dates, a float | 43 | **44** | **1.0** | 42 |
| Bengali | 38 | 10 | 3.8 | 10 |
| Japanese | 22 | 12 | 1.8 | 12 |
| `hello` | 5 | 2 | 2.5 | 2 |

What this changed:

- **Digits are the most expensive characters in the language** — about 1.1 tokens each, so a
  43-character string of numbers costs more than a 134-character paragraph of prose. Any
  context budget that treats digits as ordinary text under-counts hashes, ids and timestamps
  by an order of magnitude.
- **Non-Latin script is cheaper than Latin** on this tokenizer, not more expensive: Bengali at
  3.8 chars/token and Japanese at 1.8 beat the naive rule's 4 for prose. An estimator that
  assumed "one token per CJK character" would over-charge a Bengali prompt by 5×.
- **Punctuation is charged individually** (0.75 each in the model), which is why JSON costs
  more per character than prose.
- A single space is nearly free (0.1), but indentation and newlines are not.

Fitted model, per unit: word 1.0 · word character beyond the 7th 0.15 · ASCII punctuation 0.75 ·
**digit 1.1** · space 0.1 · CJK 0.55 · other non-ASCII 0.25 · fixed overhead 0.5.

Max error across these seven: **6.1%**; five of seven are within 3.5%. That is not a tokenizer,
and `tokens.rs` says so — but it is good enough to decide what fits, and it is checked against
these numbers by a test, so it cannot drift silently. Re-measure before trusting it on a
different vendor: these constants describe Gemini's vocabulary, not the concept of a token.

## Re-running it

```bash
export GEMINI_API_KEY=…            # never write this into a file that can be committed
cargo test --test live_gemini -- --ignored --test-threads=1
# or against a different model:
ZENITHAL_LIVE_MODEL=gemini-3.8-flash cargo test --test live_gemini -- --ignored --test-threads=1
```

Two live tests, both `#[ignore]`d so the default suite stays free, offline and deterministic:

- `a_real_turn_reports_real_tokens_and_a_rate_card_estimate` — one turn; the stderr line must
  carry tokens and a labelled registry-rate estimate, explicitly not billing data.
- `a_real_tool_loop_survives_the_second_turn` — a real repo, a real tool call, and the
  assertions are exactly the two bugs above: the transcript must contain a tool call (schema
  translation) and the loop must finish without a 400 (signature round-trip).

The default suite still runs with no key and no network. The live Gemini tests are opt-in and
must only be run within the user's confirmed free quota. The displayed price is a list-rate
estimate; Zenithal does not know whether a provider applied free-tier credits or what appears
on the user's bill.

## Cost of this document

The probes and agent loops were small (roughly 1.5k input / 300 output tokens in the probing,
plus tool turns). The rate-card equivalent is below one cent on `gemini-3.1-flash-lite`; this
is not a verified charge. Free-tier quota/billing status can only be confirmed in the user's
Google AI Studio account.
