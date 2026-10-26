//! Mission board: Strategos on the top row, The Phalanx below, and the selected operator's
//! report on the right.
//!
//! Ordering comes from `stoic_codex_ui_core::mission::Mission::display_order`, which pins the
//! boss and is stable across redraws. The board is drawn without borders: a dim column
//! header, a red-tinted rule under the boss, one red selection bar, and the rest is
//! whitespace and grey.
//!
//! ```text
//!  CODE       BRANCH             CONTEXT      RESULT      ASSIGNMENT
//!  ◆ Strategos —                 ▰▰▰▰▰▰▰▰▰▰  done        plan + review
//!  ─── (red hairline) ────────────────────────────────────────────────────
//!    ✓ x1-write  stoic-codex/x1-write  ▰▰▰▰▰▰▰▰▰▰  accepted    retry backoff
//!  ▏ ▶ x7-trace  stoic-codex/x7-trace  ▰▰▰▰▰▰░░░░  running     key rotation
//!    ✗ x5-patch  stoic-codex/x5-patch  ▰▰▰░░░░░░░  rejected    error mapping
//! ```

use stoic_codex_ui_core::mission::{Activity, ActivityKind, Agent, AgentState, Mission, Verdict};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::fit;
use crate::chrome::{self, Chrome, Segment, TAB_MISSION};
use crate::theme as t;

pub struct MissionBoardState {
    pub mission: Mission,
    /// Selected row, index into `mission.display_order()`.
    pub selected: usize,
    /// Seconds of wall clock since this board appeared. Only the animation reads it.
    pub clock: f64,
    /// When the feed last grew, so the newest line can flash and fade.
    last_change: f64,
    /// How many entries the feed had last time it was drawn.
    seen: usize,
}

impl MissionBoardState {
    pub fn new(mission: Mission) -> Self {
        let seen = mission.activity.len();
        MissionBoardState { mission, selected: 0, clock: 0.0, last_change: -10.0, seen }
    }

    /// Advance the board's clock. `dt` in seconds, clamped: a laptop that was closed for an
    /// hour must not produce a spinner that lands in one phase and stays there.
    pub fn tick(&mut self, dt: f64) {
        if !dt.is_finite() {
            return;
        }
        self.clock += dt.clamp(0.0, 0.25);
        // A new feed entry restarts the flash. `flash()` decays from here.
        let now = self.mission.activity.len();
        if now > self.seen {
            self.last_change = self.clock;
        }
        self.seen = now;
    }

    /// How bright the newest activity line should be, `1.0` → `0.0` over 0.8 s.
    ///
    /// This is the whole animation budget of the feed: one line, briefly brighter. Anything
    /// more competes with the mission board for attention while the user is reading it.
    pub fn flash(&self) -> f64 {
        (1.0 - (self.clock - self.last_change) / 0.8).clamp(0.0, 1.0)
    }

    pub fn ordered(&self) -> Vec<&Agent> {
        self.mission.display_order()
    }

    pub fn selected_agent(&self) -> Option<&Agent> {
        self.ordered().get(self.selected).copied()
    }

    pub fn down(&mut self) {
        let n = self.mission.agents.len();
        if n > 0 {
            self.selected = (self.selected + 1) % n;
        }
    }

    pub fn up(&mut self) {
        let n = self.mission.agents.len();
        if n > 0 {
            self.selected = if self.selected == 0 { n - 1 } else { self.selected - 1 };
        }
    }

    /// Keep the cursor on a row that still exists after the mission changes shape.
    pub fn clamp(&mut self) {
        let n = self.mission.agents.len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }
}

/// The colour a state is allowed to use. Running and selected are red; accepted is the one
/// jade glyph on the screen; a conflict is amber; everything settled quietly is grey.
fn state_style(state: AgentState) -> Style {
    match state {
        AgentState::Running => t::accent(),
        AgentState::WaitingApproval | AgentState::Blocked => t::warn(),
        AgentState::Failed => t::bad(),
        AgentState::Done => t::muted(),
        AgentState::Queued | AgentState::Cancelled => t::faint(),
    }
}

/// The verdict is a separate axis from the state — "it finished" and "its work was kept" are
/// different facts, and a board that blurs them is lying.
fn verdict_style(verdict: Verdict) -> Style {
    match verdict {
        Verdict::Accepted => t::good(),
        Verdict::Rejected => t::bad(),
        Verdict::Unreviewed => t::warn(),
        Verdict::Pending => t::faint(),
    }
}

