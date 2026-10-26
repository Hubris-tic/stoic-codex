//! Mission board view model: Strategos, the Phalanx, their states, budgets, and what to show.
//!
//! The shape mirrors `stoic-codex-agent`'s orchestrator on purpose: one **Strategos** (the boss —
//! it plans and it judges, and it touches no files) and N operators that together are **the
//! Phalanx**, each named by a short-hand system code (`x0-core`, `x1-write`, `x2-check`,
//! `x3-ship`, …) because the code *is* the job. Each gets one git worktree and does different
//! work on the same repository. There is no integrator and no
//! scoring committee; what Strategos accepts is copied into the working tree by the
//! orchestrator, whole files, first accepted writer wins.
//!
//! The orchestrator owns execution; this module owns *what the user sees* and which
//! transitions are legal. Keeping the state machine here means the TUI can never show
//! a nonsensical state (e.g. "done" then "running") because the transition is rejected
//! at the data layer, not in a render guard.

use std::fmt;

/// The only two roles a mission has. Strategos is singular; the Phalanx is many.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentRole {
    Strategos,
    Operator,
}

impl AgentRole {
    pub fn label(self) -> &'static str {
        match self {
            AgentRole::Strategos => "strategos",
            AgentRole::Operator => "operator",
        }
    }

    /// Display order on the board: the boss is always the top row.
    pub fn order(self) -> u8 {
        match self {
            AgentRole::Strategos => 0,
            AgentRole::Operator => 1,
        }
    }

    /// The boss never edits files, so it is never blocked by a sandbox or a bad patch.
    pub fn is_boss(self) -> bool {
        matches!(self, AgentRole::Strategos)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Queued,
    Running,
    WaitingApproval,
    Blocked,
    Done,
    Failed,
    Cancelled,
}

impl AgentState {
    pub fn is_terminal(self) -> bool {
        matches!(self, AgentState::Done | AgentState::Failed | AgentState::Cancelled)
    }

    /// Needs a human right now — these sort to the top of the board.
    pub fn needs_attention(self) -> bool {
        matches!(self, AgentState::WaitingApproval | AgentState::Blocked | AgentState::Failed)
    }

