# Stoic Codex — remaining work

Status as of **2026-10-26**. The seven local crates were just rebuilt and tested with stable Rust
1.99.1 under `/var/tmp`: 561 tests passed, 0 failed, and 4 network/live tests were skipped by default.
`cargo build --all-targets` passed for all seven local crates. Targeted rustfmt checks pass for the
files changed in this follow-up; the CLI package's `cargo fmt --check` still reports pre-existing
drift in `paths.rs`, `transcript.rs`, and `tests/config_migration.rs`, which were not reformatted.
The separate upstream-fork integration patch was not included in this local-crate validation.

Legend: `[ ]` open · `[x]` done and verified · **S/M/L** = rough size · `live` = verified against
a real service, not a stub.

---

## 0. Before you show this to anyone — security and cleanup (M)

These are not features. They are the things that would embarrass you.

**Budget constraint:** live provider verification is limited to the user's free-tier Google AI
Studio, OpenRouter, Groq, and local models. No paid Anthropic/OpenAI calls, and no hosted Stoic Codex
service. Rate-card estimates are not billing records; do not infer that an API request was free
just because its model is listed here.

- [ ] **Finish GitHub credential cleanup.** GitHub accepted an unauthenticated revocation request
      for the locally available PAT #1 (HTTP 202); its local copies were deleted. PAT #2 was not
      available locally and still needs revocation in GitHub settings. A later pasted PAT
      authenticated but had no push/admin permission on `Hubris-tic/stoic-codex`; its revocation
      request returned HTTP 422, so revoke it manually in GitHub settings. A subsequent invalid
      candidate returned HTTP 401 and was removed. The current write-capable keeper PAT is installed
      at `~/.github_pat` with mode 0600 and verified for `Hubris-tic/stoic-codex`; never print or
      commit its value. Rotate the Gemini key. **S**
- [ ] **Rotate the Gemini key** at the end of any live-testing round
      (`https://aistudio.google.com/apikey`). It lives only at
      `/home/user/.zenithal-keys/gemini` (dir 700, file 600) and is passed by env var — but a
      key you have stopped needing is a key that should not exist. **S**
- [ ] **Audit staged changes for credentials** without echoing matched values. Use high-confidence
      API/PAT patterns; broad `sk-` searches also hit synthetic upstream test fixtures in this fork
      and must be reviewed as false positives rather than treated as live credentials. Default
      `cargo test` does not read the Gemini key; ignored/live tests do. **S**
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
- [x] **Streaming usage for providers that send it late**: OpenAI needs
      `stream_options.include_usage`; `SseDecoder::finish()` now dispatches the final buffered
      frame at EOF in `run`, `providers test`, and `RemoteModel`. Parser and loopback CLI tests verify
      usage survives a final frame without a blank-line terminator or `[DONE]` event. **S**
- [ ] **Anthropic prompt caching + cache counters** in `Usage`, if support for this paid provider
      is later prioritized; cassette-only work, no live key required. **M / optional**
- [ ] **A second retry dimension**: 429 handling currently retries the *same* model; a rate limit
      on a strong model should be allowed to fall through to the mid tier
      (`registry.tier`), which is the router's job and is not wired to retries yet. **M**

---

## 2. CLI (S–M each)

- [x] **Conversation state / `--continue`.** `cli/src/transcript.rs` and `run` provide owner-only
      local JSON transcripts, canonical user/assistant history, automatic session IDs,
      `--continue` / `--resume <id>` / `--no-session`, and oldest-complete-turn eviction from the
      *request copy* using the calibrated token budget. CLI unit and loopback tests pass in the
      2026-10-26 seven-crate validation run. **L**
- [ ] **`stoic-codex stream` / a transcript renderer** — markdown, tool calls as they happen, diffs
      inline, rather than one line of progress per tool. **M**