fn verdict_label(agent: &Agent) -> &'static str {
    match agent.verdict {
        Verdict::Pending => agent.state.label(),
        other => other.label(),
    }
}

/// A slim meter. Blocks rather than a Gauge widget: a Gauge needs a whole line and a
/// background, and one line is all this row can spare.
///
/// The columns mean *context window used*, not "job done" — an agent does not know how much
/// of its task remains, but the loop does know how full the window is, and that is the number
/// that predicts a run about to lose its earlier work. Red counts *down*: it is the budget,
/// grey is what has been spent, so a nearly-full window looks nearly empty and alarming.
fn meter(pct: u8, width: usize) -> String {
    let width = width.max(3);
    let filled = (pct as usize * width) / 100;
    let mut s = String::with_capacity(width);
    for i in 0..width {
        s.push(if i < filled { '▰' } else { '▱' });
    }
    s
}

/// Column plan for the board pane.
///
/// Computed once, from the pane width, and used by *both* the column header and the rows —
/// which is the only reliable way to keep a table aligned. Alignment is by construction
/// here (see [`push_at`]), not by matching `format!` widths in two places.
struct Columns {
    /// First column of the member name.
    member: usize,
    branch: usize,
    bar: usize,
    result: usize,
    assignment: usize,
    member_width: usize,
    branch_width: usize,
    bar_width: usize,
    show_branch: bool,
    show_bar: bool,
}

impl Columns {
    fn for_width(width: usize) -> Columns {
        let member = 4; // cursor + glyph + two spaces
        let member_width = 13;
        let show_branch = width >= 68;
        let show_bar = width >= 46;
        let branch_width = 20;
        let bar_width = 10;
        let result_width = 10;

        let mut col = member + member_width + 1;
        let branch = col;
        if show_branch {
            col += branch_width + 1;
        }
        let bar = col;
        if show_bar {
            col += bar_width + 1;
        }
        let result = col;
        col += result_width + 1;
        // A pane narrower than the plan keeps its left columns and drops the rest, rather
        // than writing past its own edge.
        let assignment = if col < width { col } else { width };
        let member_width = member_width.min(width.saturating_sub(member));
        Columns {
            member,
            branch,
            bar,
            result,
            assignment,
            member_width,
            branch_width,
            bar_width,
            show_branch,
            show_bar,
        }
    }
}

/// Append `text` so that it starts at column `target`, padding with spaces if needed.
///
/// Returns the new column. Everything on the board is laid out with this, which is why a
/// header cell and a data cell can never drift apart.
fn push_at(
    spans: &mut Vec<Span<'static>>,
    col: &mut usize,
    target: usize,
    text: &str,
    style: Style,
    limit: usize,
) {
    // A row can never write past the pane it was given: pad to the column if there is room,
    // then write whatever still fits.
    let target = target.max(*col).min(limit);
    if target > *col {
        spans.push(Span::styled(" ".repeat(target - *col), Style::default()));
        *col = target;
    }
    let room = limit.saturating_sub(*col);
    if room == 0 {
        return;
    }
    let text = fit(text, room);
    *col += text.chars().count();
    spans.push(Span::styled(text, style));
}

/// The dim column header. Uppercase and faint on purpose: it is a legend, not content.
fn column_header(width: usize) -> Line<'static> {
    let w = width;
    let c = Columns::for_width(w);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut col = 0usize;
    push_at(&mut spans, &mut col, c.member, "CODE", t::faint(), w);
    if c.show_branch {
        push_at(&mut spans, &mut col, c.branch, "BRANCH", t::faint(), w);
    }
    if c.show_bar {
        push_at(&mut spans, &mut col, c.bar, "CONTEXT", t::faint(), w);
    }
    push_at(&mut spans, &mut col, c.result, "RESULT", t::faint(), w);
    push_at(&mut spans, &mut col, c.assignment, "ASSIGNMENT", t::faint(), w);
    Line::from(spans)
}

/// One member's row. Adapts by dropping columns, never by wrapping.
/// Spinner frames for a running operator.
///
/// Braille, because it is the same glyph family the black hole screen draws with, and because
/// a block spinner next to a braille meter reads as two different renderers.
const SPIN: [char; 8] = ['\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}'];

