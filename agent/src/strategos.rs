//! Strategos and the Phalanx.
//!
//! One agent commands; the rest execute, under short-hand codes. There is no vote and no
//! scoring committee:
//!
//! ```text
//!                    user task
//!                        │
//!                  ┌─────▼──────┐
//!                  │  STRATEGOS │  plans, and names the Phalanx
//!                  └─────┬──────┘
//!        ┌───────────────┼───────────────┐
//!   ┌────▼─────┐   ┌─────▼────┐   ┌─────▼────┐
//!   │ x1-write │   │ x2-check │   │ x3-ship  │  one worktree each, different work
//!   └────┬─────┘   └─────┬────┘   └─────┬────┘
//!        └───────────────┼──────────────┘
//!                  ┌─────▼──────┐
//!                  │  STRATEGOS │  reviews what came back, accepts or rejects
//!                  └─────┬──────┘
//!                        ▼
//!              accepted files land in your working tree
//! ```
//!
//! Every executor starts life as a bare slot (`slot-1`, `slot-2`, …) because nothing knows
//! yet what the work is. Strategos names each one from the code catalogue in
//! `stoic_codex_brand::phalanx` — `x0-core` harvests context, `x1-write` writes the logic,
//! `x2-check` validates, `x3-ship` deploys, `x4-scan` retrieves, `x5-patch` edits, `x6-audit`
//! reviews, `x7-trace` diagnoses — so a log line says what an agent *is*, not just that it
//! exists. The collective is **The Phalanx**.
//!
//! The division of labour matters:
//!
//! * **Strategos decides.** It is the only agent that sees the whole mission: the task, the
//!   layout of the repository, and every operator's report. It writes the assignments and
//!   picks the codes, so two operators are given different files to own rather than racing
//!   on the same ones.
//! * **The Phalanx is confined.** Each operator runs in its own git worktree with its own
//!   sandbox, so they cannot see each other's edits or touch the user's tree. Their cwd *is*
//!   their worktree; no path can reach past it.
//! * **Nothing reaches your branch un-reviewed.** After the parallel phase, Strategos reads
//!   each report and says what to keep. A rejected operator's work stays on its branch.
//!
//! Collisions are still possible (two operators editing one file because the plan was bad).
//! They are handled the boring way: the first accepted change owns the file, the second is
//! reported as a conflict and left on its branch. No merge, no conflict markers, no silent
//! overwrite — a file is either taken whole or not at all.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use stoic_codex_providers::{CanonicalRequest, Content, Message, Role, Usage};

use crate::loop_::{run_agent, AgentConfig, AgentError, Event, Model, ModelError, StopCause};
use crate::tool::ToolRegistry;
use crate::worktree::{FileChange, GitError, Repo, Worktree};

// ─── names ─────────────────────────────────────────────────────────────────────

// The cast lives in `stoic-codex-brand::phalanx` so the orchestrator, the CLI and the TUI
// cannot spell a code three ways.
pub use stoic_codex_brand::phalanx::{Code, CODES, PHALANX, STRATEGOS};

/// What an operator is called before Strategos has named it: a slot, not a job.
///
/// Codes come from the *assignment* — `x2-check` means "this one is testing" — and the
/// assignments do not exist until the plan does. Until then an operator is `slot-1`,
/// `slot-2`, … and the moment the plan lands it is renamed.
pub fn provisional_name(index: usize) -> String {
    format!("slot-{index}")
}

/// The line a log gets when a code is deployed: `x1-write · the primary logic generator`.
pub fn code_line(code: &str) -> String {
    match stoic_codex_brand::phalanx::lookup(code) {
        Some(found) => format!("{} · {}", found.id, found.role),
        None => code.to_string(),
    }
}

// ─── planning ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// The operator this assignment is for — its short-hand system code once the mission is
    /// named (`x1-write`), its slot before that (`slot-1`).
    pub codename: String,
    /// The code Strategos named this operator, if it named one. Checked against the
    /// catalogue and against the other operators; the assignment text decides otherwise.
    pub code: Option<String>,
    pub task: String,
}

impl Assignment {
    pub fn new(codename: impl Into<String>, task: impl Into<String>) -> Self {
        Assignment {
            codename: codename.into(),
            code: None,
            task: task.into(),
        }
    }

