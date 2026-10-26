# App shell — 2026-10-26

The Ratatui crate now includes an in-process terminal app shell in `src/app.rs` (this supersedes
the earlier cosmetic-pass note that key dispatch was still missing).

- `ChatTui` paints streamed `run` response deltas through a `Write` sink, gives the transcript the
  available height, pins follow-up input above a segmented powerline-style footer, handles paste,
  scroll, help and exit keys, and keeps status/accounting separate from assistant text. The visible
  history is capped at the newest 200 messages; the CLI transcript still follows the chosen session
  settings.
- `MissionTui` owns a 40 ms terminal loop, advances `MissionBoardState::tick(dt)`, receives owned
  `MissionEvent`s over a channel, and dispatches selection, detail, help and quit keys. It renders
  the existing borderless board and contextual help sheet rather than a second visual system.
- `reduce_mission_event` maps planning, naming, deployment (including the actual worktree path),
  tool activity, verification, returns, review verdicts, apply/conflict, and completion into the
  UI-core `Mission`/`MissionBoardState`. A compact final snapshot reconciles early returns that did
  not emit a `Returned` event. Closing the view does not cancel the mission worker.
- The live mission board reports mission cost as unknown until it has an estimate; it does not
  mistake missing pricing data for `$0.00`. Run accounting continues to display the CLI's explicitly
  labelled rate-card estimate.
- The terminal guard restores raw mode, bracketed paste, cursor visibility and the alternate screen
  on normal exit or unwinding. No GPU dependency was introduced. The CLI disables the optional
  `blackhole` feature for its TUI dependency, so the physics tracer is not linked into the CLI binary.
- Regression coverage uses Ratatui's in-memory `TestBackend`; no provider calls or real terminal are
  used. The standalone crate test suite passes **82 tests**.

The CLI starts chat automatically only for an interactive, non-JSON, non-dry-run `run`; pass
`--no-tui` for the one-shot interface. Interactive `mission run` uses the event-driven board;
`--json`, `--quiet`, redirected output, or `--no-tui` retain the text/script behavior.