/// The glyph shown where an operator's state goes: a spinner while it works, the settled glyph
/// afterwards. Ten frames a second — fast enough to read as motion, slow enough not to strobe.
fn state_glyph(state: AgentState, clock: f64) -> char {
    if state == AgentState::Running {
        let frame = (clock * 10.0).rem_euclid(SPIN.len() as f64) as usize;
        return SPIN[frame.min(SPIN.len() - 1)];
    }
    state.glyph()
}

pub fn agent_row(agent: &Agent, width: u16, selected: bool) -> Line<'static> {
    agent_row_at(agent, width, selected, 0.0)
}

/// The row as it looks at a given moment: identical to [`agent_row`] except that a running
/// operator's state glyph advances with the clock.
pub fn agent_row_at(agent: &Agent, width: u16, selected: bool, clock: f64) -> Line<'static> {
    let w = width as usize;
    let c = Columns::for_width(w);
    let boss = agent.role.is_boss();
    let glyph_style = if boss { t::accent() } else { state_style(agent.state) };

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut col = 0usize;
    // 0: cursor bar (the one red thing on a settled row), 2: state glyph.
    let marker_style = if selected { t::accent() } else { t::faint() };
    push_at(&mut spans, &mut col, 0, if selected { "▏" } else { " " }, marker_style, w);
    push_at(
        &mut spans,
        &mut col,
        2,
        &format!("{}", state_glyph(agent.state, clock)),
        glyph_style.add_modifier(Modifier::BOLD),
        w,
    );
    push_at(&mut spans, &mut col, c.member, &fit(&agent.id, c.member_width), if boss { t::focus() } else { t::text() }, w);

    if c.show_branch {
        let branch = agent
            .branch
            .clone()
            .unwrap_or_else(|| if boss { "—".into() } else { "(no worktree)".into() });
        let style = if agent.branch.is_some() { t::muted() } else { t::faint() };
        push_at(&mut spans, &mut col, c.branch, &fit(&branch, c.branch_width), style, w);
    }
    if c.show_bar {
        push_at(
            &mut spans,
            &mut col,
            c.bar,
            &meter(agent.progress, c.bar_width),
            if agent.progress == 0 { t::faint() } else { glyph_style },
            w,
        );
    }
    push_at(
        &mut spans,
        &mut col,
        c.result,
        &fit(verdict_label(agent), 10),
        verdict_style(agent.verdict),
        w,
    );

    // The assignment column is clipped and its tail ellipsised at the pane's edge, so the
    // rows form one straight right margin instead of ragged ends.
    let task_w = w.saturating_sub(c.assignment);
    if task_w >= 4 {
        push_at(&mut spans, &mut col, c.assignment, &fit(&agent.task, task_w), t::muted(), w);
    }
    Line::from(spans)
}

fn activity_style(kind: ActivityKind) -> Style {
    match kind {
        ActivityKind::Conflict => t::warn(),
        ActivityKind::Done => t::accent(),
        ActivityKind::Verdict => t::muted(),
        _ => t::faint(),
    }
}

/// The feed under the table: what is happening right now, newest at the bottom.
///
/// A table tells you the shape of a mission; the feed tells you whether it is alive. It gets
/// whatever height is left after the table has asked for its rows, and it never takes a row
/// the table needs.
fn render_activity(frame: &mut Frame, area: Rect, state: &MissionBoardState) {
    if area.height == 0 {
        return;
    }
    let width = area.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(" ACTIVITY", t::faint())));

    let room = area.height.saturating_sub(1) as usize;
    let tail: &[Activity] = state.mission.recent(room);
    if tail.is_empty() {
        lines.push(Line::from(Span::styled(
            "  nothing yet — the feed starts when Strategos plans",
            t::faint(),
        )));
    }
    let flash = state.flash();
    let newest = tail.len();
    for (index, entry) in tail.iter().enumerate() {
        // The freshest line is bright for a moment and then joins the rest: motion that says
        // "this is live" without moving anything the eye is trying to read.
        let fresh = flash > 0.05 && index + 1 == newest;
        let style = if fresh { t::accent() } else { activity_style(entry.kind) };
        let mut spans = vec![
            Span::styled(format!(" {} ", entry.time()), t::faint()),
            Span::styled(format!("{} ", entry.kind.glyph()), style),
        ];
        if let Some(codename) = &entry.codename {
            spans.push(Span::styled(format!("{:<12}", fit(codename, 12)), t::muted()));
        } else {
            spans.push(Span::styled(" ".repeat(12), Style::default()));
        }
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        spans.push(Span::styled(
            fit(&entry.text, width.saturating_sub(used + 1)),
            if entry.kind.loud() { style } else { t::muted() },
        ));
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// The red-tinted rule that separates the boss from the agents it commands.
fn divider(width: u16) -> Line<'static> {
    let n = width.saturating_sub(2) as usize;
    Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled("─".repeat(n), t::rule_accent()),
    ])
}