    /// Single-character glyph for narrow layouts.
    pub fn glyph(self) -> char {
        match self {
            AgentState::Queued => '·',
            AgentState::Running => '▶',
            AgentState::WaitingApproval => '?',
            AgentState::Blocked => '!',
            AgentState::Done => '✓',
            AgentState::Failed => '✗',
            AgentState::Cancelled => '–',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentState::Queued => "queued",
            AgentState::Running => "running",
            AgentState::WaitingApproval => "needs approval",
            AgentState::Blocked => "blocked",
            AgentState::Done => "done",
            AgentState::Failed => "failed",
            AgentState::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for AgentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Legal transitions. Anything a user would call a bug is rejected here.
pub fn can_transition(from: AgentState, to: AgentState) -> bool {
    use AgentState::*;
    if from == to {
        return true;
    }
    match from {
        Queued => matches!(to, Running | Cancelled | Failed),
        Running => matches!(to, WaitingApproval | Blocked | Done | Failed | Cancelled),
        WaitingApproval => matches!(to, Running | Failed | Cancelled),
        Blocked => matches!(to, Running | Failed | Cancelled),
        // Terminal states never move again; a retry is a new member of the mission.
        Done | Failed | Cancelled => false,
    }
}

/// What Strategos decided about what a Operator brought back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Not judged yet.
    Pending,
    Accepted,
    Rejected,
    /// The review never mentioned this Operator; its clean work was kept anyway.
    Unreviewed,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Pending => "…",
            Verdict::Accepted => "accepted",
            Verdict::Rejected => "rejected",
            Verdict::Unreviewed => "unreviewed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    /// `x1-write`, `x2-check`, … — the code is the id.
    pub id: String,
    pub role: AgentRole,
    pub state: AgentState,
    pub model: String,
    /// The Operator's own branch (`stoic-codex/<slug>`). Always `None` for Strategos.
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub task: String,
    /// What Strategos said when it reviewed this one.
    pub verdict: Verdict,
    /// 0..=100, coarse progress for the bar.
    pub progress: u8,
    /// Wall-clock seconds since this member started.
    pub elapsed_secs: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub micros: u64,
    /// Set when the member is blocked or failed, or when a verdict needs explaining.
    pub reason: Option<String>,
}

impl Agent {
    pub fn new(id: impl Into<String>, role: AgentRole, task: impl Into<String>) -> Self {
        Agent {
            id: id.into(),
            role,
            state: AgentState::Queued,
            model: String::new(),
            branch: None,
            worktree: None,
            task: task.into(),
            verdict: Verdict::Pending,
            progress: 0,
            elapsed_secs: 0,
            input_tokens: 0,
            output_tokens: 0,
            micros: 0,
            reason: None,
        }
    }

    /// The boss row. Its task is the mission task; it plans from the layout and reviews
    /// the reports, so it has no branch and no worktree.
    pub fn strategos(task: impl Into<String>) -> Self {
        Agent::new("Strategos", AgentRole::Strategos, task)
    }

    /// One operator of the Phalanx, by its short-hand system code: `x0-core`, `x1-write`, ….
    /// Strategos does the naming; by the time a board is drawn, the code is what the operator
    /// is called everywhere.
    pub fn operator(code: impl Into<String>, task: impl Into<String>) -> Self {
        Agent::new(code, AgentRole::Operator, task)
    }

    /// An operator still waiting for its code (before the plan exists): `slot-1`.
    pub fn slot(n: usize, task: impl Into<String>) -> Self {
        Agent::operator(slot_name(n), task)
    }

    /// A Operator's worktree, which is what its branch points at.
    pub fn deployed_at(mut self, branch: impl Into<String>, worktree: impl Into<String>) -> Self {
        self.branch = Some(branch.into());
        self.worktree = Some(worktree.into());
        self
    }

    /// Strategos has named this operator: take the code, and carry the branch and worktree
    /// along with it (they are derived from the name, so leaving them behind would make the
    /// board name a branch that no longer exists).
    pub fn rename(&mut self, code: impl Into<String>) {
        let old = std::mem::replace(&mut self.id, code.into());
        let rewrite = |s: &mut Option<String>| {
            if let Some(text) = s {
                *text = text.replace(&old, &self.id);
            }
        };
        rewrite(&mut self.branch);
        rewrite(&mut self.worktree);
    }

    pub fn transition(&mut self, to: AgentState) -> Result<(), MissionError> {
        if !can_transition(self.state, to) {
            return Err(MissionError::IllegalTransition {
                agent: self.id.clone(),
                from: self.state,
                to,
            });
        }
        self.state = to;
        if to.is_terminal() {
            self.progress = 100;
        }
        Ok(())
    }

    pub fn cost_usd(&self) -> f64 {
        self.micros as f64 / 1_000_000.0
    }

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    /// Did this member's work land in the working tree?
    pub fn landed(&self) -> bool {
        self.verdict == Verdict::Accepted
    }
}

/// One line in the mission's activity feed — what the board shows scrolling underneath the
/// table.
///
/// The orchestrator emits `MissionEvent`s; the app shell turns each into one of these. Keeping
/// the feed a plain list of strings in the view-model means the board can be rendered and
/// tested with no orchestrator, no threads and no clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Plan,
    Deploy,
    Tool,
    Result,
    Verdict,
    Conflict,
    Done,
}

impl ActivityKind {
    pub fn glyph(self) -> char {
        match self {
            ActivityKind::Plan => '◈',
            ActivityKind::Deploy => '→',
            ActivityKind::Tool => '·',
            ActivityKind::Result => '╵',
            ActivityKind::Verdict => '✓',
            ActivityKind::Conflict => '!',
            ActivityKind::Done => '●',
        }
    }

    /// Only two kinds are allowed colour: a conflict, and the end of the mission. The rest
    /// of a feed is history and should not compete with the live table above it.
    pub fn loud(self) -> bool {
        matches!(self, ActivityKind::Conflict | ActivityKind::Done)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    /// Seconds since the mission started.
    pub secs: u64,
    pub codename: Option<String>,
    pub kind: ActivityKind,
    pub text: String,
}

impl Activity {
    pub fn new(secs: u64, kind: ActivityKind, codename: Option<impl Into<String>>, text: impl Into<String>) -> Self {
        Activity { secs, kind, codename: codename.map(Into::into), text: text.into() }
    }