    pub fn with_code(
        codename: impl Into<String>,
        code: impl Into<String>,
        task: impl Into<String>,
    ) -> Self {
        Assignment {
            codename: codename.into(),
            code: Some(code.into()),
            task: task.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanSource {
    /// Strategos answered with a parseable plan.
    Strategos,
    /// The plan was unreadable, so every operator was given the whole task and told to keep
    /// its change narrow. The mission still runs: a bad plan should cost quality, not the
    /// run.
    Fallback,
}

impl PlanSource {
    pub fn label(self) -> &'static str {
        match self {
            PlanSource::Strategos => "strategos",
            PlanSource::Fallback => "fallback",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub assignments: Vec<Assignment>,
    pub source: PlanSource,
    /// What Strategos actually said. Kept so a bad plan is diagnosable.
    pub raw: String,
}

impl Plan {
    pub fn task_for(&self, codename: &str) -> Option<&str> {
        self.assignments
            .iter()
            .find(|a| a.codename == codename)
            .map(|a| a.task.as_str())
    }

    /// The code Strategos proposed for this operator, if any.
    pub fn code_for(&self, codename: &str) -> Option<&str> {
        self.assignments
            .iter()
            .find(|a| a.codename == codename)
            .and_then(|a| a.code.as_deref())
    }
}

/// Name the team: give every operator the short-hand system code that matches its job.
///
/// Strategos names its own operators when it can — its plan may carry a `code` per
/// assignment — and its answer is trusted only if it is a real code from the catalogue that
/// nobody else has taken. When it stays quiet (or invents `x9-whatever`), the assignment
/// text decides, with a preference for jobs nobody is doing: three operators get three
/// different codes wherever the words allow it.
///
/// Returns the renames, in operator order, so the observer can log
/// `[strategos] deployed x1-write`.
pub fn name_phalanx(operators: &mut [Operator], plan: &mut Plan) -> Vec<(String, String)> {
    let slots: Vec<String> = operators.iter().map(|o| o.codename.clone()).collect();
    let renames = name_codes(&slots, plan);
    for (operator, (_, code)) in operators.iter_mut().zip(renames.iter()) {
        operator.codename = code.clone();
    }
    renames
}

/// [`name_phalanx`] without the operators: same decision, for callers that only hold slots
/// (the CLI's `mission plan` previews a plan before any agent exists).
pub fn name_codes(slots: &[String], plan: &mut Plan) -> Vec<(String, String)> {
    let tasks: Vec<String> = slots
        .iter()
        .map(|slot| plan.task_for(slot).unwrap_or_default().to_string())
        .collect();
    let borrowed: Vec<&str> = tasks.iter().map(String::as_str).collect();
    let mut codes = stoic_codex_brand::phalanx::assign(&borrowed);

    // Strategos' own names win where they are real and not already spoken for.
    let mut used: Vec<String> = Vec::new();
    for (i, slot) in slots.iter().enumerate() {
        let proposed = plan
            .code_for(slot)
            .and_then(stoic_codex_brand::phalanx::lookup)
            .map(|c| c.id);
        if let Some(id) = proposed {
            if !used.iter().any(|u| u == id) {
                codes[i] = id.to_string();
                used.push(id.to_string());
            }
        }
    }
    // Belt and braces: whatever route a code took, two operators never share one.
    let mut seen: Vec<String> = Vec::new();
    for code in codes.iter_mut() {
        let mut candidate = code.clone();
        let mut n = 1;
        while seen.iter().any(|s| s == &candidate) {
            n += 1;
            candidate = format!("{code}-{n}");
        }
        *code = candidate.clone();
        seen.push(candidate);
    }

    let mut renames = Vec::with_capacity(slots.len());
    for (i, slot) in slots.iter().enumerate() {
        renames.push((slot.clone(), codes[i].clone()));
    }
    for assignment in plan.assignments.iter_mut() {
        if let Some((_, new)) = renames.iter().find(|(old, _)| old == &assignment.codename) {
            assignment.codename = new.clone();
        }
    }
    renames
}

// ─── results ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorState {
    Succeeded,
    /// Ran, but changed no files.
    Empty,
    Failed,
}

impl OperatorState {
    pub fn label(self) -> &'static str {
        match self {
            OperatorState::Succeeded => "done",
            OperatorState::Empty => "no changes",
            OperatorState::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Accepted,
    Rejected,
    /// Strategos never reviewed it (no review, or it was not mentioned).
    Unreviewed,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Accepted => "accepted",
            Verdict::Rejected => "rejected",
            Verdict::Unreviewed => "unreviewed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OperatorResult {
    pub codename: String,
    pub task: String,
    pub state: OperatorState,
    pub text: String,
    pub files: Vec<FileChange>,
    pub patch: String,
    pub error: Option<String>,
    pub steps: usize,
    pub tool_calls: usize,
    pub usage: Usage,
    pub elapsed_ms: u64,
    pub stop: Option<StopCause>,
    pub verified: Option<bool>,
    pub verify_output: String,
    pub commit: Option<String>,
    pub verdict: Verdict,
    pub reason: String,
    /// Files that did reach the working tree. Empty when rejected or conflicting.
    pub applied: Vec<String>,
}

impl OperatorResult {
    fn from_run(codename: &str, task: &str) -> OperatorResult {
        OperatorResult {
            codename: codename.to_string(),
            task: task.to_string(),
            state: OperatorState::Empty,
            text: String::new(),
            files: Vec::new(),
            patch: String::new(),
            error: None,
            steps: 0,
            tool_calls: 0,
            usage: Usage::default(),
            elapsed_ms: 0,
            stop: None,
            verified: None,
            verify_output: String::new(),
            commit: None,
            verdict: Verdict::Unreviewed,
            reason: String::new(),
            applied: Vec::new(),
        }
    }

    /// Whether this result's work is a candidate for the user's tree at all.
    pub fn usable(&self) -> bool {
        self.state == OperatorState::Succeeded
            && !self.files.is_empty()
            && self.verified != Some(false)
    }
}

// ─── spec + config ────────────────────────────────────────────────────────────

pub struct Operator {
    pub codename: String,
    pub model: Box<dyn Model>,
}

impl std::fmt::Debug for Operator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Operator")
            .field("codename", &self.codename)
            .finish()
    }
}

impl Operator {
    pub fn new(codename: impl Into<String>, model: Box<dyn Model>) -> Self {
        Operator {
            codename: codename.into(),
            model,
        }
    }

    /// `n` operators, each starting as `slot-1..n` with its own model instance from the
    /// factory (a model that carries connection state must not be shared across threads).
    /// Strategos renames them to codes during the mission; the returned set is only the
    /// starting line-up.
    pub fn squad<F>(n: usize, mut build: F) -> Vec<Operator>
    where
        F: FnMut(&str) -> Box<dyn Model>,
    {
        (1..=n)
            .map(|i| {
                let name = provisional_name(i);
                let model = build(&name);
                Operator {
                    codename: name,
                    model,
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct MissionConfig {
    pub agent: AgentConfig,
    pub base: Option<String>,
    /// Optional command run in each worktree before Strategos reviews it.
    pub verify: Option<VerifyCommand>,
    pub commit: bool,
    pub keep_worktrees: bool,
    pub keep_branches: bool,
    pub worktree_dir: Option<PathBuf>,
    /// Ask Strategos to review. Off means "apply everything that ran cleanly".
    pub review: bool,
    /// Total Operator allowed to be running at once. Worktrees are cheap; provider rate
    /// limits are not.
    pub max_parallel: usize,
}

impl Default for MissionConfig {
    fn default() -> Self {
        MissionConfig {
            agent: AgentConfig::default(),
            base: None,
            verify: None,
            commit: true,
            keep_worktrees: false,
            keep_branches: false,
            worktree_dir: None,
            review: true,
            max_parallel: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyCommand {
    pub program: String,
    pub args: Vec<String>,
    pub timeout: Duration,
}

impl VerifyCommand {
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        VerifyCommand {
            program: program.into(),
            args,
            timeout: Duration::from_secs(300),
        }
    }

    pub fn parse(argv: &str) -> Option<VerifyCommand> {
        let mut parts = argv.split_whitespace();
        let program = parts.next()?.to_string();
        Some(VerifyCommand::new(
            program,
            parts.map(|s| s.to_string()).collect(),
        ))
    }
}

// ─── events and report ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum MissionEvent {
    Planning {
        task: String,
    },
    /// Strategos named its team: `(slot, code)` in operator order.
    Named {
        renames: Vec<(String, String)>,
    },
    Planned {
        plan: Plan,
    },
    Deployed {
        codename: String,
        branch: String,
    },
    Agent {
        codename: String,
        event: Event,
    },
    Returned {
        codename: String,
        state: OperatorState,
    },
    Verified {
        codename: String,
        ok: bool,
    },
    Reviewing {
        reports: usize,
    },
    Reviewed {
        plan: Plan,
        verdicts: Vec<(String, Verdict, String)>,
    },
    Applying {
        codename: String,
        file: String,
    },
    Conflict {
        file: String,
        owner: String,
        other: String,
    },
    Done {
        applied: usize,
        conflicts: usize,
    },
}

#[derive(Debug, Clone)]
pub struct MissionReport {
    pub task: String,
    pub plan: Plan,
    pub operators: Vec<OperatorResult>,
    pub commit: Option<String>,
    pub base: String,
    pub parallel_ms: u64,
    pub review_note: String,
    /// True when Strategos could not complete its requested review.
    pub review_failed: bool,
    /// A mission commit failure leaves the applied files in the user's working tree.
    pub commit_error: Option<String>,
    /// Cleanup problems are visible instead of being silently discarded.
    pub cleanup_warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissionOutcome {
    Complete,
    PartialSuccess,
    Failed,
}

impl MissionOutcome {
    /// Human-readable outcome for the terminal summary.
    pub fn label(self) -> &'static str {
        match self {
            MissionOutcome::Complete => "complete",
            MissionOutcome::PartialSuccess => "partial success",
            MissionOutcome::Failed => "failed",
        }
    }

    /// Stable machine-readable outcome for JSON reports.
    pub fn code(self) -> &'static str {
        match self {
            MissionOutcome::Complete => "complete",
            MissionOutcome::PartialSuccess => "partial_success",
            MissionOutcome::Failed => "failed",
        }
    }
}

impl MissionReport {
    pub fn applied_count(&self) -> usize {
        self.operators.iter().map(|f| f.applied.len()).sum()
    }

    /// Any landed work keeps the exit code successful, but the report makes incomplete work
    /// explicit so humans and JSON consumers can distinguish it from a complete mission.
    pub fn outcome(&self) -> MissionOutcome {
        if self.applied_count() == 0 {
            return MissionOutcome::Failed;
        }
        let incomplete = self.review_failed
            || self.commit_error.is_some()
            || !self.cleanup_warnings.is_empty()
            || self.operators.iter().any(|result| {
                result.state != OperatorState::Succeeded
                    || result.verdict == Verdict::Rejected
                    || result.applied.len() < result.files.len()
            });
        if incomplete {
            MissionOutcome::PartialSuccess
        } else {
            MissionOutcome::Complete
        }
    }

    pub fn conflicts(&self) -> Vec<(String, String, String)> {
        // Derived, not stored: a conflict is a file two accepted results both changed, and
        // the second one's `applied` is empty for it. Recomputed so the report cannot
        // disagree with itself.
        let mut seen: Vec<(String, String)> = Vec::new();
        let mut out = Vec::new();
        for f in &self.operators {
            if f.verdict != Verdict::Accepted {
                continue;
            }
            for file in &f.files {
                if let Some((owner, _)) = seen.iter().find(|(p, _)| p == &file.path) {
                    out.push((file.path.clone(), owner.clone(), f.codename.clone()));
                } else {
                    seen.push((file.path.clone(), f.codename.clone()));
                }
            }
        }
        out
    }

    /// Aligned summary — what the CLI prints and the TUI draws.
    pub fn table(&self) -> String {
        let mut out = format!(
            "{:<12} {:<11} {:<8} {:>7} {:>9} {}\n",
            "CODE", "STATE", "VERDICT", "FILES", "TOKENS", "TASK"
        );
        for f in &self.operators {
            out.push_str(&format!(
                "{:<12} {:<11} {:<8} {:>7} {:>9} {}\n",
                truncate(&f.codename, 12),
                f.state.label(),
                f.verdict.label(),
                f.files.len(),
                f.usage.total(),
                truncate(&f.task.split_whitespace().collect::<Vec<_>>().join(" "), 48),
            ));
        }
        out
    }
}

// ─── errors ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissionError {
    Git(GitError),
    Dirty {
        path: String,
    },
    NoOperators,
    /// Strategos itself could not be reached; a mission needs its commander.
    Strategos(String),
}

impl std::fmt::Display for MissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MissionError::Git(e) => write!(f, "{e}"),
            MissionError::Dirty { path } => write!(
                f,
                "{path} has uncommitted changes; the mission applies files to this working \
                 tree, so commit or stash first"
            ),
            MissionError::NoOperators => write!(f, "a mission needs at least one Operator"),
            MissionError::Strategos(detail) => {
                write!(f, "{STRATEGOS} could not plan the mission: {detail}")
            }
        }
    }
}

impl std::error::Error for MissionError {}

// ─── the mission ─────────────────────────────────────────────────────────────

/// Plan, deploy, execute, review, apply.
///
/// `tools` is a factory rather than a registry because every Operator needs tools rooted in
/// **its own** worktree. Sharing one registry would be the exact bug the worktrees exist to
/// prevent.
pub fn run_mission(
    repo: &Repo,
    strategos: &dyn Model,
    mut operators: Vec<Operator>,
    task: &str,
    cfg: &MissionConfig,
    observer: &(dyn Fn(MissionEvent) + Sync),
) -> Result<MissionReport, MissionError> {
    if operators.is_empty() {
        return Err(MissionError::NoOperators);
    }
    if !repo.is_clean().map_err(MissionError::Git)? {
        return Err(MissionError::Dirty {
            path: repo.root().display().to_string(),
        });
    }
    let base = cfg
        .base
        .clone()
        .unwrap_or(repo.head().map_err(MissionError::Git)?);

    // ── 1. Strategos writes the plan.
    observer(MissionEvent::Planning {
        task: task.to_string(),
    });
    let layout = repo_layout(repo.root());
    let mut plan = plan_mission(strategos, task, &layout, &operators, &cfg.agent)
        .map_err(MissionError::Strategos)?;

    // Step 1b: Strategos names its team, now that it has said what each one is for.
    let renames = name_phalanx(&mut operators, &mut plan);
    observer(MissionEvent::Named { renames });
    observer(MissionEvent::Planned { plan: plan.clone() });

    // ── 2. Deploy: one worktree per operator, sequentially (git writes to a shared object
    //       store; there is nothing to gain from racing it).
    let owns_root = cfg.worktree_dir.is_none();
    let root = cfg
        .worktree_dir
        .clone()
        .unwrap_or_else(|| repo.root().join(".git").join("stoic-codex-missions"));
    let mut worktrees: Vec<Worktree> = Vec::with_capacity(operators.len());
    for member in &operators {
        let dir = root.join(&member.codename);
        match repo.create_worktree(&member.codename, &dir, Some(&base)) {
            Ok(wt) => {
                observer(MissionEvent::Deployed {
                    codename: member.codename.clone(),
                    branch: wt.branch().to_string(),
                });
                worktrees.push(wt);
            }
            Err(e) => {
                // Clean up what was created, then report: a half-deployed mission is worse
                // than no mission.
                for wt in worktrees {
                    let _ = wt.remove();
                }
                if owns_root {
                    let _ = std::fs::remove_dir_all(&root);
                }
                return Err(MissionError::Git(e));
            }
        }
    }

    // ── 3. Execute in parallel, at most `max_parallel` at a time.
    let started = Instant::now();
    let results = execute(&operators, &worktrees, &plan, cfg, observer);
    let parallel_ms = started.elapsed().as_millis() as u64;

    // ── 4. Strategos reviews what came back.
    let mut results = results;
    let (review_note, review_failed) = if cfg.review {
        observer(MissionEvent::Reviewing {
            reports: results.len(),
        });
        match review_mission(strategos, task, &plan, &results, &cfg.agent) {
            Ok(review) => {
                apply_review(&mut results, &review);
                let verdicts: Vec<(String, Verdict, String)> = results
                    .iter()
                    .map(|r| (r.codename.clone(), r.verdict, r.reason.clone()))
                    .collect();
                observer(MissionEvent::Reviewed {
                    plan: plan.clone(),
                    verdicts,
                });
                (
                    format!("{} reviewed {} report(s)", STRATEGOS, results.len()),
                    false,
                )
            }
            Err(e) => {
                // A failed review must not lose work that is already done: accept whatever
                // ran cleanly and say why.
                for r in results.iter_mut() {
                    if r.usable() {
                        r.verdict = Verdict::Accepted;
                        r.reason = "accepted unreviewed: the review step failed".into();
                    }
                }
                (
                    format!("{STRATEGOS} could not review ({e}); every clean result was kept"),
                    true,
                )
            }
        }
    } else {
        for r in results.iter_mut() {
            if r.usable() {
                r.verdict = Verdict::Accepted;
                r.reason = "review disabled".into();
            }
        }
        (
            "review disabled: every clean result was kept".to_string(),
            false,
        )
    };

    // ── 5. Apply accepted files, one at a time, first writer wins.
    let mut applied_owner: Vec<(String, String)> = Vec::new(); // (file, codename)
    for r in results.iter_mut() {
        if r.verdict != Verdict::Accepted {
            continue;
        }
        let Some(wt) = worktrees
            .iter()
            .find(|w| w.branch().ends_with(&slug(&r.codename)))
        else {
            r.reason = format!("{} (worktree not found)", r.reason);
            continue;
        };
        for file in &r.files {
            if let Some((_, owner)) = applied_owner.iter().find(|(p, _)| p == &file.path) {
                observer(MissionEvent::Conflict {
                    file: file.path.clone(),
                    owner: owner.clone(),
                    other: r.codename.clone(),
                });
                r.reason = format!("{}; {} already had {}", r.reason, owner, file.path);
                continue;
            }
            match wt.copy_file_to(&file.path, repo.root()) {
                Ok(()) => {
                    applied_owner.push((file.path.clone(), r.codename.clone()));
                    r.applied.push(file.path.clone());
                    observer(MissionEvent::Applying {
                        codename: r.codename.clone(),
                        file: file.path.clone(),
                    });
                }
                Err(e) => r.reason = format!("{}; could not apply {}: {e}", r.reason, file.path),
            }
        }
    }

    // ── 6. Commit, if anything landed.
    let total_applied = applied_owner.len();
    let mut commit = None;
    let mut note_commit_failure: Option<String> = None;
    if total_applied > 0 && cfg.commit {
        let names: Vec<String> = applied_owner.iter().map(|(f, _)| f.clone()).collect();
        let message = format!(
            "stoic-codex mission: {total_applied} file(s) from {} operators of {PHALANX} under {STRATEGOS}\n\n{}",
            results.iter().filter(|r| r.verdict == Verdict::Accepted && !r.applied.is_empty()).count(),
            names.join(", ")
        );
        match repo.commit_all(&message) {
            Ok(sha) => commit = sha,
            // A commit that cannot be made must be visible: the files are in the tree, so
            // the report says exactly that rather than pretending nothing happened.
            Err(e) => note_commit_failure = Some(format!("could not commit: {e}")),
        }
    }

    // ── 7. Clean up. Never discard uncommitted recovery work, and only delete a branch when
    // every file it produced is present in the user's working tree.
    let branches: Vec<(String, String)> = worktrees
        .iter()
        .zip(operators.iter())
        .map(|(wt, operator)| (wt.branch().to_string(), operator.codename.clone()))
        .collect();
    let mut retained_worktree_branches = Vec::new();
    let mut cleanup_warnings = Vec::new();
    if !cfg.keep_worktrees {
        for (operator, wt) in operators.iter().zip(worktrees.iter()) {
            let (dirty, inspect_failed) = match wt.git(&["status", "--porcelain"]) {
                Ok(status) => (!status.trim().is_empty(), false),
                Err(e) => {
                    cleanup_warnings.push(format!(
                        "could not inspect {}'s worktree; kept it for recovery: {e}",
                        operator.codename
                    ));
                    (true, true)
                }
            };
            if dirty {
                if !inspect_failed {
                    cleanup_warnings.push(format!(
                        "kept {}'s worktree because it still contains uncommitted changes",
                        operator.codename
                    ));
                }
                retained_worktree_branches.push(wt.branch().to_string());
                continue;
            }
            if let Err(e) = wt.clone().remove() {
                cleanup_warnings.push(format!(
                    "could not remove {}'s worktree; kept its branch for recovery: {e}",
                    operator.codename
                ));
                retained_worktree_branches.push(wt.branch().to_string());
            }
        }
        if owns_root && retained_worktree_branches.is_empty() && root.exists() {
            if let Err(e) = std::fs::remove_dir_all(&root) {
                cleanup_warnings.push(format!("could not remove mission worktree directory: {e}"));
            }
        }
        if let Err(e) = repo.git(&["worktree", "prune"]) {
            cleanup_warnings.push(format!("could not prune Git worktree metadata: {e}"));
        }
    } else {
        retained_worktree_branches.extend(worktrees.iter().map(|wt| wt.branch().to_string()));
    }
    if !cfg.keep_branches {
        for (branch, codename) in &branches {
            if retained_worktree_branches.contains(branch) {
                continue;
            }
            let has_unapplied_work = results
                .iter()
                .find(|result| &result.codename == codename)
                .is_some_and(|result| {
                    !result.files.is_empty() && result.applied.len() < result.files.len()
                });
            if !has_unapplied_work {
                if let Err(e) = repo.delete_branch(branch) {
                    cleanup_warnings.push(format!("could not delete branch {branch}: {e}"));
                }
            }
        }
    }

    let conflicts = {
        let mut seen: Vec<(String, String)> = Vec::new();
        let mut n = 0;
        for r in &results {
            if r.verdict != Verdict::Accepted {
                continue;
            }
            for f in &r.files {
                if let Some((_, owner)) = seen.iter().find(|(p, _)| p == &f.path) {
                    let _ = owner;
                    n += 1;
                } else {
                    seen.push((f.path.clone(), r.codename.clone()));
                }
            }
        }
        n
    };
    observer(MissionEvent::Done {
        applied: total_applied,
        conflicts,
    });

    let commit_error = note_commit_failure;
    let review_note = match &commit_error {
        Some(note) => format!("{review_note} · {note}"),
        None => review_note,
    };

    Ok(MissionReport {
        task: task.to_string(),
        plan,
        operators: results,
        commit,
        base,
        parallel_ms,
        review_note,
        review_failed,
        commit_error,
        cleanup_warnings,
    })
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn execute(
    members: &[Operator],
    worktrees: &[Worktree],
    plan: &Plan,
    cfg: &MissionConfig,
    observer: &(dyn Fn(MissionEvent) + Sync),
) -> Vec<OperatorResult> {
    let limit = cfg.max_parallel.max(1);
    let mut results: Vec<OperatorResult> = Vec::with_capacity(members.len());
    for chunk_start in (0..members.len()).step_by(limit) {
        let chunk_end = (chunk_start + limit).min(members.len());
        let mut batch: Vec<OperatorResult> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for i in chunk_start..chunk_end {
                let member = &members[i];
                let wt = &worktrees[i];
                let task = plan
                    .task_for(&member.codename)
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        plan.assignments
                            .first()
                            .map(|a| a.task.clone())
                            .unwrap_or_default()
                    });
                handles.push(scope.spawn(move || run_operators(member, wt, &task, cfg, observer)));
            }
            handles
                .into_iter()
                .map(|h| {
                    h.join().unwrap_or_else(|_| {
                        let mut r = OperatorResult::from_run("unknown", "");
                        r.state = OperatorState::Failed;
                        r.error = Some("the Operator thread panicked".into());
                        r
                    })
                })
                .collect()
        });
        results.append(&mut batch);
    }
    // Stable order regardless of which thread finished first.
    results.sort_by(|a, b| a.codename.cmp(&b.codename));
    results
}