- [x] **Budget guard before sending**: transcript context fitting uses provider/model
      `max_context` and `max_output` capabilities; the current task is never trimmed and an
      over-budget request fails before provider I/O. Covered by CLI unit and loopback tests in the
      2026-10-26 validation run. **S**
- [x] **`--max-attempts` override** mirroring validated `providers.max_attempts` (1–10), for
      scripts that would rather fail fast than wait through backoff. It reaches `run`, agent,
      mission planning, and provider model-list/test calls; config validation, unit tests, and
      loopback retry tests for every command family pass. Help and `cli/README.md` updated. **S**
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
- [x] **Regenerate previews after any layout change** —
      `cd tui && cargo run --example snapshot && cargo run --example frames && python3 render_preview.py
      && python3 render_preview.py --gif`. This round regenerated 12 cell-grid snapshots and 12
      PNGs, plus 48 animation-frame JSON files and `hole.gif`; GIF dimensions/frame count and
      sampled-frame differences were verified. All static wide, narrow, and 40-column previews
      were visually inspected. Repeat after future layout edits. **S**
- [x] **40-column terminal and preview.** A unified diff replaces the cramped side-by-side view
      below 72 columns; 40-column welcome, mission, help, and diff snapshots are regenerated and
      visually inspected. The contextual help sheet fills the narrow width to avoid leaking a
      strip of the underlying diff at its edges; a regression test covers that case. **S**
- [ ] **Wire the contextual key guide.** The welcome and per-view help-sheet renderers are now
      present, and footers preserve a compact `? help` affordance. The app shell still needs to
      dispatch `?`/Escape to show/dismiss the overlay. **S**

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

- [x] All seven local crates: `cargo build --all-targets` passed with no warnings; `cargo test`
      passed 561 tests, 0 failed (4 live/network tests skipped by default). The separate upstream
      integration patch was not part of this local-crate run.
- [ ] `cargo test -- --ignored` green **with a key present** for Gemini; skipped-with-message
      (not failed) without one.
- [ ] One live mission: 3 operators, real repo, real worktrees, one commit, using a provider the
      user confirms is within their free quota (or local); label rate-card cost as an estimate, not
      an invoice. No paid-provider calls.
- [ ] Search for obsolete pre-fork product terms; keep upstream identifiers only where
      `NAMING.md` documents them.
- [x] Static wide/narrow/40-column PNG previews regenerated from shipped views and committed with
      the layout update. `hole.gif` was regenerated from 48 fresh frame dumps and verified (48
      frames, 788×428, 120 ms/frame); its animation source was unchanged. All static screens were
      inspected.
- [ ] Keys and PATs revoked; the leak checklist in §0 ticked.

---

## 9. Active request — Stoic Codex branch cleanup and sync

The public destination is `Hubris-tic/stoic-codex`; retain both `main` and the existing feature
branch `agent/zenithal-workspace-sync-2026-10-26`. The user has now authorized a non-forced
fast-forward promotion of all validated feature-branch commits to `main`; that promotion is
complete. Preserve upstream identifiers and include the pre-existing transcript/context-budget
work in the workspace sync.

- [x] Static review of the source rebrand, `.zenithal` config migration, and `ZENITHAL_*`
      compatibility aliases; historical TypeDB database ID preserved.
- [x] Compile and test the seven local crates with stable Rust 1.99.1: `cargo build --all-targets`
      passed for each; `cargo test` passed 561 tests, 0 failed, with 4 live/network tests skipped
      by default. Targeted rustfmt checks passed on this follow-up's Rust files. The CLI package's
      `cargo fmt --check` still flags pre-existing drift in `paths.rs`, `transcript.rs`, and
      `tests/config_migration.rs`; these were not reformatted. **Unverified:** the separate
      upstream-fork integration patch was not compiled in this run.