/// The board pane: header, rows, and one rule under the boss.
fn render_board(frame: &mut Frame, area: Rect, state: &MissionBoardState) {
    let width = area.width;
    let section = state.mission.phalanx_section();
    let mut lines: Vec<Line> = vec![column_header(width as usize)];
    for (i, agent) in state.ordered().iter().enumerate() {
        if Some(i) == section {
            lines.push(divider(width));
        }
        lines.push(agent_row_at(agent, width, i == state.selected, state.clock));
    }
    if state.mission.agents.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  no mission yet — `stoic-codex mission run <task>` deploys one",
            t::faint(),
        )));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// `21.4k` rather than `21400`: three significant figures is all a glance needs.
fn human(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    let k = n as f64 / 1000.0;
    if k < 10.0 {
        format!("{k:.1}k")
    } else {
        format!("{:.0}k", k)
    }
}

fn field(label: &str, value: &str, value_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::styled(format!("{:<11}", label), t::label()),
        Span::styled(value.to_string(), value_style),
    ])
}

/// The right pane: what the selected member is doing, on what branch, with what verdict.
pub fn render_detail(frame: &mut Frame, area: Rect, state: &MissionBoardState) {
    let Some(a) = state.selected_agent() else {
        frame.render_widget(Paragraph::new(""), area);
        return;
    };
    let boss = a.role.is_boss();
    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(
                format!(" {}  ", a.state.glyph()),
                if boss { t::accent() } else { state_style(a.state) }.add_modifier(Modifier::BOLD),
            ),
            Span::styled(a.id.clone(), t::title()),
        ]),
        Line::from(Span::styled(
            format!(" {}", "─".repeat(area.width.saturating_sub(3) as usize)),
            t::rule(),
        )),
        Line::from(""),
        field("assignment", &fit(&a.task, area.width.saturating_sub(14) as usize), t::text()),
    ];

    if boss {
        lines.push(field("tools", "none — it plans and reviews", t::muted()));
        lines.push(field("role", "commander of the Operator", t::muted()));
    } else {
        lines.push(field("branch", a.branch.as_deref().unwrap_or("—"), t::accent()));
        lines.push(field("worktree", a.worktree.as_deref().unwrap_or("—"), t::muted()));
        lines.push(field(
            "verdict",
            a.verdict.label(),
            verdict_style(a.verdict),
        ));
    }

    let tokens = if a.input_tokens + a.output_tokens == 0 {
        "—".to_string()
    } else {
        format!("{} in / {} out", human(a.input_tokens), human(a.output_tokens))
    };
    lines.push(field("tokens", &tokens, t::muted()));
    if !boss {
        lines.push(field("context", &format!("{}% of the window", a.progress), t::muted()));
    }
    if a.elapsed_secs > 0 {
        let secs = a.elapsed_secs;
        let elapsed = if secs >= 60 {
            format!("{}m{:02}s", secs / 60, secs % 60)
        } else {
            format!("{secs}s")
        };
        lines.push(field("elapsed", &elapsed, t::muted()));
    }
    if let Some(reason) = &a.reason {
        lines.push(Line::from(""));
        lines.push(field(
            "why",
            &fit(reason, area.width.saturating_sub(14) as usize),
            if a.landed() { t::good() } else { t::warn() },
        ));
    }

    // The foot of the pane: the mission itself, so the right-hand side is never a blank
    // rectangle. These are the four numbers a commander actually reads.
    let s = state.mission.summary();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(" {}", "─".repeat(area.width.saturating_sub(3) as usize)),
        t::rule(),
    )));
    lines.push(field("base", &state.mission.base, t::muted()));
    lines.push(field(
        "plan",
        &format!("{} assignments", state.mission.agents.len().saturating_sub(1)),
        t::muted(),
    ));
    lines.push(field(
        "applied",
        &format!("{} accepted · {} rejected", s.accepted, s.rejected),
        if s.rejected > 0 { t::warn() } else { t::good() },
    ));
    let budget = if state.mission.budget_micros == 0 {
        format!("${:.2} (no cap)", s.spent_usd())
    } else {
        format!(
            "${:.2} of ${:.2}",
            s.spent_usd(),
            state.mission.budget_micros as f64 / 1_000_000.0
        )
    };
    lines.push(field("budget", &budget, t::muted()));

    frame.render_widget(Paragraph::new(lines), area);
}