fn run_operators(
    member: &Operator,
    wt: &Worktree,
    task: &str,
    cfg: &MissionConfig,
    observer: &(dyn Fn(MissionEvent) + Sync),
) -> OperatorResult {
    let started = Instant::now();
    let mut result = OperatorResult::from_run(&member.codename, task);
    let sandbox = match crate::sandbox::Sandbox::new(wt.path()) {
        Ok(s) => std::sync::Arc::new(s),
        Err(e) => {
            result.state = OperatorState::Failed;
            result.error = Some(e.to_string());
            return result;
        }
    };
    let registry = ToolRegistry::editing(sandbox);

    let mut agent_cfg = cfg.agent.clone();
    agent_cfg.system = Some(operator_prompt(&member.codename, wt.branch()));
    let mut on_event = |event: Event| {
        observer(MissionEvent::Agent {
            codename: member.codename.clone(),
            event,
        });
    };

    let agent_failed = match run_agent(
        member.model.as_ref(),
        &registry,
        task,
        &agent_cfg,
        &mut on_event,
    ) {
        Ok(outcome) => {
            result.text = outcome.text;
            result.steps = outcome.steps;
            result.tool_calls = outcome.tool_calls;
            result.usage = outcome.usage;
            result.stop = Some(outcome.stop);
            false
        }
        Err(e) => {
            result.error = Some(describe(&e));
            true
        }
    };

    // Harvest even after a provider/tool-loop error. The partial result stays untrusted and
    // cannot be applied, but committing it to its isolated branch lets the user recover it.
    match wt.changed_files() {
        Ok(files) => result.files = files,
        Err(e) => add_result_error(
            &mut result,
            format!("could not inspect worktree changes: {e}"),
        ),
    }
    result.patch = wt.patch().unwrap_or_default();
    result.commit = match wt.commit_all(&format!(
        "stoic-codex: {} — {}",
        member.codename,
        short(task)
    )) {
        Ok(sha) => sha,
        Err(e) => {
            add_result_error(
                &mut result,
                format!("could not commit worktree changes: {e}"),
            );
            None
        }
    };
    result.state = if agent_failed || result.error.is_some() {
        OperatorState::Failed
    } else if result.files.is_empty() {
        OperatorState::Empty
    } else {
        OperatorState::Succeeded
    };

    if !agent_failed {
        if let Some(cmd) = &cfg.verify {
            let (ok, output) = verify(wt.path(), cmd);
            result.verified = Some(ok);
            result.verify_output = output;
            observer(MissionEvent::Verified {
                codename: member.codename.clone(),
                ok,
            });
        }
    }

    result.elapsed_ms = started.elapsed().as_millis() as u64;
    observer(MissionEvent::Returned {
        codename: member.codename.clone(),
        state: result.state,
    });
    result
}

