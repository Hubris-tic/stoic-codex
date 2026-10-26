# Stoic Codex — remaining work

Status as of **2026-10-26**. Everything ticked in §A was re-verified in the preceding validation
round; everything unticked is either unverified, unscheduled, or blocked on something named in
the line. The current container has no `cargo` or `rustc`, so new Rust changes cannot be compiled
here until that toolchain is available.

Legend: `[ ]` open · `[x]` done and verified · **S/M/L** = rough size · `live` = verified against
a real service, not a stub.

---

## 0. Before you show this to anyone — security and cleanup (M)

These are not features. They are the things that would embarrass you.

**Budget constraint:** live provider verification is limited to the user's free-tier Google AI
Studio, OpenRouter, Groq, and local models. No paid Anthropic/OpenAI calls, and no hosted Stoic Codex
service. Rate-card estimates are not billing records; do not infer that an API request was free
just because its model is listed here.

- [ ] **Revoke the two pasted PATs.** Both were pasted in chat and must be treated as public.
      Keep only PAT #1's successor in `~/.github_pat` (0600).
      Verify with `~/bin/gh-env && curl -s -H "Authorization: Bearer $GITHUB_TOKEN" \
      https://api.github.com/user | head -3`. **S**
- [ ] **Rotate the Gemini key** at the end of any live-testing round
      (`https://aistudio.google.com/apikey`). It lives only at
      `/home/user/.zenithal-keys/gemini` (dir 700, file 600) and is passed by env var — but a
      key you have stopped needing is a key that should not exist. **S**
- [ ] **Confirm nothing secret is in the tree**: `git grep -nE 'AIza|AQ\.|ghp_|sk-'` → no hits.
      Default `cargo test` does not read the key; the 4 `--ignored` live tests and explicit
      `providers models gemini` / live commands do. **S**
- [ ] **Rotate the demo images** only if they embed anything real — they don't (all fixtures are
      synthetic repos). No action needed, listed so it is not re-checked. **S**

---

## 1. Provider layer (the part that is now live)

- [x] **Gemini, live, end to end** — `live`: streamed turn, two full agentic tool loops
      (read → edit → verify), real token counts; the displayed dollar figure is only a
      rate-card estimate, not an invoice. See `providers/LIVE.md`.
- [x] `WireAdapter::capabilities()` on every adapter, with declared-or-inferred capabilities for
      user-defined providers.
- [x] `thoughtSignature` replay, schema allow-list, thinking parts, `StopReason::ToolUse` on tool
      turns, real `functionCall.id`.
- [x] **No paid-provider live calls.** Anthropic/OpenAI adapters remain covered by offline
      cassettes; paid API keys and paid completions are not a requirement or a release gate.
- [x] **`stoic-codex providers models <id> [--free]`** — read-only GET against Gemini, OpenAI-compatible
      (Groq/OpenRouter/Ollama/custom) and Anthropic catalogue endpoints; parses IDs, context,
      reported prices, and only explicit free markers/zero prices. CLI e2e test uses loopback and
      proves this sends a GET, not a completion. Unknown price/free-tier status stays unknown.
- [x] **Google AI Studio catalogue GET** — `stoic-codex providers models gemini` returned 46
      chat-capable IDs with context limits; read-only GET, no prompt/completion. Prices and
      free-tier eligibility are not returned by this endpoint.
- [ ] **OpenRouter/Groq catalogue GETs** — only if their free-tier keys are available:
      `stoic-codex providers models openrouter --free` and `stoic-codex providers models groq`.
      Do not run a paid completion. Groq's list may not expose quota eligibility. **S**
- [ ] **Re-verify the Ollama preset** (`qwen2.5-coder`) against a local daemon, or delete it. **S**
- [ ] **Streaming usage for providers that send it late**: OpenAI needs
      `stream_options.include_usage`; confirm the last frame is not dropped when the socket
      closes immediately after it. **S**
- [ ] **Anthropic prompt caching + cache counters** in `Usage`, if support for this paid provider
      is later prioritized; cassette-only work, no live key required. **M / optional**
- [ ] **A second retry dimension**: 429 handling currently retries the *same* model; a rate limit
      on a strong model should be allowed to fall through to the mid tier
      (`registry.tier`), which is the router's job and is not wired to retries yet. **M**

---

## 2. CLI (S–M each)

- [ ] **Conversation state / `--continue`.** Implementation is now written in
      `cli/src/transcript.rs` and wired into `run`: owner-only local JSON transcripts, canonical
      user/assistant history, automatic session IDs, `--continue` / `--resume <id>` / `--no-session`,
      and oldest-complete-turn eviction from the *request copy* using the calibrated token budget.
      Unit and loopback e2e tests are added, but this remains **unverified**: `cargo`/`rustc` are
      absent in the current environment, so compile/test validation must be rerun when the Rust
      toolchain is available. **L**
- [ ] **`stoic-codex stream` / a transcript renderer** — markdown, tool calls as they happen, diffs
      inline, rather than one line of progress per tool. **M**