    /// `12:04` — minute resolution is plenty and keeps the gutter narrow.
    pub fn time(&self) -> String {
        format!("{:02}:{:02}", self.secs / 60, self.secs % 60)
    }
}

/// What an operator is called before Strategos names it. Same spelling as the orchestrator's
/// `provisional_name`: one word, one place.
pub fn slot_name(n: usize) -> String {
    format!("slot-{n}")
}

/// The catalogue of codes, re-exported so a view can say what a code means without a second
/// copy of the table.
pub use stoic_codex_brand::phalanx;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissionError {
    IllegalTransition { agent: String, from: AgentState, to: AgentState },
    BudgetExceeded { spent_micros: u64, cap_micros: u64 },
}

impl fmt::Display for MissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MissionError::IllegalTransition { agent, from, to } => {
                write!(f, "agent {agent}: illegal transition {from} -> {to}")
            }
            MissionError::BudgetExceeded { spent_micros, cap_micros } => write!(
                f,
                "budget exceeded: spent {} of {} micro-USD",
                spent_micros, cap_micros
            ),
        }
    }
}

impl std::error::Error for MissionError {}

#[derive(Debug, Clone)]
pub struct Mission {
    pub name: String,
    /// The Strategos row plus one row per Operator.
    pub agents: Vec<Agent>,
    /// Hard ceiling for the whole mission, micro-USD. 0 = unlimited.
    pub budget_micros: u64,
    /// Repository the mission is working in.
    pub repo: String,
    /// The commit the Operator were deployed from. Accepted files land in the working
    /// tree on top of it — there is no integration branch to merge into.
    pub base: String,
    /// Newest last. Capped, so a long mission cannot grow the UI's memory without bound.
    pub activity: Vec<Activity>,
}

/// How many feed lines a mission keeps. Older lines are dropped: a board that scrolls
/// forever is a board you cannot read, and nobody scrolls back through a tool log.
pub const ACTIVITY_CAP: usize = 400;

impl Mission {
    pub fn new(name: impl Into<String>, repo: impl Into<String>) -> Self {
        Mission {
            name: name.into(),
            agents: Vec::new(),
            budget_micros: 0,
            repo: repo.into(),
            base: "HEAD".into(),
            activity: Vec::new(),
        }
    }

    /// A whole mission in one call: Strategos plus `n` operators, all queued and still
    /// unnamed — the codes arrive with the plan, so until then they are slots.
    pub fn deploy(name: impl Into<String>, repo: impl Into<String>, n: usize, task: &str) -> Self {
        let mut mission = Mission::new(name, repo);
        mission.push(Agent::strategos(task));
        for i in 1..=n {
            mission.push(Agent::slot(i, task));
        }
        mission
    }

    /// Push a row. A mission has exactly one Strategos and it is always the first row.
    pub fn push(&mut self, agent: Agent) {
        if agent.role.is_boss() {
            self.agents.retain(|a| !a.role.is_boss());
            self.agents.insert(0, agent);
            return;
        }
        self.agents.push(agent);
    }

    /// Strategos names the Phalanx. Each slot takes its code, the row keeps its history
    /// (state, spend, progress) and the feed says what the code means, so the naming is
    /// visible rather than a silent re-label. Returns how many rows were renamed.
    pub fn name_phalanx(&mut self, renames: &[(String, String)]) -> usize {
        let mut renamed = 0;
        for (old, code) in renames {
            let Some(agent) = self.agents.iter_mut().find(|a| !a.role.is_boss() && &a.id == old)
            else {
                continue;
            };
            if old == code {
                continue;
            }
            agent.rename(code.clone());
            renamed += 1;
        }
        renamed
    }

    /// One feed line per rename, as the board prints it: `slot-1 → x1-write · the primary
    /// logic generator`. Kept next to [`Mission::name_phalanx`] so the label and the row
    /// cannot disagree.
    pub fn naming_log(secs: u64, renames: &[(String, String)]) -> Vec<Activity> {
        renames
            .iter()
            .filter(|(old, code)| old != code)
            .map(|(old, code)| {
                let meaning = stoic_codex_brand::phalanx::lookup(code)
                    .map(|c| format!("{} · {}", c.id, c.role))
                    .unwrap_or_else(|| code.clone());
                Activity::new(
                    secs,
                    ActivityKind::Plan,
                    Some(code.clone()),
                    format!("{old} → {meaning}"),
                )
            })
            .collect()
    }