/// The board pane: the table, then the activity feed in whatever is left.
fn render_board_with_feed(frame: &mut Frame, area: Rect, state: &MissionBoardState) {
    if area.height <= 1 {
        render_board(frame, area, state);
        return;
    }
    // The table asks for exactly the rows it needs (header + members + the boss rule), and
    // never for more than half the pane: the feed is not an afterthought.
    let wanted = (state.mission.agents.len() + 2).min((area.height as usize).saturating_sub(3)) as u16;
    let table_h = wanted.min(area.height / 2 + 1).max(1);
    let chunks = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            Constraint::Length(table_h),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);
    render_board(frame, chunks[0], state);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("─".repeat(area.width as usize), t::rule()))),
        chunks[1],
    );
    if chunks[2].height > 0 {
        render_activity(frame, chunks[2], state);
    }
}

/// The body: board on the left, a hairline, the report on the right.
pub fn render(frame: &mut Frame, area: Rect, state: &MissionBoardState) {
    // Below ~90 columns the report pane has nothing useful to say, so the board takes it all.
    let (board, divider_area, detail) = if area.width >= 90 {
        let chunks = Layout::default()
            .direction(ratatui::layout::Direction::Horizontal)
            .constraints([
                Constraint::Percentage(62),
                Constraint::Length(1),
                Constraint::Min(28),
            ])
            .split(area);
        (Some(chunks[0]), Some(chunks[1]), Some(chunks[2]))
    } else {
        (Some(area), None, None)
    };

    if let Some(a) = board {
        render_board_with_feed(frame, a, state);
    }
    if let Some(d) = divider_area {
        frame.render_widget(
            Block::default().borders(Borders::LEFT).border_style(t::rule()),
            d,
        );
    }
    if let Some(d) = detail {
        render_detail(frame, d, state);
    }
}

/// Whole-screen version: the shared frame, then the board. Used by the screenshot tool and
/// by the app shell once the TUI is wired to the orchestrator.
pub fn render_screen(frame: &mut Frame, area: Rect, state: &MissionBoardState) {
    let s = state.mission.summary();
    let running = if s.running > 0 {
        Segment::new(format!("● {} running", s.running), t::accent())
    } else {
        Segment::new("● idle", t::faint())
    };
    let chrome = Chrome::new(state.mission.base.clone(), &chrome::TABS, TAB_MISSION)
        .with_branch_status()
        .status(running)
        .status(Segment::new(format!("${:.2}", s.spent_usd()), t::muted()))
        .footer(Segment::chip(format!("● {} operators", s.operators)))
        .footer(Segment::chip(format!("{} accepted", s.accepted)))
        .hint("↑↓ select · enter detail · ? help · q quit");

    let body = chrome::frame(frame, area, &chrome);
    render(frame, body, state);
}