fn add_result_error(result: &mut OperatorResult, detail: String) {
    match &mut result.error {
        Some(existing) => {
            existing.push_str("; ");
            existing.push_str(&detail);
        }
        None => result.error = Some(detail),
    }
}

fn describe(error: &AgentError) -> String {
    error.to_string()
}

fn short(task: &str) -> String {
    let one_line = task.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut s: String = one_line.chars().take(50).collect();
    if one_line.chars().count() > 50 {
        s.push('…');
    }
    s
}

fn verify(dir: &Path, cmd: &VerifyCommand) -> (bool, String) {
    use std::process::{Command, Stdio};
    match Command::new(&cmd.program)
        .args(&cmd.args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).to_string();
            let err = String::from_utf8_lossy(&output.stderr).to_string();
            if !err.trim().is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&err);
            }
            if text.chars().count() > 4000 {
                text = text.chars().take(4000).collect::<String>() + "\n… truncated";
            }
            (output.status.success(), text)
        }
        Err(e) => (false, format!("cannot run {:?}: {e}", cmd.program)),
    }
}

// ─── prompts ─────────────────────────────────────────────────────────────────

/// What Strategos is told. It has no tools: its whole job is the plan and the review, and a
/// commander that starts editing files is a commander not commanding.
pub fn strategos_plan_prompt(task: &str, layout: &str, codenames: &[String]) -> CanonicalRequest {
    let catalog: String = stoic_codex_brand::phalanx::CODES
        .iter()
        .map(|code| format!("{} = {}", code.id, code.role))
        .collect::<Vec<_>>()
        .join("; ");
    let system = format!(
        "You are {STRATEGOS}, the commander of {PHALANX} — a line of coding agents named by \
         short-hand system codes.\n\
         \n\
         You have {} operator(s): {}.\n\
         \n\
         Each one works alone in its own git worktree of the same repository, at the same time. \
         They cannot see each other. They will each be given one assignment and will edit files \
         to complete it.\n\
         \n\
         Split the task below into assignments that do not overlap. Prefer assignments that own \
         distinct files or directories: two operators editing the same file is wasted work. Give \
         every operator something real to do — if the task only needs one, give the others \
         verification, documentation or test work rather than leaving them idle.\n\
         \n\
         Name each operator with a code from this catalogue. The code is the job: it is what the \
         operator is called in every log line, so pick the one that describes the work:\n\
         {catalog}\n\
         \n\
         Reply with JSON only, no prose and no code fences:\n\
         {{\"assignments\":[{{\"codename\":\"slot-1\",\"code\":\"x1-write\",\"task\":\"what that operator must do\"}}]}}\n\
         \n\
         Use exactly these codenames: {}, and a different code for each operator.",
        codenames.len(),
        codenames.join(", "),
        codenames.join(", ")
    );
    let user = format!("Repository layout:\n{layout}\n\nTask:\n{task}");
    request(system, user)
}