    /// Append to the feed, dropping the oldest line past the cap.
    pub fn log(&mut self, activity: Activity) {
        self.activity.push(activity);
        if self.activity.len() > ACTIVITY_CAP {
            let excess = self.activity.len() - ACTIVITY_CAP;
            self.activity.drain(..excess);
        }
    }

    /// The last `n` lines, oldest first — i.e. exactly what a tail of the feed shows.
    pub fn recent(&self, n: usize) -> &[Activity] {
        let start = self.activity.len().saturating_sub(n);
        &self.activity[start..]
    }

    pub fn boss(&self) -> Option<&Agent> {
        self.agents.iter().find(|a| a.role.is_boss())
    }

    pub fn operators(&self) -> Vec<&Agent> {
        self.agents.iter().filter(|a| !a.role.is_boss()).collect()
    }

    pub fn spent_micros(&self) -> u64 {
        self.agents.iter().map(|a| a.micros).sum()
    }

    pub fn budget_used_fraction(&self) -> f64 {
        if self.budget_micros == 0 {
            0.0
        } else {
            (self.spent_micros() as f64 / self.budget_micros as f64).clamp(0.0, 1.0)
        }
    }

    pub fn check_budget(&self) -> Result<(), MissionError> {
        if self.budget_micros > 0 && self.spent_micros() > self.budget_micros {
            return Err(MissionError::BudgetExceeded {
                spent_micros: self.spent_micros(),
                cap_micros: self.budget_micros,
            });
        }
        Ok(())
    }

    pub fn summary(&self) -> Summary {
        let mut s = Summary::default();
        for a in &self.agents {
            match a.state {
                AgentState::Queued => s.queued += 1,
                AgentState::Running => s.running += 1,
                AgentState::WaitingApproval => s.waiting_approval += 1,
                AgentState::Blocked => s.blocked += 1,
                AgentState::Done => s.done += 1,
                AgentState::Failed => s.failed += 1,
                AgentState::Cancelled => s.cancelled += 1,
            }
            match a.verdict {
                Verdict::Accepted => s.accepted += 1,
                Verdict::Rejected => s.rejected += 1,
                Verdict::Unreviewed => s.unreviewed += 1,
                Verdict::Pending => {}
            }
            if a.role.is_boss() {
                s.boss_present = true;
            } else {
                s.operators += 1;
            }
            s.spent_micros += a.micros;
            s.input_tokens += a.input_tokens;
            s.output_tokens += a.output_tokens;
        }
        s.total = self.agents.len() as u32;
        s
    }

    /// Rows in display order: **Strategos first, always**, then the Operator ordered by
    /// who needs a human soonest. Stability matters — a board that reshuffles every
    /// redraw is unusable, so ties break on state, then codename.
    pub fn display_order(&self) -> Vec<&Agent> {
        let mut v: Vec<&Agent> = self.agents.iter().collect();
        v.sort_by(|a, b| {
            let pri = |s: AgentState| match s {
                AgentState::WaitingApproval => 0,
                AgentState::Failed => 1,
                AgentState::Blocked => 2,
                AgentState::Running => 3,
                AgentState::Queued => 4,
                AgentState::Done => 5,
                AgentState::Cancelled => 6,
            };
            a.role
                .order()
                .cmp(&b.role.order())
                .then(pri(a.state).cmp(&pri(b.state)))
                .then(a.id.cmp(&b.id))
        });
        v
    }

    /// Index in `display_order()` where the Operator start — the board draws a rule there,
    /// so the boss reads as separate from the agents it is commanding.
    pub fn phalanx_section(&self) -> Option<usize> {
        let order = self.display_order();
        let n = order.iter().take_while(|a| a.role.is_boss()).count();
        if n == 0 || n == order.len() {
            None
        } else {
            Some(n)
        }
    }