/// Highest number of rows the board can show at a given height, for scrolling logic.
pub fn visible_rows(height: u16) -> usize {
    height.saturating_sub(2) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use stoic_codex_ui_core::mission::{AgentRole, Mission};

    fn agent(role: AgentRole, state: AgentState, progress: u8) -> Agent {
        let mut a = Agent::new("x1-write", role, "task");
        a.state = state;
        a.progress = progress;
        a.branch = Some("stoic-codex/x1-write".into());
        a.micros = 420_000;
        a
    }

    fn mission() -> Mission {
        let mut m = Mission::deploy("refactor-auth", "repo", 3, "split the work");
        for a in m.agents.iter_mut() {
            a.state = AgentState::Running;
        }
        m
    }

    fn text_of(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn the_context_meter_is_always_the_same_width() {
        for pct in [0u8, 1, 33, 50, 99, 100] {
            assert_eq!(meter(pct, 10).chars().count(), 10, "pct {pct}");
        }
        assert!(meter(0, 10).chars().all(|c| c == '▱'));
        assert!(meter(100, 10).chars().all(|c| c == '▰'));
    }

    #[test]
    fn selection_wraps_over_every_row_including_the_boss() {
        let m = mission();
        let n = m.agents.len();
        let mut st = MissionBoardState::new(m);
        assert_eq!(st.selected, 0);
        st.up();
        assert_eq!(st.selected, n - 1, "up from 0 wraps to last");
        st.down();
        assert_eq!(st.selected, 0);
    }

    #[test]
    fn the_first_row_is_always_strategos() {
        let mut m = mission();
        // A Operator that needs a human must not jump above the boss.
        m.agents[2].state = AgentState::WaitingApproval;
        let st = MissionBoardState::new(m);
        assert_eq!(st.ordered()[0].id, "Strategos");
        assert_eq!(st.selected_agent().unwrap().id, "Strategos");
    }

    #[test]
    fn the_row_layout_lines_up_under_its_own_column_header() {
        // The header and the rows are generated by the same column plan, so a row's branch
        // must start exactly where the word BRANCH starts.
        for width in [120u16, 100, 70] {
            let a = agent(AgentRole::Operator, AgentState::Running, 62);
            let header = text_of(&column_header(width as usize));
            let row = text_of(&agent_row(&a, width, false));
            if width >= 68 {
                let chars: Vec<char> = header.chars().collect();
                let needle: Vec<char> = "BRANCH".chars().collect();
                let col = chars
                    .windows(needle.len())
                    .position(|w| w == needle.as_slice())
                    .expect("branch column");
                // Char positions, not byte offsets.
                let row_chars: Vec<char> = row.chars().collect();
                let at: String = row_chars[col.min(row_chars.len())..].iter().skip_while(|c| **c == ' ').collect();
                assert!(
                    at.starts_with("stoic-codex/x1-write"),
                    "width {width}: expected the branch at col {col}, got {at:?}"
                );
            }
        }
    }

    #[test]
    fn rows_never_exceed_the_width_they_are_given() {
        let a = agent(AgentRole::Operator, AgentState::Running, 62);
        for width in [24u16, 40, 52, 60, 80, 100, 160] {
            let line = agent_row(&a, width, false);
            assert!(
                line.width() <= width as usize,
                "width {width}: row is {} wide",
                line.width()
            );
        }
    }

    #[test]
    fn the_boss_row_carries_no_branch_and_the_operators_do() {
        let mut boss = Agent::strategos("plan and review");
        boss.state = AgentState::Running;
        let boss_row = text_of(&agent_row(&boss, 120, false));
        assert!(boss_row.contains("Strategos"), "{boss_row}");
        assert!(!boss_row.contains("stoic-codex/"), "the boss has no branch: {boss_row}");

        let mut fed = agent(AgentRole::Operator, AgentState::Done, 100);
        fed.verdict = Verdict::Accepted;
        let fed_row = text_of(&agent_row(&fed, 120, false));
        assert!(fed_row.contains("stoic-codex/x1-write"), "{fed_row}");
        assert!(fed_row.contains("accepted"), "{fed_row}");
    }

    #[test]
    fn a_settled_row_has_exactly_one_red_thing_and_it_is_the_cursor() {
        let mut a = agent(AgentRole::Operator, AgentState::Done, 100);
        a.verdict = Verdict::Accepted;
        let line = agent_row(&a, 120, true);
        let red: Vec<&str> = line
            .spans
            .iter()
            .filter(|s| s.style.fg == Some(t::RED))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(red, vec!["▏"], "only the selection bar may be red on a settled row");
    }

    #[test]
    fn a_running_row_spends_its_red_on_the_glyph_and_the_bar() {
        // Red means "live" here, so a running row is allowed exactly three: cursor, glyph,
        // progress. Nothing else on the row competes.
        let line = agent_row(&agent(AgentRole::Operator, AgentState::Running, 40), 120, true);
        let red: Vec<String> = line
            .spans
            .iter()
            .filter(|s| s.style.fg == Some(t::RED))
            .map(|s| s.content.to_string())
            .collect();
        // The glyph is a spinner frame now, so compare the shape of the row rather than a
        // fixed character: cursor, exactly one braille spinner frame, the meter.
        assert_eq!(red.len(), 3, "{red:?}");
        assert_eq!(red[0], "▏");
        assert_eq!(red[1].chars().count(), 1, "one cell: {:?}", red[1]);
        assert!(red[1].chars().next().unwrap() as u32 >= 0x2800, "braille spinner: {:?}", red[1]);
        assert!(red[1].chars().next().unwrap() <= '\u{28ff}');
        assert_eq!(red[2], "▰▰▰▰▱▱▱▱▱▱");
    }

    #[test]
    fn an_empty_mission_says_what_to_type_instead_of_nothing() {
        let area = Rect { x: 0, y: 0, width: 60, height: 6 };
        let st = MissionBoardState::new(Mission::new("m", "repo"));
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 6)).unwrap();
        terminal
            .draw(|f| {
                render_board(f, area, &st);
            })
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("stoic-codex mission run"), "{screen}");
    }

    #[test]
    fn the_palette_rule_holds_on_the_board() {
        // Only three hues may appear on this screen: red, jade and amber, plus greys.
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 20)).unwrap();
        terminal
            .draw(|f| {
                render_screen(f, f.area(), &MissionBoardState::new(mission()));
            })
            .unwrap();
        let mut hues = std::collections::BTreeSet::new();
        for cell in terminal.backend().buffer().content() {
            if let ratatui::style::Color::Rgb(r, g, b) = cell.fg {
                hues.insert((r, g, b));
            }
        }
        // red, deep-ish greys, bone, jade/amber — never a fourth accent.
        assert!(hues.len() <= 8, "too many colours on one screen: {hues:?}");
    }

    #[test]
    fn clamping_keeps_the_cursor_inside_a_shrinking_mission() {
        let mut st = MissionBoardState::new(mission());
        st.selected = 3;
        st.mission.agents.truncate(2);
        st.clamp();
        assert_eq!(st.selected, 1);
        st.mission.agents.clear();
        st.clamp();
        assert_eq!(st.selected, 0);
    }

    #[test]
    fn visible_rows_accounts_for_the_column_header_and_the_divider() {
        assert_eq!(visible_rows(10), 8);
        assert_eq!(visible_rows(2), 0);
    }

    #[test]
    fn a_running_operator_spins_and_a_settled_one_does_not() {
        let st = MissionBoardState::new(mission());
        // Ten frames a second, so a tenth of a second later the glyph has moved on.
        let early = state_glyph(AgentState::Running, 0.0);
        let later = state_glyph(AgentState::Running, 0.15);
        assert_ne!(early, later, "the spinner must advance with the clock");
        assert!((early as u32) >= 0x2800 && (later as u32) <= 0x28ff, "braille spinner frames");

        // A settled operator never spins, whatever the clock says.
        assert_eq!(state_glyph(AgentState::Done, 12.34), AgentState::Done.glyph());
        assert_eq!(state_glyph(AgentState::Failed, 12.34), AgentState::Failed.glyph());
        assert_eq!(state_glyph(AgentState::Blocked, 12.34), AgentState::Blocked.glyph());

        // And the row really carries it, rather than the glyph living only in this test.
        let running = st
            .mission
            .agents
            .iter()
            .find(|a| a.state == AgentState::Running)
            .expect("the fixture has a running operator");
        assert!(agent_row_at(running, 120, false, 0.0).to_string().contains(early));
        assert!(!agent_row_at(running, 120, false, 0.15).to_string().contains(early));
    }

    #[test]
    fn the_clock_advances_and_is_clamped() {
        let mut st = MissionBoardState::new(mission());
        st.tick(0.1);
        assert!((st.clock - 0.1).abs() < 1e-9);
        st.tick(9.0); // a suspended laptop
        assert!((st.clock - 0.35).abs() < 1e-9, "dt is clamped to a quarter second: {}", st.clock);
        st.tick(f64::NAN); // must not poison the clock
        assert!(st.clock.is_finite());
    }

    #[test]
    fn a_new_feed_entry_flashes_and_then_fades() {
        let mut st = MissionBoardState::new(mission());
        assert_eq!(st.flash(), 0.0, "nothing has happened yet");
        st.mission.log(Activity::new(40, ActivityKind::Tool, Some("x1-write"), "reading"));
        st.tick(0.1);
        assert!(st.flash() > 0.8, "a new entry is bright: {}", st.flash());
        for _ in 0..4 {
            st.tick(0.25);
        }
        assert_eq!(st.flash(), 0.0, "and it fades within a second");
    }
}