pub fn strategos_review_prompt(
    task: &str,
    plan: &Plan,
    results: &[OperatorResult],
) -> CanonicalRequest {
    let mut fleet = String::new();
    for r in results {
        fleet.push_str(&format!(
            "\n--- {} ---\nassignment: {}\nstate: {}{}\nfiles: {}\nreport: {}\n",
            r.codename,
            r.task,
            r.state.label(),
            match r.verified {
                Some(true) => " · verification passed",
                Some(false) => " · verification FAILED",
                None => "",
            },
            if r.files.is_empty() {
                "(none)".to_string()
            } else {
                r.files
                    .iter()
                    .map(|f| format!("{} {}", f.status, f.path))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            truncate(r.text.trim(), 600)
        ));
        if !r.patch.is_empty() {
            fleet.push_str(&format!(
                "diff (truncated):\n{}\n",
                truncate(&r.patch, 2000)
            ));
        }
    }
    let system = format!(
        "You are {STRATEGOS}. The {} operators of {PHALANX} have returned from their \
         assignments in the same repository.\n\
         \n\
         Judge each one against the original task: did it do the work it was given, is the change \
         coherent, and does it belong in the repository? Accept work that is right. Reject work \
         that is wrong, incomplete, or that touches files another agent owns.\n\
         \n\
         A failed verification is not automatically a rejection if the failure is unrelated to \
         this change, but say so.\n\
         \n\
         Reply with JSON only, no prose and no code fences:\n\
         {{\"verdicts\":[{{\"codename\":\"x1-write\",\"accept\":true,\"reason\":\"one sentence\"}}]}}",
        results.len()
    );
    let user = format!(
        "Task given to {PHALANX}:\n{task}\n\nThe plan was:\n{}\n\nWhat came back:{fleet}",
        plan.assignments
            .iter()
            .map(|a| format!("- {}: {}", a.codename, a.task))
            .collect::<Vec<_>>()
            .join("\n")
    );
    request(system, user)
}

fn request(system: String, user: String) -> CanonicalRequest {
    CanonicalRequest {
        model: String::new(),
        system: Some(system),
        messages: vec![Message {
            role: Role::User,
            content: vec![Content::text(user)],
        }],
        tools: Vec::new(),
        tool_choice: None,
        max_tokens: None,
        temperature: None,
        top_p: None,
        stop: Vec::new(),
        json_mode: false,
        cache: stoic_codex_providers::CacheStrategy::Off,
        reasoning: None,
        stream: Some(false),
        extra: Default::default(),
    }
}

fn operator_prompt(codename: &str, branch: &str) -> String {
    // The code is not decoration: it is the operator's job description, so the assignment
    // arrives inside a sentence that already says what kind of work this is.
    let standing = match stoic_codex_brand::phalanx::lookup(codename) {
        Some(code) => format!(
            "You are {codename} — {} — one of {PHALANX} under {STRATEGOS}.\n\
         \n\
         Your code names your job: {}. Stay inside it. If your assignment asks for a different \
         kind of work, do the assignment and say so in your report.\n",
            code.role, code.role
        ),
        None => format!("You are {codename}, one of {PHALANX} under {STRATEGOS}.\n"),
    };
    format!(
        "{standing}\
         \n\
         You are alone in your own git worktree on branch {branch}. Other operators are working on \
         other parts of the same repository at the same time, in other directories: you cannot \
         see their edits and they cannot see yours. Stay strictly inside your assignment — if you \
         notice a problem outside it, mention it in your final answer instead of fixing it.\n\
         \n\
         Work by reading, not guessing: use search and read_file before changing anything. Make \
         the smallest change that completes your assignment, then check it with git_diff.\n\
         \n\
         Every tool path is relative to the repository root and cannot leave it. When you are \
         done, answer in one short paragraph: what you changed and anything the commander must \
         know. No code blocks."
    )
}

// ─── planning and review parsing ─────────────────────────────────────────────

fn plan_mission(
    strategos: &dyn Model,
    task: &str,
    layout: &str,
    operators: &[Operator],
    agent_cfg: &AgentConfig,
) -> Result<Plan, String> {
    let codenames: Vec<String> = operators.iter().map(|f| f.codename.clone()).collect();
    let mut request = strategos_plan_prompt(task, layout, &codenames);
    request.model = agent_cfg.model.clone();
    request.max_tokens = agent_cfg.max_tokens;

    let response = strategos.complete(&request).map_err(|e| e.to_string())?;
    let raw = response.text();
    Ok(parse_plan(&raw, &codenames, task))
}

/// Parse Strategos' plan. Anything unreadable degrades to a fallback plan rather than
/// failing the mission: a commander that mumbles should still command.
pub fn parse_plan(raw: &str, codenames: &[String], task: &str) -> Plan {
    if let Some(value) = json_in(raw) {
        if let Some(list) = value.get("assignments").and_then(|a| a.as_array()) {
            let mut assignments: Vec<Assignment> = Vec::new();
            for item in list {
                let codename = item
                    .get("codename")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let body = item
                    .get("task")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if codename.is_empty() || body.is_empty() {
                    continue;
                }
                // Strategos may name the operator as well as task it.
                let code = item
                    .get("code")
                    .and_then(|c| c.as_str())
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty());
                // Only accept codenames we actually deployed.
                if !codenames.iter().any(|c| c == &codename) {
                    continue;
                }
                assignments.push(Assignment {
                    codename,
                    code,
                    task: body,
                });
            }
            if !assignments.is_empty() {
                // Every Operator needs a task; one Strategos forgot gets the whole task.
                for codename in codenames {
                    if !assignments.iter().any(|a| &a.codename == codename) {
                        assignments.push(Assignment::new(codename, task.to_string()));
                    }
                }
                assignments.sort_by(|a, b| a.codename.cmp(&b.codename));
                return Plan {
                    assignments,
                    source: PlanSource::Strategos,
                    raw: raw.to_string(),
                };
            }
        }
    }
    Plan {
        assignments: codenames
            .iter()
            .map(|c| Assignment::new(c.clone(), task.to_string()))
            .collect(),
        source: PlanSource::Fallback,
        raw: raw.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub verdicts: Vec<(String, bool, String)>,
}

fn review_mission(
    strategos: &dyn Model,
    task: &str,
    plan: &Plan,
    results: &[OperatorResult],
    agent_cfg: &AgentConfig,
) -> Result<Review, String> {
    let mut request = strategos_review_prompt(task, plan, results);
    request.model = agent_cfg.model.clone();
    request.max_tokens = agent_cfg.max_tokens;
    let response = strategos.complete(&request).map_err(|e| e.to_string())?;
    parse_review(&response.text(), results)
}

pub fn parse_review(raw: &str, results: &[OperatorResult]) -> Result<Review, String> {
    let value =
        json_in(raw).ok_or_else(|| format!("no JSON in the review: {}", truncate(raw, 200)))?;
    let list = value
        .get("verdicts")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "the review has no `verdicts` array".to_string())?;
    let mut verdicts = Vec::new();
    for item in list {
        let codename = item
            .get("codename")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if codename.is_empty() || !results.iter().any(|r| r.codename == codename) {
            continue;
        }
        let accept = item
            .get("accept")
            .and_then(|a| a.as_bool())
            .unwrap_or(false);
        let reason = item
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        verdicts.push((codename, accept, reason));
    }
    if verdicts.is_empty() {
        return Err("the review named no agents that exist".into());
    }
    Ok(Review { verdicts })
}