    /// What the status line should say. Never empty.
    pub fn headline(&self) -> String {
        let s = self.summary();
        if let Some(a) = self.agents.iter().find(|a| a.state.needs_attention()) {
            return match a.state {
                AgentState::WaitingApproval => format!("{} needs approval: {}", a.id, a.task),
                AgentState::Failed => {
                    format!("{} failed: {}", a.id, a.reason.clone().unwrap_or_default())
                }
                _ => format!("{} blocked: {}", a.id, a.reason.clone().unwrap_or_default()),
            };
        }
        if s.running > 0 {
            return format!(
                "{} operators deployed · {} running · ${:.2} spent",
                s.operators, s.running, s.spent_usd()
            );
        }
        if s.total > 0 && s.settled() == s.total {
            return format!(
                "mission complete · {} accepted · ${:.2} spent",
                s.accepted,
                s.spent_usd()
            );
        }
        if s.boss_present {
            return "strategos planning".to_string();
        }
        "mission ready".to_string()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub total: u32,
    pub queued: u32,
    pub running: u32,
    pub waiting_approval: u32,
    pub blocked: u32,
    pub done: u32,
    pub failed: u32,
    pub cancelled: u32,
    pub accepted: u32,
    pub rejected: u32,
    pub unreviewed: u32,
    pub operators: u32,
    pub boss_present: bool,
    pub spent_micros: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Summary {
    pub fn spent_usd(&self) -> f64 {
        self.spent_micros as f64 / 1_000_000.0
    }

    /// Finished regardless of outcome.
    pub fn settled(&self) -> u32 {
        self.done + self.failed + self.cancelled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, role: AgentRole, state: AgentState) -> Agent {
        let mut a = Agent::new(id, role, "do the thing");
        a.state = state;
        a
    }

    #[test]
    fn terminal_states_never_move() {
        for terminal in [AgentState::Done, AgentState::Failed, AgentState::Cancelled] {
            for to in [
                AgentState::Running,
                AgentState::Queued,
                AgentState::WaitingApproval,
                AgentState::Blocked,
            ] {
                assert!(!can_transition(terminal, to), "{terminal:?} -> {to:?} must be illegal");
            }
        }
    }

    #[test]
    fn happy_path_transitions_are_allowed() {
        let path = [
            AgentState::Queued,
            AgentState::Running,
            AgentState::WaitingApproval,
            AgentState::Running,
            AgentState::Done,
        ];
        let mut a = Agent::operator("x1-write", "write the parser");
        for next in &path[1..] {
            a.transition(*next).unwrap_or_else(|e| panic!("{e}"));
        }
        assert_eq!(a.state, AgentState::Done);
        assert_eq!(a.progress, 100, "terminal members show as complete");
    }

    #[test]
    fn illegal_transition_is_rejected_with_a_clear_error() {
        let mut a = agent("x1-write", AgentRole::Operator, AgentState::Done);
        let err = a.transition(AgentState::Running).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("x1-write") && msg.contains("done"), "{msg}");
        assert_eq!(a.state, AgentState::Done, "state must not change on error");
    }

    #[test]
    fn codenames_match_the_orchestrator() {
        assert_eq!(Agent::operator("x1-write", "x").id, "x1-write");
        assert_eq!(Agent::operator("x2-check", "x").id, "x2-check");
        assert_eq!(Agent::strategos("x").id, "Strategos");
        assert_eq!(slot_name(2), "slot-2");
        assert_eq!(Agent::slot(2, "x").id, "slot-2");
        assert_eq!(Agent::strategos("x").role.label(), "strategos");
        assert_eq!(Agent::operator("x1-write", "x").role.label(), "operator");
        // Every code the board can be handed is a real one, and the catalogue agrees.
        for code in phalanx::CODES.iter() {
            assert_eq!(Agent::operator(code.id, "x").id, code.id);
        }
    }

    #[test]
    fn the_strategos_row_is_pinned_to_the_top_whatever_the_state() {
        let mut m = Mission::new("refactor-auth", "stoic-codex");
        m.push(agent("x1-write", AgentRole::Operator, AgentState::WaitingApproval));
        m.push(agent("x2-check", AgentRole::Operator, AgentState::Failed));
        m.push(agent("Strategos", AgentRole::Strategos, AgentState::Running));
        let order: Vec<&str> = m.display_order().iter().map(|a| a.id.as_str()).collect();
        assert_eq!(order[0], "Strategos", "the boss row must not be displaced: {order:?}");
    }

    #[test]
    fn operators_sort_by_attention_then_codename() {
        let mut m = Mission::new("x", "y");
        m.push(agent("x1-write", AgentRole::Operator, AgentState::Done));
        m.push(agent("x2-check", AgentRole::Operator, AgentState::Running));
        m.push(agent("x3-ship", AgentRole::Operator, AgentState::WaitingApproval));
        m.push(agent("x0-core", AgentRole::Operator, AgentState::Blocked));
        let order: Vec<&str> = m.display_order().iter().map(|a| a.id.as_str()).collect();
        assert_eq!(order, vec!["x3-ship", "x0-core", "x2-check", "x1-write"]);
    }

    #[test]
    fn display_order_is_stable_across_calls() {
        let mut m = Mission::deploy("x", "y", 3, "task");
        for a in m.agents.iter_mut() {
            a.state = AgentState::Running;
        }
        let a: Vec<String> = m.display_order().iter().map(|x| x.id.clone()).collect();
        let b: Vec<String> = m.display_order().iter().map(|x| x.id.clone()).collect();
        assert_eq!(a, b);
        // Slots, because `deploy` builds a mission before Strategos has named anyone.
        assert_eq!(a, vec!["Strategos", "slot-1", "slot-2", "slot-3"]);
    }

    #[test]
    fn deploying_three_operators_builds_one_boss_and_three_rows() {
        let m = Mission::deploy("m", "repo", 3, "split the work");
        assert_eq!(m.agents.len(), 4);
        assert_eq!(m.boss().unwrap().id, "Strategos");
        assert_eq!(m.operators().len(), 3);
        assert_eq!(m.phalanx_section(), Some(1), "one boss row, then the operators");
        let s = m.summary();
        assert!(s.boss_present);
        assert_eq!(s.operators, 3);
    }

    #[test]
    fn pushing_a_second_strategos_replaces_the_first_instead_of_duplicating_it() {
        let mut m = Mission::deploy("m", "repo", 2, "task");
        m.push(Agent::strategos("reviewing"));
        assert_eq!(m.agents.len(), 3);
        assert_eq!(m.boss().unwrap().task, "reviewing");
        assert_eq!(m.operators().len(), 2);
    }

    #[test]
    fn budget_guard_trips_only_when_configured() {
        let mut m = Mission::new("x", "y");
        let mut a = agent("x1-write", AgentRole::Operator, AgentState::Running);
        a.micros = 5_000_000; // $5
        m.push(a);
        assert!(m.check_budget().is_ok(), "0 means unlimited");

        m.budget_micros = 1_000_000; // $1
        let err = m.check_budget().unwrap_err();
        assert!(err.to_string().contains("budget exceeded"), "{err}");
        assert!((m.budget_used_fraction() - 1.0).abs() < 1e-9, "fraction clamps at 1.0");
    }

    #[test]
    fn headline_prioritises_a_blocked_member_over_running_ones() {
        let mut m = Mission::deploy("x", "y", 2, "task");
        for a in m.agents.iter_mut() {
            a.state = AgentState::Running;
        }
        assert!(m.headline().contains("running"), "{}", m.headline());

        m.agents[2].state = AgentState::WaitingApproval;
        let h = m.headline();
        assert!(h.contains("needs approval"), "{h}");
        assert!(h.contains("slot-2"), "a blocked row must be named: {h}");
    }

    #[test]
    fn summary_counts_every_state_and_every_verdict() {
        let mut m = Mission::deploy("x", "y", 3, "task");
        for (i, st) in [
            AgentState::Queued,
            AgentState::Running,
            AgentState::WaitingApproval,
            AgentState::Blocked,
            AgentState::Done,
            AgentState::Failed,
            AgentState::Cancelled,
        ]
        .iter()
        .enumerate()
        {
            m.push(agent(&format!("x{}-probe", i + 10), AgentRole::Operator, *st));
        }
        m.agents[1].verdict = Verdict::Accepted;
        m.agents[2].verdict = Verdict::Rejected;
        m.agents[3].verdict = Verdict::Unreviewed;

        let sum = m.summary();
        assert_eq!(sum.operators, 10);
        assert_eq!(sum.settled(), 3);
        assert_eq!(sum.running, 1);
        assert_eq!(sum.accepted, 1);
        assert_eq!(sum.rejected, 1);
        assert_eq!(sum.unreviewed, 1);
    }

    #[test]
    fn the_activity_feed_keeps_the_newest_lines_and_is_capped() {
        let mut m = Mission::deploy("x", "y", 2, "task");
        assert!(m.activity.is_empty());
        for i in 0..(ACTIVITY_CAP + 25) {
            m.log(Activity::new(i as u64, ActivityKind::Tool, Some("x1-write"), format!("call {i}")));
        }
        assert_eq!(m.activity.len(), ACTIVITY_CAP, "the feed cannot grow without bound");
        assert_eq!(m.activity.last().unwrap().text, format!("call {}", ACTIVITY_CAP + 24));
        let tail = m.recent(3);
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[2].text, format!("call {}", ACTIVITY_CAP + 24), "newest is last");
    }

