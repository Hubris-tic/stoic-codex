# Zenithal

Provider-agnostic coding agent. Fork of `openai/codex`, rebuilt around BYOK,
multi-agent missions — one boss, N isolated worktree agents — and hybrid vector+graph memory.

The interface is a black hole: a void that absorbs light, an accretion disk at the hot end of
red, and the cold blue of the light that escaped. The `hole` tab draws one for real —
integrated null geodesics, not a spiral animation.

_Working tree for the fork — provider layer first._

## Layout

```
zenithal/
  providers/            <- zenithal-providers crate (bypasses nothing; drop into codex-rs/providers)
    src/                <- IR, adapters, SSE, routing, cost
    tests/              <- cassettes, SSE spec, routing, cost
    examples/show_wire  <- inspect any provider's wire format, no network needed
    src/custom.rs       <- user-defined providers: JSON spec, templates, paths, stream map
    CUSTOM-PROVIDERS.md <- how to write your own API (two levels of escape)
    examples/custom_provider*.json <- ready-to-copy specs
  memory/               <- zenithal-memory crate (graph + vector, swappable backend)
    src/                <- domain, trait, ranking, in-process backend, daemon manager
    src/typedb_driver   <- real TypeDB backend, `--features typedb`, tested live
    schema/memory.tql   <- TypeDB 3.x schema + recursive `reachable` function
    TYPEDB.md           <- decision record + what the live server taught us
  agent/                <- zenithal-agent crate: tool loop, sandbox, worktrees, the orchestrator
    src/loop_.rs        <- single-agent loop: Model trait, events, stop causes
    src/tools/          <- read/write/edit/list/search/shell/git tools, all sandboxed
    src/worktree.rs     <- Repo, Worktree, branches, patches, copy_file_to
    src/strategos.rs    <- Strategos plans, names The Phalanx, reviews; work lands
    README.md           <- the mission diagram and the rules that make it safe
  cli/                  <- zenithal-cli crate: the `zenithal` binary (providers + memory + agents)
    src/cmd/            <- run, agent, mission, providers, memory, daemon; no process::exit
    src/model.rs        <- the bridge: zenithal-agent's Model trait over the live transport
    src/net/            <- the only I/O: blocking HTTP/1.1 + rustls, no async runtime
    src/session.rs      <- prompt assembly: identity → fenced memory → task
    dev/stub_provider.py <- fake OpenAI-compatible server: exercise `run` with no model
    README.md           <- command reference: config, backends, exit codes, network
  tui/                  <- zenithal-tui-views: the interface (ratatui 0.29, no crossterm in the lib)
    src/theme.rs        <- the black-hole palette; heat() is the accretion-disk ramp
    src/chrome.rs       <- one frame for every screen: header, rule, body, rule, footer
    views/blackhole.rs  <- a real Kerr image on a terminal: nullgeo geodesics, cached per size
    render_preview.py   <- JSON cell dump -> PNG / GIF; the previews are the shipped render
    DESIGN.md           <- the rules, each held by a test
  config/               <- example BYOK config + model catalogue
  ci/upstream-sync.yml  <- weekly drift detection for the hard fork
  INTEGRATION.md        <- exact seams in codex-rs to patch, with code
  PATCHES.md            <- ledger of every upstream file we diverge from
  ../AGENTIC-UPGRADE-PLAN.md <- the overall audit + roadmap
```

## Where the name actually appears

| Surface | Zenithal? |
|---|---|
| GitHub repo, crate, docs, config examples | **yes** |
| `zenithal-providers` (all 16 provider ids) | **yes** |
| **TUI** | **no — still upstream `codex`**. 185 user-visible strings + 12 OpenAI URLs |
| CLI binary | **no — still `codex`** (one-line fix, see `NAMING.md`) |

Rebranding the TUI is real work, not a sed: `rebrand/strings.md` has the full inventory and
`rebrand/apply-rebrand.sh` does the mechanical part (17 verified replacements). The
dangerous items are not cosmetic — the fork currently fetches OpenAI's announcement feed,
links bug reports to `openai/codex`, and `zenithal app` would download and install OpenAI's
signed desktop binary.

## Verified state