fn apply_review(results: &mut [OperatorResult], review: &Review) {
    for result in results.iter_mut() {
        match review
            .verdicts
            .iter()
            .find(|(c, _, _)| c == &result.codename)
        {
            Some((_, accept, reason)) => {
                let accepted = *accept && result.usable();
                result.verdict = if accepted {
                    Verdict::Accepted
                } else {
                    Verdict::Rejected
                };
                result.reason = if *accept && !result.usable() {
                    format!("{reason} (nothing usable to apply)")
                } else {
                    reason.clone()
                };
            }
            None => {
                // Not mentioned: keep it only if it is clearly fine, and say that the
                // commander did not look at it.
                result.verdict = if result.usable() {
                    Verdict::Accepted
                } else {
                    Verdict::Unreviewed
                };
                result.reason = "not mentioned by the review; kept because it ran cleanly".into();
            }
        }
    }
}

/// Find the first JSON object in a string that may be fenced, prefixed, or chatty.
pub fn json_in(raw: &str) -> Option<serde_json::Value> {
    let trimmed = raw.trim();
    let candidate = if let Some(start) = trimmed.find('{') {
        let end = trimmed.rfind('}')?;
        if end <= start {
            return None;
        }
        &trimmed[start..=end]
    } else {
        return None;
    };
    serde_json::from_str(candidate).ok()
}