    #[test]
    fn recent_never_asks_for_more_than_exists() {
        let mut m = Mission::new("x", "y");
        assert!(m.recent(5).is_empty());
        m.log(Activity::new(0, ActivityKind::Plan, None::<String>, "planned"));
        assert_eq!(m.recent(5).len(), 1);
    }

    #[test]
    fn activity_times_are_minutes_and_seconds() {
        assert_eq!(Activity::new(0, ActivityKind::Tool, None::<String>, "x").time(), "00:00");
        assert_eq!(Activity::new(64, ActivityKind::Tool, None::<String>, "x").time(), "01:04");
        assert_eq!(Activity::new(3725, ActivityKind::Tool, None::<String>, "x").time(), "62:05");
    }

    #[test]
    fn only_conflicts_and_the_end_of_the_mission_are_loud() {
        let loud: Vec<ActivityKind> = [
            ActivityKind::Plan,
            ActivityKind::Deploy,
            ActivityKind::Tool,
            ActivityKind::Result,
            ActivityKind::Verdict,
            ActivityKind::Conflict,
            ActivityKind::Done,
        ]
        .into_iter()
        .filter(|k| k.loud())
        .collect();
        assert_eq!(loud, vec![ActivityKind::Conflict, ActivityKind::Done]);
    }