- [ ] **Budget guard before sending**: wired into transcript context fitting using provider/model
      `max_context` and `max_output` capabilities; the current task is never trimmed and an
      over-budget request fails before provider I/O. Shares the unverified compile/test status
      above. **S**
- [ ] **`--max-attempts` flag** mirroring `providers.max_attempts`, for scripts that would rather
      fail fast than wait through backoff. **S**
- [ ] **`stoic-codex doctor`** — one command that checks config validity, key presence (never the
      value), daemon reachability, and TLS trust, and exits non-zero on a broken setup. **M**
- [ ] **Exit codes audit** — the documented contract is 0/1/2 (ok/failed/usage). Add a test per
      command that asserts the code, not just the message. **S**

---

## 3. TUI (S–M each)

- [ ] **Feed the board from `MissionEvent`.** `run_mission` already emits
      `Planning / Named / Planned / Deployed / …`; nothing subscribes. Write the app shell:
      a channel, an event→`Mission`/`MissionBoardState` reducer, a tick loop
      (`state.tick(dt)`, `anim.tick(dt)`), and key handling. Then `Preview`/`Mission` in
      `ui-core` stops being fed by fixtures only. **L**
- [x] **Animations** — running operators spin (braille, 10 fps), the newest feed line flashes and
      fades, the hole gained an orbiting hot spot, twinkling stars, a two-harmonic photon ring, a
      glowing inner rim and an outer edge that dissolves. `tui/DESIGN.md` §"The rest of the app
      moves too".
- [ ] **Transcript / diff views driven by real data** — the diff view renders a `Diff` built by
      hand in the snapshot; it should read `git diff` from the selected worktree. **M**
- [ ] **Memory graph from a live TypeDB store**, not a fixture, with the recall path that fed the
      prompt highlighted. **M**
- [ ] **Regenerate previews after any layout change** —
      `cd tui && cargo run -q --example snapshot && cargo run -q --example frames \
      && python3 render_preview.py && python3 render_preview.py --gif`. Previews are part of the
      docs, so a stale one is a wrong claim. The Stoic Codex header, underline, and mission branch
      labels were patched into the cell grids and re-rasterized, but the Rust snapshot example
      could not run here; regenerate from the compiled views when tooling is available. **S**
- [ ] **A 40-column terminal.** `mission-narrow.png` is 84×22; below ~60 columns the assignment
      column collapses to nothing and the code column is the only identifier left. Decide and
      test. **S**
- [ ] **Keybindings doc** — the welcome screen advertises `?`; make the `?` sheet real. **S**

---

## 4. Agent / mission (S–M each)

- [x] Strategos names The Phalanx between plan and deploy; `MissionEvent::Named { renames }`
      emitted; codes flow into `Mission::name_phalanx` / `Agent::rename`.
- [x] Commit message and log format use codes (`[x1-write]`, `…operators of The Phalanx…`).
- [ ] **Retries inside a mission.** `RemoteModel` uses the standard policy, but a mission has N
      operators hitting one rate limit at once; the backoff needs the mission's own jitter seed
      and a cap on total wall clock. **M**
- [ ] **Partial failure policy** — if 2 of 3 operators fail, the mission currently reviews what it
      has. Decide whether that is a success, and say so in the report line and the exit code. **S**
- [ ] **Worktree cleanup** — `.git/stoic-codex-missions/<code>` is never pruned; a tenth mission
      leaves nine stale trees and their branches. **S**
- [ ] **`--parallel` scheduling test** with a slow model: assert the wall clock is closer to
      max(t) than sum(t). **S**
- [ ] **Sandbox review**: confirm what an operator can write outside its worktree, and add a test
      for the answer. The claim "git writes belong to the orchestrator" needs a proof. **M**

---

## 5. Memory (M)

- [ ] **TypeDB parity sweep** — the daemon is not currently running here
      (`/var/tmp/typedb/…/typedb server --server.listen-address 127.0.0.1:1729 …`); start it and
      re-run `cargo test --features typedb` to confirm the 7 live tests still pass against 3.14.1.
      **S**
- [ ] **Vector recall quality** — recall is lexical-ish scoring; measure it against a fixed set of
      queries with expected hits, and record the baseline in `memory/README.md`. Without a
      baseline, "improved recall" is unfalsifiable. **M**
- [ ] **Eviction and growth caps** — `context_budget` is enforced in characters; enforce the
      token budget too (the estimator is calibrated for exactly this). **S**
- [ ] **Migration path** between the JSON store and TypeDB, with a test that round-trips. **M**

---

## 6. Fork integration (the upstream-facing work, M–L)

- [ ] **`cargo check -p codex-tui -p codex-cli`** on a machine that can build the ~2.1M-line
      workspace. `patches/0001-stoic-codex-tui-brand.patch` applies cleanly but **compilation is
      unverified** — it says so in `PATCHES.md`, and that caveat is the whole risk.
