//! # zenithal-agent
//!
//! The half of Zenithal that acts: a tool-calling loop, the tools, and a squad of agents
//! commanded by one of their own — **Strategos** plans and reviews, the **Operator** execute
//! in isolated git worktrees, and nothing reaches your branch unreviewed.
//!
//! ```text
//!   task ──► AgentConfig ──► run_agent ──► Model (any provider)
//!                                │  ▲
//!                        ToolCall│  │ToolResult
//!                                ▼  │
//!                            ToolRegistry (sandboxed to one directory)
//! ```
//!
//! And with more than one agent:
//!
//! ```text
//!                       user task
//!                           │
//!                     ┌─────▼──────┐
//!                     │  STRATEGOS │  writes the plan
//!                     └─────┬──────┘
//!          ┌────────────────┼────────────────┐
//!    x1-write     x2-check     x3-ship      one worktree each
//!    (worktree)   (worktree)   (worktree)   different work, same repo
//!          └────────────────┼────────────────┘
//!                     ┌─────▼──────┐
//!                     │  STRATEGOS │  accepts or rejects each report
//!                     └─────┬──────┘
//!                           ▼
//!              accepted files land in your working tree
//! ```
//!
//! Two deliberate absences:
//!
//! * **No vendor code.** The loop speaks `zenithal_providers`' canonical IR; every wire
//!   difference is in an adapter.
//! * **No hidden I/O policy.** Filesystem access goes through `Sandbox`, the shell is
//!   opt-in with an allow list, and git writes belong to `worktree.rs` — the orchestrator —
//!   never to a tool the model can call.

pub mod loop_;
pub mod sandbox;
pub mod strategos;
pub mod tool;
pub mod tools;
pub mod worktree;

pub use loop_::{
    run_agent, AgentConfig, AgentError, AgentOutcome, Event, Model, ModelError, StopCause,
};
pub use sandbox::{Sandbox, SandboxError};
pub use strategos::{
    code_line, name_codes, name_phalanx, provisional_name, run_mission, Assignment, Code,
    MissionConfig, MissionError, MissionEvent, MissionReport, Operator, OperatorResult,
    OperatorState, Plan, PlanSource, Verdict, VerifyCommand, CODES, PHALANX, STRATEGOS,
};
pub use tool::{Tool, ToolOutcome, ToolRegistry};
pub use worktree::{FileChange, GitError, Repo, Worktree};

/// The module is called `loop`, which is a keyword; this alias keeps the import readable
/// (`zenithal_agent::r#loop` vs `zenithal_agent::loop_`) without renaming the concept.
pub use loop_ as agent_loop;