    #[test]
    fn naming_the_phalanx_keeps_a_rows_history_and_moves_its_branch() {
        let mut m = Mission::deploy("fix the parser", "stoic-codex", 2, "fix the parser");
        let ids: Vec<String> = m.operators().iter().map(|a| a.id.clone()).collect();
        assert_eq!(ids, vec!["slot-1".to_string(), "slot-2".to_string()]);

        // Work already in flight when the naming lands.
        if let Some(a) = m.agents.iter_mut().find(|a| a.id == "slot-1") {
            a.branch = Some("stoic-codex/slot-1".into());
            a.worktree = Some(".git/stoic-codex-missions/slot-1".into());
            a.input_tokens = 120;
            a.transition(AgentState::Running).unwrap();
        }

        let renames = vec![
            ("slot-1".to_string(), "x1-write".to_string()),
            ("slot-2".to_string(), "x2-check".to_string()),
        ];
        assert_eq!(m.name_phalanx(&renames), 2);

        let named = &m.operators()[0];
        assert_eq!(named.id, "x1-write");
        assert_eq!(named.state, AgentState::Running, "the row kept its state");
        assert_eq!(named.input_tokens, 120, "the row kept its spend");
        assert_eq!(named.branch.as_deref(), Some("stoic-codex/x1-write"));
        assert_eq!(
            named.worktree.as_deref(),
            Some(".git/stoic-codex-missions/x1-write")
        );
        assert_eq!(m.operators()[1].id, "x2-check");
        // Strategos is never renamed.
        assert_eq!(m.boss().unwrap().id, "Strategos");
    }

    #[test]
    fn a_rename_that_changes_nothing_is_not_counted() {
        let mut m = Mission::deploy("t", "stoic-codex", 1, "t");
        let same = vec![("slot-1".to_string(), "slot-1".to_string())];
        assert_eq!(m.name_phalanx(&same), 0);
        let unknown = vec![("slot-99".to_string(), "x1-write".to_string())];
        assert_eq!(m.name_phalanx(&unknown), 0);
    }

    #[test]
    fn the_naming_line_explains_the_code_it_prints() {
        let log = Mission::naming_log(
            7,
            &[("slot-1".to_string(), "x1-write".to_string())],
        );
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].codename.as_deref(), Some("x1-write"));
        assert_eq!(log[0].text, "slot-1 → x1-write · the primary logic generator");
        assert_eq!(log[0].time(), "00:07");
    }

    #[test]
    fn only_accepted_work_counts_as_landed() {
        let mut a = Agent::operator("x5-patch", "x");
        assert!(!a.landed(), "pending work has not landed");
        a.verdict = Verdict::Rejected;
        assert!(!a.landed());
        a.verdict = Verdict::Accepted;
        assert!(a.landed());
    }
}
