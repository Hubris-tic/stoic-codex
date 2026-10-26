# `stoic-codex-agent` — agents with tools, and the mission orchestrator

The library that actually runs things: a tool loop, a sandbox, git worktrees, and the
multi-agent orchestrator the rest of Stoic Codex is built around.

Two layers, and they are deliberately separate:

| Layer | What it is | Entry point |
|---|---|---|
| **Single agent** | one model, one tool registry, one loop, one repository | `run_agent` |
| **Mission** | one boss (**Strategos**) plans and names **The Phalanx**, N operators (`xN-role` codes) execute in isolated worktrees, the boss reviews, accepted work lands | `run_mission` |

## The mission

There is **no integrator**, no voting, no per-file merge strategy and no scoring
committee. One agent decides, three agents work, one agent judges.

```
                              ┌───────────────────────────┐
      task ──────────────────▶│        STRATEGOS          │
                              │  boss · no tools          │
                              │  sees: task, repo layout, │
                              │        every report       │
                              └───┬───────────┬───────┬───┘
            assigns each a different job and a code, on the same repo
                ┌─────────────────┼───────────────┼─────────────────┐
                ▼                 ▼               ▼
     ┌───────────────┐  ┌───────────────┐  ┌───────────────┐
     │   x1-write    │  │   x2-check    │  │   x3-ship     │   each in its own
     │ own worktree  │  │ own worktree  │  │ own worktree  │◀── git worktree,
     │ own sandbox   │  │ own sandbox   │  │ own sandbox   │    own branch,
     │ own tools     │  │ own tools     │  │ own tools     │    own model client
     └───────┬───────┘  └───────┬───────┘  └───────┬───────┘
             │ commit on        │                  │
             │ stoic-codex/<code>  │                  │
             └─────────────────┼──────────────────┘
                               ▼
                      ┌─────────────────┐
                      │    STRATEGOS    │  accept / reject, per operator
                      └────────┬────────┘
                               ▼
              accepted files copied into the working tree
              (whole file, first accepted writer wins)
                               ▼
                   one commit: `stoic-codex mission: N file(s) …`
```

### The rules that make it safe

- **Strategos has no tools.** Its entire job is the plan and the review. It cannot edit a
  file, so it cannot break the repository while thinking about it.
- **CLI mission retries share one bounded window.** The CLI gives Strategos and every Operator
  independent equal-jitter streams over a shared `providers.mission_retry_window_secs` deadline
  (30 seconds by default, starting at the first retryable provider error). The cap covers retry
  waits and subsequent live retry requests; the provider-agnostic `run_mission` library remains
  unchanged. Standalone commands keep their generic retry behavior.
- **An operator is confined to its worktree.** Its own `Sandbox`, its own `ToolRegistry`,
  its own model client (a `Model` that carries connection state must not be shared across
  threads). The sandbox rejects absolute paths, `..`, symlink escapes (including dangling
  external links) and `.git` writes.
- **Nothing is merged.** Accepted work is copied file-by-file with `Worktree::copy_file_to`.
  If two operators changed one file, the first accepted one wins and the other is reported
  as a `Conflict` — never silently blended.
- **Rejected or partly applied work is not lost.** It stays committed on `stoic-codex/<code>`;
  a branch is deleted only when all of its changed files landed (or it had no changes).
- **Provider failures preserve partial edits.** If an Operator wrote files before its model failed,
  the orchestrator stages and commits that untrusted work to the recovery branch but never applies
  it. If that recovery commit fails, the dirty worktree is kept and cleanup reports a warning.
- **Git writes belong to the orchestrator.** No model-callable tool can commit, check out,
  or touch `.git`.
- **Partial outcomes are explicit.** `MissionReport::outcome()` distinguishes `Complete`,
  `PartialSuccess`, and `Failed`; partial work remains a successful exit when at least one
  applicable file landed, while the CLI summary and JSON identify the partial outcome.

### It degrades instead of failing

Being wrong about a plan should cost quality, not the whole run:

| Situation | What happens |
|---|---|
| Strategos' plan is not machine-readable | `PlanSource::Fallback` — every operator is given the whole task |
| The review call fails | every clean result is accepted, reason `accepted unreviewed: …` |
| The review never mentions an operator | its clean work is kept, flagged `not mentioned by the review` |
| An operator crashes or loops out | only that member fails; the others finish and are judged |
| `--verify` fails for a member | that member is not applicable; the rest still are |

`MissionError` is returned only when the *plan* call itself fails.

### The naming