/// A short listing of the repository, for the plan prompt. Token-cheap and enough for
/// Strategos to divide the work sensibly.
pub fn repo_layout(root: &Path) -> String {
    const SKIP: &[&str] = &[
        ".git",
        "node_modules",
        "target",
        "dist",
        "build",
        ".venv",
        "__pycache__",
    ];
    let mut out = String::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    let mut items: Vec<_> = entries.flatten().collect();
    items.sort_by_key(|e| e.file_name());
    for entry in items.into_iter().take(60) {
        let name = entry.file_name().to_string_lossy().to_string();
        if SKIP.contains(&name.as_str()) {
            continue;
        }
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let count = std::fs::read_dir(entry.path())
                .map(|rd| rd.count())
                .unwrap_or(0);
            out.push_str(&format!("{name}/ ({count} entries)\n"));
        } else {
            out.push_str(&format!("{name}\n"));
        }
    }
    out
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}

/// A model that answers from a fixed script and records what it was asked. The tests use it
/// for both Strategos and the Operator; the shape is public so an example can too.
pub struct Scripted {
    script: std::sync::Mutex<Vec<String>>,
    seen: std::sync::Mutex<Vec<CanonicalRequest>>,
}

impl Scripted {
    pub fn new<S: Into<String>>(script: Vec<S>) -> Scripted {
        Scripted {
            script: std::sync::Mutex::new(script.into_iter().map(Into::into).collect()),
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn requests(&self) -> Vec<CanonicalRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl Model for Scripted {
    fn complete(
        &self,
        request: &CanonicalRequest,
    ) -> Result<stoic_codex_providers::CanonicalResponse, ModelError> {
        self.seen.lock().unwrap().push(request.clone());
        let mut script = self.script.lock().unwrap();
        if script.is_empty() {
            return Err(ModelError::Refused {
                status: 500,
                detail: "script exhausted".into(),
            });
        }
        let text = script.remove(0);
        Ok(stoic_codex_providers::CanonicalResponse {
            model: request.model.clone(),
            content: vec![Content::text(text)],
            stop_reason: stoic_codex_providers::StopReason::EndTurn,
            usage: Usage {
                input_tokens: 100,
                output_tokens: 40,
                ..Usage::default()
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A model that is never called: these tests are about naming, not about running.
    struct Null;

    impl Model for Null {
        fn complete(
            &self,
            _request: &CanonicalRequest,
        ) -> Result<stoic_codex_providers::CanonicalResponse, ModelError> {
            Err(ModelError::Transport(
                "naming tests do not call a model".into(),
            ))
        }
    }

    fn names() -> Vec<String> {
        vec![
            provisional_name(1),
            provisional_name(2),
            provisional_name(3),
        ]
    }

    #[test]
    fn an_unnamed_team_is_a_set_of_slots() {
        // Codenames are jobs, and the jobs are not known until the plan is. Until then the
        // operator is a slot.
        assert_eq!(names(), vec!["slot-1", "slot-2", "slot-3"]);
        assert_eq!(STRATEGOS, "Strategos");
        assert_eq!(PHALANX, "The Phalanx");
    }

    #[test]
    fn strategos_names_the_team_from_its_own_plan() {
        let raw = r#"{"assignments":[
            {"codename":"slot-1","code":"x2-check","task":"add tests for the token cache"},
            {"codename":"slot-2","code":"x3-ship","task":"wire up the release pipeline"}
        ]}"#;
        let mut plan = parse_plan(raw, &names(), "do the thing");
        let mut team = vec![
            Operator::new("slot-1", Box::new(Null)),
            Operator::new("slot-2", Box::new(Null)),
            Operator::new("slot-3", Box::new(Null)),
        ];
        let renames = name_phalanx(&mut team, &mut plan);
        assert_eq!(team[0].codename, "x2-check");
        assert_eq!(team[1].codename, "x3-ship");
        // slot-3 was never planned, so its assignment text is empty and it takes a free code.
        assert!(
            CODES.iter().any(|c| c.id == team[2].codename),
            "{}",
            team[2].codename
        );
        assert_eq!(renames.len(), 3);
        assert_eq!(renames[0], ("slot-1".to_string(), "x2-check".to_string()));
        // The plan is rewritten with the codes, so `task_for` still finds the work.
        assert_eq!(
            plan.task_for("x2-check"),
            Some("add tests for the token cache")
        );
    }

    #[test]
    fn a_bogus_or_taken_code_from_the_commander_is_overruled() {
        let raw = r#"{"assignments":[
            {"codename":"slot-1","code":"x9-nonsense","task":"add tests"},
            {"codename":"slot-2","code":"x2-check","task":"verify the parser"}
        ]}"#;
        let mut plan = parse_plan(raw, &[provisional_name(1), provisional_name(2)], "t");
        let mut team = vec![
            Operator::new("slot-1", Box::new(Null)),
            Operator::new("slot-2", Box::new(Null)),
        ];
        name_phalanx(&mut team, &mut plan);
        // slot-1's invented code is replaced by what its assignment looks like ...
        assert_eq!(team[0].codename, "x2-check");
        // ... and slot-2, whose code slot-1 stole, still gets a distinct one.
        assert_ne!(team[0].codename, team[1].codename);
    }

    #[test]
    fn every_operator_of_a_real_misson_gets_a_distinct_code() {
        let raw = r#"{"assignments":[
            {"codename":"slot-1","task":"implement the rate limiter"},
            {"codename":"slot-2","task":"add tests for the limiter"},
            {"codename":"slot-3","task":"map the repository layout"}
        ]}"#;
        let slots: Vec<String> = (1..=3).map(provisional_name).collect();
        let mut plan = parse_plan(raw, &slots, "t");
        let mut team: Vec<Operator> = slots
            .iter()
            .map(|s| Operator::new(s.clone(), Box::new(Null)))
            .collect();
        name_phalanx(&mut team, &mut plan);
        let mut codes: Vec<&str> = team.iter().map(|o| o.codename.as_str()).collect();
        codes.sort();
        let before = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), before, "codes collided: {codes:?}");
        assert_eq!(
            team.iter().map(|o| o.codename.as_str()).collect::<Vec<_>>(),
            vec!["x1-write", "x2-check", "x0-core"]
        );
    }

    #[test]
    fn a_clean_plan_is_parsed_and_sorted() {
        let raw = r#"{"assignments":[
            {"codename":"slot-2","task":"write the tests"},
            {"codename":"slot-1","task":"add the parser"}
        ]}"#;
        let plan = parse_plan(raw, &names(), "do the thing");
        assert_eq!(plan.source, PlanSource::Strategos);
        assert_eq!(
            plan.assignments.len(),
            3,
            "the forgotten agent still gets work"
        );
        assert_eq!(plan.task_for("slot-1"), Some("add the parser"));
        assert_eq!(plan.task_for("slot-2"), Some("write the tests"));
        // slot-3 was not assigned, so it inherits the whole task rather than idling.
        assert_eq!(plan.task_for("slot-3"), Some("do the thing"));
    }

    #[test]
    fn a_plan_wrapped_in_prose_and_fences_is_still_read() {
        let raw = "Sure! Here is the plan:\n```json\n{\"assignments\":[{\"codename\":\"slot-1\",\"task\":\"go\"}]}\n```\nGood luck.";
        let plan = parse_plan(raw, &names(), "task");
        assert_eq!(plan.source, PlanSource::Strategos);
        assert_eq!(plan.task_for("slot-1"), Some("go"));
    }

    #[test]
    fn invented_codenames_are_ignored() {
        let raw = r#"{"assignments":[{"codename":"slot-9","task":"ghost"}]}"#;
        let plan = parse_plan(raw, &names(), "task");
        assert_eq!(plan.source, PlanSource::Fallback);
        assert!(plan.assignments.iter().all(|a| a.codename != "slot-9"));
    }

    #[test]
    fn unreadable_plans_degrade_instead_of_failing() {
        for raw in ["", "I cannot help with that.", "{oops", "{}"] {
            let plan = parse_plan(raw, &names(), "the whole task");
            assert_eq!(plan.source, PlanSource::Fallback, "{raw:?}");
            assert_eq!(plan.assignments.len(), 3);
            assert!(plan.assignments.iter().all(|a| a.task == "the whole task"));
        }
    }

    fn result(codename: &str, state: OperatorState, files: &[&str]) -> OperatorResult {
        let mut r = OperatorResult::from_run(codename, "task");
        r.state = state;
        r.files = files
            .iter()
            .map(|f| FileChange {
                status: "M".into(),
                path: f.to_string(),
            })
            .collect();
        r
    }

    #[test]
    fn a_review_accepts_and_rejects_by_codename() {
        let mut results = vec![
            result("x1-write", OperatorState::Succeeded, &["a.rs"]),
            result("x2-check", OperatorState::Succeeded, &["b.rs"]),
        ];
        let review = parse_review(
            r#"{"verdicts":[
                {"codename":"x1-write","accept":true,"reason":"correct"},
                {"codename":"x2-check","accept":false,"reason":"wrong file"}
            ]}"#,
            &results,
        )
        .unwrap();
        apply_review(&mut results, &review);
        assert_eq!(results[0].verdict, Verdict::Accepted);
        assert_eq!(results[1].verdict, Verdict::Rejected);
        assert_eq!(results[1].reason, "wrong file");
    }

    #[test]
    fn accepting_an_agent_that_changed_nothing_is_downgraded() {
        let mut results = vec![result("x1-write", OperatorState::Empty, &[])];
        let review = Review {
            verdicts: vec![("x1-write".into(), true, "looks fine".into())],
        };
        apply_review(&mut results, &review);
        assert_eq!(results[0].verdict, Verdict::Rejected);
        assert!(
            results[0].reason.contains("nothing usable"),
            "{}",
            results[0].reason
        );
    }

    #[test]
    fn an_unmentioned_agent_is_kept_but_flagged() {
        let mut results = vec![result("x1-write", OperatorState::Succeeded, &["a.rs"])];
        let review = Review {
            verdicts: vec![("x2-check".into(), true, "x".into())],
        };
        apply_review(&mut results, &review);
        assert_eq!(results[0].verdict, Verdict::Accepted);
        assert!(
            results[0].reason.contains("not mentioned"),
            "{}",
            results[0].reason
        );
    }

    #[test]
    fn a_review_that_names_nobody_is_an_error_so_the_caller_can_fall_back() {
        let results = vec![result("x1-write", OperatorState::Succeeded, &["a.rs"])];
        assert!(parse_review(
            r#"{"verdicts":[{"codename":"Ghost","accept":true}]}"#,
            &results
        )
        .is_err());
        assert!(parse_review("no json here", &results).is_err());
    }

    #[test]
    fn json_extraction_survives_chatty_output() {
        let value = json_in("blah {\"a\":1} trailing").unwrap();
        assert_eq!(value["a"], 1);
        assert!(json_in("nothing").is_none());
        assert!(json_in("{").is_none());
    }

    #[test]
    fn slugs_match_the_branch_naming_used_by_worktrees() {
        assert_eq!(slug("x1-write"), "x1-write");
        assert_eq!(slug("x1-write-2"), "x1-write-2");
        assert_eq!(slug("slot 3"), "slot-3");
    }
}

#[cfg(test)]
mod code_line_tests {
    use super::*;
    #[test]
    fn code_line_explains_a_code() {
        assert_eq!(
            code_line("x1-write"),
            "x1-write · the primary logic generator"
        );
        assert_eq!(code_line("nonsense"), "nonsense");
    }
}