| Check | Result |
|---|---|
| `cargo build` | clean, **0 warnings** in all seven crates |
| `cargo test` | **527 passed, 0 failed** (cli 125 · providers 112 · memory 90 · agent 84 · tui 60 · ui-core 39 · brand 17); 4 ignored are live-network tests; **+9** with `--features typedb` incl. 7 live TypeDB tests |
| **A real vendor call** | **verified live** — Gemini 3.1 Flash-Lite, 2026-10-26: a streamed turn, two full agentic tool loops (read → edit → verify), and a read-only model-list GET through the shipped CLI. No Anthropic/OpenAI paid API calls. Cost display is a rate-card estimate, not billing data: [`providers/LIVE.md`](providers/LIVE.md) |
| Providers wired | 16 ids (5 first-class + 11 compat presets) |
| Network needed for the default tests | none — cassette/loopback only; 4 opt-in ignored live tests |
| `zenithal-brand` crate | 17 tests green (identity self-consistency, no OpenAI URLs, and the Phalanx code catalogue) |
| TUI rebrand | **patch 0001 ready** — 4 supply-chain bugs fixed + guard; `cargo check` still needed |
| TUI views | **built** — diff review, mission board (Strategos pinned above The Phalanx), memory graph, and the black hole; 60 tests, compiles vs ratatui 0.29 |
| TUI design system | **built** — `tui/src/theme.rs` + `tui/src/chrome.rs`: a black-hole palette with red and black as the only primaries, `heat()` as the disk ramp, one shared frame with four tabs, an activity feed under the mission table; rules in `tui/DESIGN.md`, each held by a test |
| **Black hole screen** | **built** — `tui/views/blackhole.rs` renders a real Kerr image on a terminal: null geodesics integrated per cell with [`nullgeo`](https://crates.io/crates/nullgeo) (MIT, CPU-only, no GPU, no network), ISCO-bounded disk, photon ring, relativistic beaming, cached raster and a Keplerian flow animation; analytic fallback if the tracer ever fails |
| Previews | `tui/preview/{mission,mission-narrow,diff,memory,hole,hole-narrow}.png` + `hole.gif` — **rendered from the shipped views** (`cargo run --example snapshot`, `--example frames`), not hand-drawn |
| **Multi-agent (`zenithal-agent`)** | **built** — `run_mission`: Strategos plans, names The Phalanx, N operators work in N git worktrees, Strategos reviews, accepted files land whole, one commit |
| Mission end-to-end | **verified** — 11 tests in `agent/tests/mission.rs` + 15 in `cli/tests/mission.rs` on real repos, real worktrees, real threads, one loopback socket |
| Memory layer | **built** — `zenithal-memory`, 90 tests; TypeDB is an opt-in backend, not a hard dep |
| **CLI (`zenithal`)** | **built** — `zenithal-cli`, 125 tests; `run`, `agent`, `mission` (run/plan), `tools`, `providers` (incl. `models`, `test`), `memory`, `context`, `daemon` |
| **A real turn** | **verified over a socket** — 13 integration tests drive the shipped transport against a loopback HTTP server: streaming, non-streaming fallback, 429/500 handling, refused connections, credential handling, memory write-back and graph growth |
| TLS | **verified live** — a real certificate chain from the system store is accepted, and a mismatched certificate is refused (`cargo test --test net_live -- --ignored`) |
| **Retries** | **built** — 4 attempts by default, exponential backoff with equal jitter, `Retry-After` honoured, and never a retry after a delta has been printed (that would duplicate the answer). `providers.max_attempts` in config |
| **Token budget** | **built** — a local estimator in `providers/src/tokens.rs`, fitted against live `promptTokenCount` values and within 7% of them; `memory context` now reports tokens as well as characters. Digits cost ~1.1 tokens each, which is why a char count was never enough |
| `providers models <id> [--free]` | **built** — read-only catalogue GET; IDs, context and reported prices; filters only explicitly free/zero-priced models |
| `providers test <id>` | **built** — one real completion ping: status, latency, tokens, reply; use only with a confirmed free/local endpoint |
| End-to-end, daemon | **verified** — `memory init/remember/link/recall/context/stats` against a real local TypeDB 3.14.1, data persisted across separate processes |
| No-daemon path | **verified** — same commands on the JSON file store; in-process store when neither exists |
| TypeDB backend | **verified live** — 7 integration tests against a local TypeDB 3.14.1 |
| Local daemon | **built** — spawn/attach/stop on loopback; never a hosted service |
| Works with no daemon installed | **yes** — `auto` falls back to a file, never to a hosted service |
| Custom providers | **built** — JSON spec: OpenAI-compatible *or* user-written templates + paths |
| Secrets in adapter state | none by construction (see `AuthSpec`) |

## What the provider layer gives you

- One `CanonicalRequest` / `CanonicalEvent` IR. Adding a vendor does not touch agent code.
- Four real wire protocols implemented: Anthropic Messages, OpenAI Chat Completions,
  OpenAI Responses, Gemini — each with its specific traps handled and tested.
- 11 OpenAI-compatible vendor presets with their quirks encoded as data.
- Capability-aware router with deterministic, auditable selection.
- Live model-catalogue discovery for Gemini and OpenAI-compatible endpoints, plus explicit free markers where the provider reports them.
- Cache-aware cost ledger in integer micro-USD (rate-card estimate, not an invoice).

## The mission: Strategos and The Phalanx

The multi-agent model is one boss spawning N isolated workers — not a panel of peers and
not an integrator with a merge strategy.

```
             task
               │
               ▼
        ┌─────────────┐      plans + names: one job and one code each
        │  STRATEGOS  │─────────────────────────────────────────┐
        │  no tools   │                                         │
        └──────┬──────┘                                         ▼
               │                                    ┌───────────┬───────────┬───────────┐
               │                                    │ x0-core   │ x1-write  │ x2-check  │
               │                                    │ worktree  │ worktree  │ worktree  │
               │                                    │ sandbox   │ sandbox   │ sandbox   │
               │                                    └─────┬─────┴─────┬─────┴─────┬─────┘
               │                                          │ commit on zenithal/<code>
               ▼                                          │
        ┌─────────────┐   accept / reject each report      │
        │  STRATEGOS  │◀──────────────────────────────────┘
        └──────┬──────┘
               ▼
     accepted files copied into the working tree (whole file, first accepted writer wins)
               ▼
     one commit: `zenithal mission: N file(s) from …`
```

- **The boss has no tools.** Strategos plans, names and judges; it never edits a file.
- **Everyone has a code, not a name.** An operator starts as `slot-1` and leaves the plan as
  `x0-core` (context harvester), `x1-write` (logic generator), `x2-check` (validation),
  `x3-ship` (deployment), `x4-scan` (retrieval), `x5-patch` (surgical edit), `x6-audit`
  (review) or `x7-trace` (diagnostics). Strategos picks the code from the work it assigned, the
  catalogue lives in `brand/src/phalanx.rs`, and the log prints the meaning next to it:

  ```
  [strategos] name    slot-1     → x1-write · the primary logic generator
  [x1-write]  tool    write_file       {"path":"src/auth.rs"}
  [strategos] done    applied=3 conflicts=0
  ```
- **Each operator is confined** to its own worktree, sandbox, tool registry and model client.
- **Overlap is reported, not merged.** If two agents changed one file, one wins and the
  other is recorded as a conflict and keeps its branch.
- **Wrong plans cost quality, not the run.** An unreadable plan gives everyone the whole
  task; a failed review accepts every clean result and says so.

Detail, including the degradation table: `agent/README.md`. Where the views live:
`ui-core/src/mission.rs` and `tui/views/mission_board.rs`.

## Run it

```bash
cd providers
cargo test                                    # 112 tests, no network
cargo run --example show_wire -- anthropic
cargo run --example show_wire -- all

cd ../memory
cargo test                                    # 90 tests, no server, no network
cargo run --example recall_demo               # in-process memory

cd ../agent
cargo test                                    # 84 tests: unit + real-git-worktree missions

cd ../tui
cargo test                                    # 60 tests: palette, frame, four views
cargo run --example snapshot                  # the shipped screens -> preview/*.png
cargo run --release --example hole            # the black hole, live: q quits, space pauses, j jets

cd ../cli
cargo test                                    # 125 tests, no daemon, no network, no key
cargo test --test net_live -- --ignored       # 2 live TLS tests (needs network)

# One turn against a fake local model server (no weights, no key)
python3 dev/stub_provider.py --port 8099 &
cat > /tmp/lab.json <<'JSON'
{ "id": "lab", "base_url": "http://127.0.0.1:8099/v1", "api_style": "openai",
  "models": ["tiny"], "auth": { "kind": "none" } }
JSON
./target/debug/zenithal providers add /tmp/lab.json
./target/debug/zenithal providers test lab --model tiny   # local fake completion; no vendor spend
./target/debug/zenithal memory remember file:auth.rs auth.rs --text "token verification"
./target/debug/zenithal run "what do you know about auth?" --provider lab --model tiny

# The actual product surface, with no daemon present (falls back to ~/.zenithal/memory.json)
./target/debug/zenithal memory remember file:auth.rs auth.rs --text "token verification"
./target/debug/zenithal memory recall token
./target/debug/zenithal context "token expiry" --budget 600

# Same commands using a real local TypeDB daemon
cargo build --features typedb
ZENITHAL_TYPEDB_BIN=/path/to/typedb ./target/debug/zenithal memory init
ZENITHAL_TYPEDB_BIN=/path/to/typedb ./target/debug/zenithal memory stats

# Whole local stack: spawn a TypeDB daemon, use it, stop it
ZENITHAL_TYPEDB_BIN=/path/to/typedb \
  cargo run --features typedb --example local_stack

# Live integration tests against a local daemon
cargo test --features typedb --test typedb_live   # needs ZENITHAL_TYPEDB_ADDR
```

## Next steps, in order

1. **M0 hygiene** — clone the fork locally, add `upstream` remote, drop in
   `ci/upstream-sync.yml`, capture baseline cold-start/RSS/TTFT numbers on your machine
   (this sandbox has 2 cores / 1 GB and cannot build the 2.1M-line workspace).
2. **M1 wire it in** — follow `INTEGRATION.md` §1–§5, recording each row in `PATCHES.md`.
3. **M2 prove it, free-tier only** — use the Google AI Studio free quota, OpenRouter `:free`
   models, Groq free tier (if enabled on the account), and a local model. No paid Anthropic/OpenAI
   calls; separate the displayed rate-card estimate from actual account billing.
4. **`zenithal stream` / transcript** — `run` sends one user turn and prints deltas; a real
   session needs a pty-ish renderer (markdown, tool calls, diffs) and `--continue`.
5. **Conversation state** — `run` is one turn with the memory block as its only history. A
   session needs a transcript in the request (`messages`, not just `messages[0]`), a token
   budget that evicts oldest-first, and `--continue` to resume.
6. ~~**First real vendor call**~~ — **done** (`providers/LIVE.md`): Gemini 3.1 Flash-Lite,
   streamed turn plus a four-turn tool loop. The `$0.000045` figure is a rate-card estimate,
   not a verified charge. The live pass found two bugs — mandatory `thoughtSignature` replay
   and an illegal tool schema. `providers models` now lists the service's current catalogue;
   next live catalogue checks are OpenRouter/Groq only if their free-tier keys are available.
7. **Drive the mission board live** — `run_mission` already emits `MissionEvent`s that map
   1:1 onto `ui-core::mission`; what is missing is the terminal app that subscribes to them.

## Known gaps in this drop

- ~~`WireAdapter` lacks a `capabilities()` accessor~~ — **added**; every adapter reports what
  its wire format carries, and a user-defined provider can declare its own.
- No AWS SigV4 / GCP auth path for Bedrock and Vertex (upstream has those crates; add an
  `AuthSpec::Custom` variant resolved in `http-client`).
- No local tokenizer: `ollama`/`vllm` report no usage, so cost under-counts for local runs.
- `providers show` prints the request without sending it. `providers models` opens a socket for a
  read-only catalogue GET; `run`, `agent run`, `mission run` and `providers test` send completions.
- **The multi-agent path has never talked to a real model here** — `cli/tests/mission.rs`
  drives the shipped transport against a scripted loopback server, so the protocol,
  worktrees, apply step and commit are real while the model replies are canned.
- The mission board is a **view, not a driver**: there is no terminal app consuming
  `MissionEvent`s yet.
- `memory forget` deletes one node and its edges; there is no bulk `memory reset` command
  yet, and no import from `git`/ripgrep.
- `Registry::seeded()` prices and model names are placeholders — replace from config.