An operator is not called `agent 2`. It starts as a bare slot (`slot-1`, `slot-2`, …) because
nothing knows yet what the work is, and Strategos names it from a fixed catalogue when the plan
lands:

| Code | Role |
|---|---|
| `x0-core` | the base context harvester |
| `x1-write` | the primary logic generator |
| `x2-check` | the testing and validation engine |
| `x3-ship` | the deployment and optimization pipeline |
| `x4-scan` | the search and retrieval array |
| `x5-patch` | the surgical edit unit |
| `x6-audit` | the review and hardening layer |
| `x7-trace` | the diagnostics and debugging rig |

The catalogue is `brand/src/phalanx.rs` — data, not strings scattered through the orchestrator,
so `stoic-codex mission plan` and the TUI can print the same sentence the log does. The naming
rules:

- Strategos may name an operator itself (`"code": "x1-write"` in the plan JSON). If the code
  exists in the catalogue and is not already taken, that name wins.
- Otherwise `assign()` reads the operator's assignment and picks the best-scoring free code.
  Ties go to catalogue order; no evidence at all means `x1-write`; a full catalogue falls back
  to `classify()`, then to a numbered variant.
- The decision is emitted as `MissionEvent::Named { renames }`, so every consumer — the CLI log,
  the mission board, a future app shell — learns the codes from one event instead of guessing.
- Codes are slugs (`x1-write` survives `slug()`), because the branch and the worktree directory
  are named after them: `stoic-codex/x1-write`, `.git/stoic-codex-missions/x1-write`.

## Using it

```rust
use stoic_codex_agent::{run_mission, AgentConfig, Operator, MissionConfig, Repo, VerifyCommand};

let repo = Repo::discover(".")?;
let strategos = my_model("boss");
// `squad` hands out slots (`slot-1`, `slot-2`, …) and one client each; `run_mission` lets
// Strategos rename them to codes before anything is deployed.
let operators = Operator::squad(3, |codename| my_model(codename));
let cfg = MissionConfig {
    agent: AgentConfig::new("gpt-5-codex"),
    verify: Some(VerifyCommand::parse("cargo test")),
    ..MissionConfig::default()
};
let report = run_mission(&repo, &strategos, operators, "split the retry work", &cfg, &|e| {
    eprintln!("{e:?}");
})?;
println!("{}", report.table());
```

From the CLI:

```bash
stoic-codex mission plan "add per-file retry budgets"     # one call: show the assignments
stoic-codex mission run  "add per-file retry budgets"     # full mission, 3 operators by default
stoic-codex mission run  "…" --agents 5 --verify "cargo test" --parallel 3
stoic-codex mission run  "…" --no-review --no-commit      # look before you leap
```

## Layout

| File | What lives there |
|---|---|
| `src/loop_.rs` | the single-agent loop: `Model` trait, `Event`, `StopCause`, `run_agent` |
| `src/tool.rs` | `Tool` trait, `ToolRegistry` (`readonly` / `editing` / `with_shell`) |
| `src/tools/` | `read_file`, `write_file`, `edit_file`, `list_dir`, `search`, `run_command`, `git_status`, `git_diff` |
| `src/sandbox.rs` | path confinement for every tool |
| `src/worktree.rs` | `Repo`, `Worktree`, branches, patches, `copy_file_to` |
| `src/strategos.rs` | **the orchestrator**: plan → deploy → execute → review → apply → commit |
| `tests/mission.rs` | 16 end-to-end tests: real git repo, real worktrees, scripted models |

## Tests

```bash
cd stoic-codex/agent && cargo test
```

| Suite | Tests |
|---|---|
| unit (`src/*.rs`) | 75 |
| `tests/mission.rs` (real git, real worktrees, real threads) | 16 |
| **total** | **91**, 0 warnings |

A verbatim run of the whole thing, including the repository state afterwards, is in
`MISSION-DEMO.md` — reproducible with `cli/dev/stub_mission.py`, no key required.

The end-to-end tests fake exactly one thing: the model. Everything else is real — `git init`,
`git worktree add`, the sandbox, the tool loop, the apply step and the commit.

## What is not here yet

- **No real model call in the tests.** Deliberate: the model layer is proven to the socket
  with scripted models, and the CLI's `RemoteModel` is what plugs a vendor in.
- **The TUI board is a view, not a driver.** `stoic-codex/ui-core::mission` models exactly this
  orchestrator (Strategos pinned on top, the codes below) so the events map 1:1, but the
  terminal app that consumes them is a later milestone.