- [ ] **Drop `desktop_app`** from the workspace and remove the doctor's signer checks — both ship
      OpenAI's signed binary / assume OpenAI's signatures. Not present in this tree because this
      tree has no upstream crates; they must be deleted in the real fork.
- [ ] **`ci/upstream-sync.yml`** — an upstream remote and a scheduled sync PR, so divergence is a
      reviewed diff instead of a surprise.
- [ ] **Baseline numbers** on the real workspace: cold start, RSS, time-to-first-token, binary
      size. The "fast, low-RAM" claim is currently about *this* tree, not the merged one.
- [ ] **Rerun `brand_urls` guard** against a fresh upstream checkout; it caught 24 hits on
      pristine upstream, and that number will drift.

---

## 7. Docs (S each)

- [x] `providers/LIVE.md` — the live record, both wire bugs, the token calibration table.
- [x] `providers/README.md`, `cli/README.md`, `agent/README.md`, `tui/DESIGN.md`,
      `tui/WORKLOG.md`, root `README.md` refreshed to the current counts.
- [x] `AGENTIC-UPGRADE-PLAN.md` and `GITHUB_TOKEN_SETUP.md` updated to the Stoic Codex name and
      `Hubris-tic/stoic-codex` destination (workspace-level docs outside the repo are not part of
      the source commit).
- [x] `NAMING.md` documents the **Phalanx code names** (`x0-core` … `x7-trace`) and preserves
      upstream identifiers.
- [ ] `INTEGRATION.md` §2 still describes `WireAdapter` as lacking `capabilities()`.
- [ ] A `SECURITY.md`: key handling, what is never logged, how to report a leak.

---

## 8. Release gate (do not tag before all of these)

- [ ] All seven crates: `cargo build --all-targets` 0 warnings, `cargo test` green.
- [ ] `cargo test -- --ignored` green **with a key present** for Gemini; skipped-with-message
      (not failed) without one.
- [ ] One live mission: 3 operators, real repo, real worktrees, one commit, using a provider the
      user confirms is within their free quota (or local); label rate-card cost as an estimate, not
      an invoice. No paid-provider calls.
- [ ] Search for obsolete pre-fork product terms; keep upstream identifiers only where
      `NAMING.md` documents them.
- [ ] Preview PNG/GIF regenerated from the shipped views in the same commit as the layout change.
- [ ] Keys and PATs revoked; the leak checklist in §0 ticked.

---

## 9. Active request — Stoic Codex public rename and sync

The selected public destination is `Hubris-tic/stoic-codex`; push only to the existing branch
`agent/zenithal-workspace-sync-2026-10-26`. Do not push to `main`. Preserve upstream identifiers
and include the pre-existing transcript/context-budget work in the same workspace sync.

- [ ] Review the source rebrand, `.zenithal` config migration, and `ZENITHAL_*` compatibility
      aliases; the code is written but **not compiled or tested** in this environment.
- [ ] Transfer the verified-in-working-tree changes into `/var/tmp/zenithal-push`, scan the full
      diff and secrets, then commit and push to the selected feature branch only.
- [ ] Rename the GitHub repository to `Hubris-tic/stoic-codex`, update the local push remote, and
      verify the branch points to the new commit. No `main` push.
- [ ] Revoke both pasted GitHub PATs at the end, keep only the keeper PAT's rotated successor, and
      rotate the Gemini key; never print or commit any credential value.

## A. Verified in the preceding validation round — with the evidence, so it does not get re-litigated

The suite counts below predate the new local transcript implementation and tests. They are
historical verified results, not validation of the current working-tree changes.

| Item | Evidence |
|---|---|
| Live vendor call, streamed | `gemini-3.1-flash-lite`, `in 207 out 4 tok · 794 ms`; `$0.000058` is the rate-card estimate, not a billing record |
| Live two-turn tool loop, read | 1 tool call, answer, exit 0 |
| Live four-turn tool loop, write | read → edit → read → answer; `M src/auth.rs` correct on disk |
| `thoughtSignature` replay | was `400` without it, `200` with it; pinned by 7 offline tests from live payloads |
| Schema allow-list | was `400 Unknown name "additionalProperties"`; `declaration_schema()` + 2 tests |
| Retries | 12 tests: 429 retried, 400 not, bounded attempts, no retry after a delta, `Retry-After` |
| Tokenizer | 9 tests against 7 live counts, max error 6.1% |
| Animation | spinner + feed flash + hot spot + twinkle + rim + dissolving edge; 7 new tests |
| Suite | **527 passed, 0 failed** (cli 125 · providers 112 · memory 90 · agent 84 · tui 60 · ui-core 39 · brand 17) |
| Live suite | 4 `--ignored` tests: 2 TLS, 2 Gemini — previously green with a key; no Anthropic/OpenAI live tests |
| Model catalogues | Gemini live GET returned 46 models; CLI loopback test verifies `--free` filtering sends GET only |
| Cost display | Terminal labels registry pricing as a rate-card estimate; JSON has `cost_basis`; the figure is not billing data |