- [x] Earlier sync: transferred the workspace, scanned the staged diff and secrets, and pushed
      implementation commit `5a7bb0d17e31c17f8b6636675876cc990c10af26` plus checklist follow-up
      `a7a58378f9ce7a8f89419acd6ff9a13c0358d7f3` to the selected feature branch. At that stage,
      there was no `main` push.
- [x] Renamed the GitHub repository to `Hubris-tic/stoic-codex` and updated the remote. At the
      time of that earlier checklist follow-up, the feature branch was at
      `a7a58378f9ce7a8f89419acd6ff9a13c0358d7f3` and `main` was at
      `970111f6cb62cc02fe36a82ea7d71b37a2889c97`.
- [x] **Delete all other remote branches.** User confirmed keeping only `main` and
      `agent/zenithal-workspace-sync-2026-10-26`. The public ref listing found 4,709 branches;
      4,707 were deleted. A fresh `git ls-remote --heads` at the time verified exactly the two
      requested branches remained; the later authorized promotion changed `main`'s commit only.
- [x] **TUI cosmetic follow-up (requested priority):** welcome screen, contextual help-sheet views,
      compact `? help` footer, and 40-column unified diff; 69 TUI tests pass and all previews were
      regenerated and inspected. Pushed only to the feature branch; app-shell `?`/Escape dispatch
      remains open.
- [x] **Non-TUI follow-up:** corrected stale identity guards; added the validated 1–10 retry-count
      override across run, agent, mission, and provider requests; flushes EOF-terminated SSE usage;
      updated docs and regression tests. All seven local crates pass (561 tests); implementation
      commit `a3e2b7f375b8fb185708ac44ac9e535709f885e0` and validation checklist commit
      `8edeba9bee576ee6256cb9adbc9f1f1d8b85dc18` were pushed to the feature branch.
- [x] **Authorized promotion to `main`:** fast-forwarded `main` from
      `970111f6cb62cc02fe36a82ea7d71b37a2889c97` to feature commit
      `8edeba9bee576ee6256cb9adbc9f1f1d8b85dc18`; no force push or merge commit. This checklist
      update is being kept on both `main` and the feature branch.
- [ ] Finish credential cleanup: PAT #1's local copy was revoked (HTTP 202) and removed; PAT #2
      was unavailable locally and still needs manual revocation. The later pasted PAT's revocation
      request returned HTTP 422, so it must also be revoked in GitHub settings; its local copies
      were removed. The invalid candidate's temporary file was removed. The current write-capable
      keeper PAT is installed at `~/.github_pat` (0600) for `Hubris-tic/stoic-codex`. Rotate the
      Gemini key. Credential values must never be printed or committed; see §0.

## A. Verified in the preceding validation round — with the evidence, so it does not get re-litigated

The 2026-10-26 verification run covered the seven local crates. It excludes ignored live/network
tests by default and does not compile the separate upstream-fork patch.

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
| Local suite | **561 passed, 0 failed** (cli 149 · providers 113 · memory 90 · agent 84 · tui 69 · ui-core 39 · brand 17); 4 live/network tests skipped by default |
| All-target build | `cargo build --all-targets` passed for all seven local crate manifests; no warnings |
| Retry override | Validated 1–10; CLI loopback tests cover run, agent, mission plan, provider model-list, and provider test after 429 |
| EOF usage frame | Provider parser test and CLI stream integration pass with final usage event missing its blank-line delimiter and `[DONE]` |
| Formatting | Targeted rustfmt check passes for changed files; CLI `cargo fmt --check` only flags pre-existing drift in `paths.rs`, `transcript.rs`, and `tests/config_migration.rs` |
| Live suite | 4 `--ignored` tests: 2 TLS, 2 Gemini — historically green with a key but skipped in this round; no Anthropic/OpenAI live tests |
| Model catalogues | Gemini live GET returned 46 models; CLI loopback test verifies `--free` filtering sends GET only |
| Cost display | Terminal labels registry pricing as a rate-card estimate; JSON has `cost_basis`; the figure is not billing data |
