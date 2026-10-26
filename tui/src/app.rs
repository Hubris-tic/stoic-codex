//! Terminal app shells for the run chat and the live mission board.
//!
//! The views stay pure Ratatui renderers; this module owns terminal mode, the event loop,
//! streaming sinks, and the `MissionEvent` → board-state reducer. Nothing here opens a
//! provider connection.

use std::io::{self, Write};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use stoic_codex_agent::{
    Event as AgentEvent, MissionEvent, OperatorState, Verdict as AgentVerdict,
};
use stoic_codex_ui_core::mission::{
    can_transition, Activity, ActivityKind, AgentState, Mission, Verdict,
};

use crate::theme as t;
use crate::views::help::{self, HelpContext};
use crate::views::mission_board::{self, MissionBoardState};

/// Keep the visible transcript bounded; older turns remain in the CLI's configured session.
const CHAT_MESSAGE_LIMIT: usize = 200;

/// Which CLI stream a chat writer represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatStream {
    /// Model response deltas, shown as the current assistant message.
    Assistant,
    /// CLI diagnostics and final provider/accounting metadata.
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChatMessage {
    assistant: bool,
    body: String,
}

#[derive(Debug)]
struct ChatState {
    title: String,
    messages: Vec<ChatMessage>,
    current: Vec<u8>,
    status: Vec<u8>,
    status_label: String,
    input: String,
    busy: bool,
    /// `None` follows the live tail; `Some` is a manual scroll offset from the top.
    scroll: Option<usize>,
    help: bool,
}

impl ChatState {
    fn new(title: String) -> ChatState {
        ChatState {
            title,
            messages: Vec::new(),
            current: Vec::new(),
            status: Vec::new(),
            status_label: "ready".into(),
            input: String::new(),
            busy: false,
            scroll: None,
            help: false,
        }
    }
}

struct ChatRuntime {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    state: ChatState,
}

impl ChatRuntime {
    fn draw(&mut self) -> io::Result<()> {
        self.terminal
            .draw(|frame| render_chat(frame, &self.state))
            .map(|_| ())
    }
}

/// A cloneable `Write` sink which paints streamed run output into the chat screen.
pub struct ChatWriter {
    runtime: Arc<Mutex<ChatRuntime>>,
    stream: ChatStream,
}

impl Write for ChatWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut runtime = lock(&self.runtime);
        match self.stream {
            ChatStream::Assistant => {
                runtime.state.current.extend_from_slice(bytes);
                runtime.state.scroll = None;
            }
            ChatStream::Status => runtime.state.status.extend_from_slice(bytes),
        }
        runtime.draw()?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        lock(&self.runtime).draw()
    }
}

/// Full-screen chat UI for an interactive `stoic-codex run` session.
///
/// The initial task is submitted by the CLI; after each response, [`next_prompt`](Self::next_prompt)
/// keeps an input line anchored to the bottom and returns follow-up turns to the caller.
pub struct ChatTui {
    runtime: Arc<Mutex<ChatRuntime>>,
    _terminal_guard: TerminalGuard,
}

impl ChatTui {
    pub fn enter(title: impl Into<String>) -> io::Result<ChatTui> {
        let terminal_guard = TerminalGuard::enter()?;
        let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        let runtime = Arc::new(Mutex::new(ChatRuntime {
            terminal,
            state: ChatState::new(title.into()),
        }));
        lock(&runtime).draw()?;
        Ok(ChatTui {
            runtime,
            _terminal_guard: terminal_guard,
        })
    }

    pub fn writer(&self, stream: ChatStream) -> ChatWriter {
        ChatWriter {
            runtime: self.runtime.clone(),
            stream,
        }
    }

    /// Add the submitted user turn and switch the footer to its streaming state.
    pub fn begin_turn(&mut self, prompt: &str) -> io::Result<()> {
        let mut runtime = lock(&self.runtime);
        runtime.state.messages.push(ChatMessage {
            assistant: false,
            body: prompt.to_string(),
        });
        trim_chat_history(&mut runtime.state.messages);
        runtime.state.current.clear();
        runtime.state.status.clear();
        runtime.state.status_label = "thinking".into();
        runtime.state.input.clear();
        runtime.state.busy = true;
        runtime.state.scroll = None;
        runtime.draw()
    }

    /// Close the live response and make the screen ready for another turn.
    pub fn finish_turn(&mut self, exit_code: i32) -> io::Result<()> {
        let mut runtime = lock(&self.runtime);
        if !runtime.state.current.is_empty() {
            let body = String::from_utf8_lossy(&runtime.state.current).into_owned();
            runtime.state.messages.push(ChatMessage {
                assistant: true,
                body,
            });
            trim_chat_history(&mut runtime.state.messages);
            runtime.state.current.clear();
        }
        runtime.state.busy = false;
        runtime.state.status_label = if exit_code == 0 {
            "complete".into()
        } else {
            "failed".into()
        };
        runtime.state.scroll = None;
        runtime.draw()
    }

    /// Wait for one follow-up prompt. Esc or Ctrl-C exits; Enter submits; PgUp/PgDn scroll.
    pub fn next_prompt(&mut self) -> io::Result<Option<String>> {
        loop {
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            match event::read()? {
                TerminalEvent::Key(key) if is_key_press(key) => {
                    if let Some(prompt) = self.handle_key(key)? {
                        return Ok(prompt);
                    }
                }
                TerminalEvent::Paste(text) => {
                    let mut runtime = lock(&self.runtime);
                    runtime
                        .state
                        .input
                        .push_str(&text.replace(['\r', '\n'], " ").replace('\t', "    "));
                    runtime.state.help = false;
                    runtime.draw()?;
                }
                TerminalEvent::Resize(_, _) => lock(&self.runtime).draw()?,
                _ => {}
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> io::Result<Option<Option<String>>> {
        let mut runtime = lock(&self.runtime);
        let terminal_size = runtime.terminal.size()?;
        let state = &mut runtime.state;
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl_c {
            return Ok(Some(None));
        }
        if key.code == KeyCode::Esc {
            if state.help {
                state.help = false;
                runtime.draw()?;
                return Ok(None);
            }
            return Ok(Some(None));
        }
        if key.code == KeyCode::F(1) || (key.code == KeyCode::Char('?') && state.input.is_empty()) {
            state.help = !state.help;
            runtime.draw()?;
            return Ok(None);
        }
        match key.code {
            KeyCode::Enter => {
                let prompt = std::mem::take(&mut state.input);
                if prompt.trim().is_empty() {
                    runtime.draw()?;
                    return Ok(None);
                }
                state.help = false;
                runtime.draw()?;
                return Ok(Some(Some(prompt)));
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                state.input.push(c);
                state.help = false;
            }
            KeyCode::Backspace => {
                state.input.pop();
            }
            KeyCode::PageUp | KeyCode::Up | KeyCode::PageDown | KeyCode::Down => {
                let width = terminal_size.width.min(108).saturating_sub(2).max(1);
                let visible = terminal_size.height.saturating_sub(3) as usize;
                let step = match key.code {
                    KeyCode::PageUp => -12,
                    KeyCode::Up => -3,
                    KeyCode::PageDown => 12,
                    KeyCode::Down => 3,
                    _ => 0,
                };
                state.scroll = Some(chat_scroll(
                    &state.messages,
                    &state.current,
                    state.scroll,
                    width,
                    visible,
                    step,
                ));
            }
            KeyCode::End => state.scroll = None,
            _ => {}
        }
        runtime.draw()?;
        Ok(None)
    }
}

/// Final operator data, without copying raw model transcripts into the UI event channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionUiOperator {
    pub codename: String,
    pub task: String,
    pub state: OperatorState,
    pub verdict: AgentVerdict,
    pub reason: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub elapsed_ms: u64,
}

/// Small final snapshot used to reconcile rows if an early worker return skipped an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionUiFinal {
    pub outcome: String,
    pub operators: Vec<MissionUiOperator>,
    pub applied: usize,
    pub conflicts: usize,
}

/// Events delivered from the mission worker to the Ratatui app loop.
#[derive(Debug, Clone)]
pub enum MissionUiEvent {
    Mission(MissionEvent),
    Notice(String),
    Finished(Result<MissionUiFinal, String>),
}

/// Live, event-driven Ratatui mission board.
pub struct MissionTui {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    state: MissionBoardState,
    started: Instant,
    help: bool,
    detail_open: bool,
    worker_finished: bool,
    _terminal_guard: TerminalGuard,
}

impl MissionTui {
    /// Create the initial Strategos + Phalanx rows before the worker starts emitting events.
    pub fn enter(
        task: &str,
        repo: impl Into<String>,
        operators: usize,
        model: &str,
    ) -> io::Result<MissionTui> {
        let terminal_guard = TerminalGuard::enter()?;
        let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        let mut mission = Mission::deploy(task, repo, operators, task);
        for agent in &mut mission.agents {
            agent.model = model.to_string();
        }
        let mut tui = MissionTui {
            terminal,
            state: MissionBoardState::new(mission),
            started: Instant::now(),
            help: false,
            detail_open: false,
            worker_finished: false,
            _terminal_guard: terminal_guard,
        };
        tui.draw()?;
        Ok(tui)
    }

    /// Run the tick/input loop until the user closes the board. Events keep arriving while the
    /// mission worker runs; closing the view does not cancel that worker.
    pub fn run(&mut self, receiver: mpsc::Receiver<MissionUiEvent>) -> io::Result<()> {
        let mut last_tick = Instant::now();
        loop {
            let now = Instant::now();
            self.state.tick(now.duration_since(last_tick).as_secs_f64());
            last_tick = now;

            loop {
                match receiver.try_recv() {
                    Ok(event) => self.apply(event),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        if !self.worker_finished {
                            self.apply(MissionUiEvent::Finished(Err(
                                "mission worker exited without a final report".into(),
                            )));
                        }
                        break;
                    }
                }
            }

            if event::poll(Duration::from_millis(40))? {
                match event::read()? {
                    TerminalEvent::Key(key) if is_key_press(key) => {
                        if self.handle_key(key)? {
                            break;
                        }
                    }
                    TerminalEvent::Resize(_, _) => {}
                    _ => {}
                }
            }
            self.draw()?;
        }
        Ok(())
    }

    fn apply(&mut self, event: MissionUiEvent) {
        let secs = self.started.elapsed().as_secs();
        match event {
            MissionUiEvent::Mission(event) => reduce_mission_event(&mut self.state, event, secs),
            MissionUiEvent::Notice(message) => {
                push_activity(&mut self.state, secs, ActivityKind::Result, None, message);
            }
            MissionUiEvent::Finished(Ok(summary)) => {
                self.worker_finished = true;
                reconcile_mission_final(&mut self.state, summary, secs);
            }
            MissionUiEvent::Finished(Err(error)) => {
                self.worker_finished = true;
                set_agent_state(&mut self.state, "Strategos", AgentState::Failed);
                for agent in &mut self.state.mission.agents {
                    if !agent.role.is_boss() && !agent.state.is_terminal() {
                        set_state(agent, AgentState::Cancelled);
                        agent.reason.get_or_insert_with(|| "mission aborted".into());
                    }
                }
                push_activity(
                    &mut self.state,
                    secs,
                    ActivityKind::Conflict,
                    Some("Strategos".into()),
                    error,
                );
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> io::Result<bool> {
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl_c {
            return Ok(true);
        }
        if self.help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::F(1) => self.help = false,
                KeyCode::Char('q') => return Ok(true),
                _ => {}
            }
            return Ok(false);
        }
        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Esc if self.detail_open => self.detail_open = false,
            KeyCode::Esc => return Ok(true),
            KeyCode::Char('?') | KeyCode::F(1) => self.help = true,
            KeyCode::Enter => self.detail_open = !self.detail_open,
            KeyCode::Up | KeyCode::Char('k') => self.state.up(),
            KeyCode::Down | KeyCode::Char('j') => self.state.down(),
            _ => {}
        }
        Ok(false)
    }

    fn draw(&mut self) -> io::Result<()> {
        let help_open = self.help;
        let detail_open = self.detail_open;
        self.terminal
            .draw(|frame| {
                let area = frame.area();
                mission_board::render_screen(frame, area, &self.state);
                if detail_open {
                    render_mission_detail_overlay(frame, area, &self.state);
                }
                if help_open {
                    help::render(frame, area, HelpContext::Mission);
                }
            })
            .map(|_| ())
    }
}

/// Apply one real orchestrator event to the same model used by the board renderer.
pub fn reduce_mission_event(state: &mut MissionBoardState, event: MissionEvent, secs: u64) {
    match event {
        MissionEvent::Planning { task } => {
            state.mission.name = task.clone();
            set_agent_state(state, "Strategos", AgentState::Running);
            push_activity(
                state,
                secs,
                ActivityKind::Plan,
                Some("Strategos".into()),
                "reading the repository layout",
            );
        }
        MissionEvent::Named { renames } => {
            state.mission.name_phalanx(&renames);
            for activity in Mission::naming_log(secs, &renames) {
                state.mission.log(activity);
            }
        }
        MissionEvent::Planned { plan } => {
            for assignment in &plan.assignments {
                if let Some(agent) = state
                    .mission
                    .agents
                    .iter_mut()
                    .find(|agent| agent.id == assignment.codename)
                {
                    agent.task = assignment.task.clone();
                }
            }
            push_activity(
                state,
                secs,
                ActivityKind::Plan,
                Some("Strategos".into()),
                format!(
                    "{} plan · {} assignment(s)",
                    plan.source.label(),
                    plan.assignments.len()
                ),
            );
        }
        MissionEvent::Deployed {
            codename,
            branch,
            worktree,
        } => {
            if let Some(agent) = find_agent_mut(state, &codename) {
                set_state(agent, AgentState::Running);
                agent.branch = Some(branch.clone());
                agent.worktree = Some(worktree.clone());
            }
            push_activity(
                state,
                secs,
                ActivityKind::Deploy,
                Some(codename),
                format!("deployed on {branch}"),
            );
        }
        MissionEvent::Agent { codename, event } => {
            let (kind, text) = match event {
                AgentEvent::Step { index } => (ActivityKind::Tool, format!("step {}", index + 1)),
                AgentEvent::Text { .. } => return,
                AgentEvent::ToolCall { name, .. } => (ActivityKind::Tool, format!("call {name}")),
                AgentEvent::ToolResult {
                    name,
                    ok,
                    summary,
                    ms,
                } => (
                    ActivityKind::Result,
                    format!(
                        "{name} {} · {}ms · {}",
                        if ok { "ok" } else { "failed" },
                        ms,
                        short(&summary, 72)
                    ),
                ),
                AgentEvent::Finished { reason, .. } => {
                    (ActivityKind::Result, reason.label().to_string())
                }
            };
            push_activity(state, secs, kind, Some(codename), text);
        }
        MissionEvent::Returned {
            codename,
            state: result,
        } => {
            if let Some(agent) = find_agent_mut(state, &codename) {
                match result {
                    OperatorState::Succeeded => set_state(agent, AgentState::Done),
                    OperatorState::Empty => {
                        set_state(agent, AgentState::Done);
                        agent.reason.get_or_insert_with(|| "no changes".into());
                    }
                    OperatorState::Failed => {
                        set_state(agent, AgentState::Failed);
                        agent.reason.get_or_insert_with(|| "operator failed".into());
                    }
                }
            }
            push_activity(
                state,
                secs,
                ActivityKind::Result,
                Some(codename.clone()),
                format!("returned · {}", result.label()),
            );
        }
        MissionEvent::Verified { codename, ok } => {
            if !ok {
                if let Some(agent) = find_agent_mut(state, &codename) {
                    agent.reason = Some("verification failed".into());
                }
            }
            push_activity(
                state,
                secs,
                ActivityKind::Result,
                Some(codename),
                if ok {
                    "verification passed"
                } else {
                    "verification failed"
                }
                .to_string(),
            );
        }
        MissionEvent::Reviewing { reports } => {
            set_agent_state(state, "Strategos", AgentState::Running);
            push_activity(
                state,
                secs,
                ActivityKind::Verdict,
                Some("Strategos".into()),
                format!("reviewing {reports} report(s)"),
            );
        }
        MissionEvent::Reviewed { verdicts, .. } => {
            for (codename, verdict, reason) in verdicts {
                if let Some(agent) = find_agent_mut(state, &codename) {
                    agent.verdict = match verdict {
                        AgentVerdict::Accepted => Verdict::Accepted,
                        AgentVerdict::Rejected => Verdict::Rejected,
                        AgentVerdict::Unreviewed => Verdict::Unreviewed,
                    };
                    if !reason.is_empty() {
                        agent.reason = Some(reason.clone());
                    }
                }
                push_activity(
                    state,
                    secs,
                    ActivityKind::Verdict,
                    Some(codename),
                    format!("{} · {}", verdict.label(), short(&reason, 72)),
                );
            }
        }
        MissionEvent::Applying { codename, file } => push_activity(
            state,
            secs,
            ActivityKind::Result,
            Some(codename.clone()),
            format!("applied {file}"),
        ),
        MissionEvent::Conflict { file, owner, other } => push_activity(
            state,
            secs,
            ActivityKind::Conflict,
            Some(owner.clone()),
            format!("{file} · {owner} wins over {other}"),
        ),
        MissionEvent::Done { applied, conflicts } => {
            set_agent_state(state, "Strategos", AgentState::Done);
            push_activity(
                state,
                secs,
                ActivityKind::Done,
                Some("Strategos".into()),
                format!("mission done · {applied} applied · {conflicts} conflict(s)"),
            );
        }
    }
    state.clamp();
}

/// Reconcile the board from the compact report snapshot after the worker exits. The event
/// stream is primary; this catches early returns such as sandbox setup failures that do not
/// emit an individual `Returned` event.
pub fn reconcile_mission_final(state: &mut MissionBoardState, summary: MissionUiFinal, secs: u64) {
    for final_agent in summary.operators {
        if let Some(agent) = find_agent_mut(state, &final_agent.codename) {
            agent.task = final_agent.task;
            set_state(
                agent,
                match final_agent.state {
                    OperatorState::Succeeded | OperatorState::Empty => AgentState::Done,
                    OperatorState::Failed => AgentState::Failed,
                },
            );
            agent.verdict = match final_agent.verdict {
                AgentVerdict::Accepted => Verdict::Accepted,
                AgentVerdict::Rejected => Verdict::Rejected,
                AgentVerdict::Unreviewed => Verdict::Unreviewed,
            };
            if !final_agent.reason.is_empty() {
                agent.reason = Some(final_agent.reason);
            } else if final_agent.state == OperatorState::Empty {
                agent.reason.get_or_insert_with(|| "no changes".into());
            }
            agent.input_tokens = final_agent.input_tokens;
            agent.output_tokens = final_agent.output_tokens;
            agent.elapsed_secs = final_agent.elapsed_ms / 1_000;
            if agent.state.is_terminal() {
                agent.progress = 100;
            }
        }
    }
    push_activity(
        state,
        secs,
        ActivityKind::Done,
        Some("Strategos".into()),
        format!(
            "{} · {} applied · {} conflict(s)",
            summary.outcome, summary.applied, summary.conflicts
        ),
    );
    state.clamp();
}

fn push_activity(
    state: &mut MissionBoardState,
    secs: u64,
    kind: ActivityKind,
    codename: Option<String>,
    text: impl Into<String>,
) {
    state.mission.log(Activity::new(secs, kind, codename, text));
}

fn find_agent_mut<'a>(
    state: &'a mut MissionBoardState,
    id: &str,
) -> Option<&'a mut stoic_codex_ui_core::mission::Agent> {
    state.mission.agents.iter_mut().find(|agent| agent.id == id)
}

fn set_agent_state(state: &mut MissionBoardState, id: &str, target: AgentState) {
    if let Some(agent) = find_agent_mut(state, id) {
        set_state(agent, target);
    }
}

fn set_state(agent: &mut stoic_codex_ui_core::mission::Agent, target: AgentState) {
    if can_transition(agent.state, target) {
        let _ = agent.transition(target);
    }
}

fn short(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn render_mission_detail_overlay(frame: &mut Frame, area: Rect, state: &MissionBoardState) {
    let width = area.width.min(76);
    let height = area.height.min(18);
    if width < 28 || height < 7 {
        return;
    }
    let overlay = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, overlay);
    frame.render_widget(
        Block::default().style(Style::default().bg(t::RAISED)),
        overlay,
    );
    mission_board::render_detail(
        frame,
        Rect {
            x: overlay.x + 1,
            y: overlay.y + 1,
            width: overlay.width.saturating_sub(2),
            height: overlay.height.saturating_sub(3),
        },
        state,
    );
    frame.render_widget(
        Paragraph::new(" Enter / Esc close").style(t::faint()),
        Rect {
            x: overlay.x + 1,
            y: overlay.y + overlay.height - 2,
            width: overlay.width.saturating_sub(2),
            height: 1,
        },
    );
}

fn render_chat(frame: &mut Frame, state: &ChatState) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(t::VOID)), area);
    if area.width < 28 || area.height < 5 {
        frame.render_widget(
            Paragraph::new("Resize the terminal to at least 28 × 5")
                .style(t::muted())
                .alignment(ratatui::layout::Alignment::Center),
            area,
        );
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);

    let title = Line::from(vec![
        Span::styled("● ", t::mark()),
        Span::styled("STOIC CODEX", t::title()),
        Span::styled("   /   CHAT", t::faint()),
    ]);
    let live_tag = if state.busy { "● LIVE" } else { "● READY" };
    let indicator_width = area.width.saturating_sub(24).min(38);
    let live_width = live_tag.chars().count() as u16;
    let indicator = if indicator_width <= live_width {
        Line::from(Span::styled(
            crate::views::fit(live_tag, indicator_width as usize),
            if state.busy { t::accent() } else { t::faint() },
        ))
    } else {
        Line::from(vec![
            Span::styled(live_tag, if state.busy { t::accent() } else { t::faint() }),
            Span::raw("  "),
            Span::styled(
                visible_input(
                    &state.title,
                    indicator_width.saturating_sub(live_width + 2) as usize,
                ),
                t::faint(),
            ),
        ])
    };
    let header = Layout::horizontal([Constraint::Min(1), Constraint::Length(indicator_width)])
        .split(rows[0]);
    frame.render_widget(Paragraph::new(title), header[0]);
    frame.render_widget(
        Paragraph::new(indicator).alignment(ratatui::layout::Alignment::Right),
        header[1],
    );

    let content_width = area.width.min(108).saturating_sub(2).max(1);
    let content_x = area.x + area.width.saturating_sub(content_width) / 2;
    let transcript = Rect {
        x: content_x,
        y: rows[1].y,
        width: content_width,
        height: rows[1].height,
    };
    let text = chat_text(state);
    let total_lines = wrapped_line_count(&text, transcript.width);
    let visible = transcript.height as usize;
    let max_scroll = total_lines.saturating_sub(visible);
    let scroll = state
        .scroll
        .unwrap_or(max_scroll)
        .min(max_scroll)
        .min(u16::MAX as usize) as u16;
    frame.render_widget(
        Paragraph::new(text)
            .style(t::text())
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        transcript,
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("› ", t::focus()),
            if state.input.is_empty() {
                Span::styled("Ask a follow-up…", t::faint())
            } else {
                Span::styled(
                    visible_input(&state.input, rows[2].width.saturating_sub(3) as usize),
                    t::text(),
                )
            },
            Span::styled("▏", t::accent()),
        ])),
        rows[2],
    );
    let last = last_status(&state.status);
    let footer_text = if state.busy {
        last.unwrap_or_else(|| "streaming response".to_string())
    } else {
        match last {
            Some(last) => format!("{} · {last}", state.status_label),
            None => format!("{} · {}", state.status_label, state.title),
        }
    };
    frame.render_widget(
        Paragraph::new(chat_footer(
            &state.status_label,
            state.busy,
            &footer_text,
            rows[3].width,
        )),
        rows[3],
    );

    if state.help {
        help::render(frame, area, HelpContext::Chat);
    }
}

fn chat_footer(status_label: &str, busy: bool, detail: &str, width: u16) -> Line<'static> {
    let width = width as usize;
    let hint = if width < 40 {
        "Esc exit · Enter · ?"
    } else if width < 66 {
        "Esc close · Enter send · ? help"
    } else if width < 92 {
        "Esc close · Enter send · ? help · PgUp scroll"
    } else {
        "Esc close · Enter send · ? keys · PgUp/PgDn scroll"
    };
    let state_tag = if width < 42 {
        if busy {
            "●"
        } else {
            ""
        }
    } else if busy {
        "● LIVE"
    } else if status_label == "complete" {
        "● DONE"
    } else if status_label == "failed" {
        "● FAILED"
    } else {
        "● READY"
    };
    let badge = format!(" {state_tag} ");
    let hint_width = hint.chars().count().min(width);
    let info_budget = width.saturating_sub(badge.chars().count() + hint_width + 3);
    let info = if info_budget >= 3 {
        format!(" {} ", visible_input(detail, info_budget - 2))
    } else {
        String::new()
    };
    let padding = width.saturating_sub(badge.chars().count() + info.chars().count() + hint_width);
    let mut spans = vec![Span::styled(
        badge,
        if busy { t::badge() } else { t::chip() },
    )];
    if !info.is_empty() {
        spans.push(Span::styled(info, t::chip_text()));
    }
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled(hint, t::faint()));
    Line::from(spans)
}

fn trim_chat_history(messages: &mut Vec<ChatMessage>) {
    while messages.len() > CHAT_MESSAGE_LIMIT {
        let remove = messages.len().min(2);
        messages.drain(..remove);
    }
}

fn chat_text(state: &ChatState) -> Text<'static> {
    let mut lines = Vec::new();
    for message in &state.messages {
        let (speaker, speaker_style, body_style) = if message.assistant {
            ("CODEX", t::accent(), t::text())
        } else {
            ("YOU", t::good(), t::text())
        };
        lines.push(Line::from(vec![Span::styled(
            speaker,
            speaker_style.add_modifier(Modifier::BOLD),
        )]));
        for body_line in message.body.lines() {
            lines.push(Line::from(Span::styled(
                format!("  {body_line}"),
                body_style,
            )));
        }
        lines.push(Line::raw(""));
    }
    let current = String::from_utf8_lossy(&state.current);
    if !current.is_empty() || state.busy {
        lines.push(Line::from(Span::styled(
            if current.is_empty() {
                "CODEX · thinking"
            } else {
                "CODEX"
            },
            t::accent().add_modifier(Modifier::BOLD),
        )));
        for body_line in current.lines() {
            lines.push(Line::from(Span::styled(
                format!("  {body_line}"),
                t::text(),
            )));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Start with a question or a task.",
            t::muted(),
        )));
    }
    Text::from(lines)
}

fn wrapped_line_count(text: &Text<'_>, width: u16) -> usize {
    let width = usize::from(width.max(1));
    text.lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(width))
        .sum()
}

fn visible_input(input: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let chars: Vec<char> = input.chars().collect();
    if chars.len() <= width {
        return input.to_string();
    }
    let start = chars.len().saturating_sub(width.saturating_sub(1));
    format!("…{}", chars[start..].iter().collect::<String>())
}

fn last_status(bytes: &[u8]) -> Option<String> {
    let text = strip_ansi(bytes);
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| short(line, 120))
}

fn strip_ansi(bytes: &[u8]) -> String {
    let input = String::from_utf8_lossy(bytes);
    let mut chars = input.chars().peekable();
    let mut out = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for code in chars.by_ref() {
                    if ('@'..='~').contains(&code) {
                        break;
                    }
                }
            }
            continue;
        }
        if ch == '\r' || ch == '\n' || !ch.is_control() {
            out.push(ch);
        }
    }
    out
}

fn chat_scroll(
    messages: &[ChatMessage],
    current: &[u8],
    current_scroll: Option<usize>,
    width: u16,
    visible: usize,
    delta: isize,
) -> usize {
    let state = ChatState {
        title: String::new(),
        messages: messages.to_vec(),
        current: current.to_vec(),
        status: Vec::new(),
        status_label: String::new(),
        input: String::new(),
        busy: false,
        scroll: None,
        help: false,
    };
    let text = chat_text(&state);
    let total = wrapped_line_count(&text, width);
    let max_scroll = total.saturating_sub(visible);
    let current = current_scroll.unwrap_or(max_scroll) as isize;
    (current + delta).clamp(0, max_scroll as isize) as usize
}

fn is_key_press(key: KeyEvent) -> bool {
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<TerminalGuard> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            crossterm::event::EnableBracketedPaste,
            Hide
        ) {
            let _ = execute!(
                stdout,
                Show,
                crossterm::event::DisableBracketedPaste,
                LeaveAlternateScreen
            );
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(
            stdout,
            Show,
            crossterm::event::DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use stoic_codex_agent::{Assignment, Plan, PlanSource};
    use stoic_codex_ui_core::mission::Mission;

    fn state(task: &str, operators: usize) -> MissionBoardState {
        MissionBoardState::new(Mission::deploy(task, "/repo", operators, task))
    }

    fn line_text(buffer: &ratatui::buffer::Buffer, row: u16) -> String {
        (0..buffer.area.width)
            .map(|col| buffer[(col, row)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn chat_input_and_footer_remain_pinned_at_the_bottom() {
        let mut state = ChatState::new("gemini / model".into());
        state.messages.push(ChatMessage {
            assistant: false,
            body: "Do the task".into(),
        });
        state.current = b"Streaming answer".to_vec();
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render_chat(frame, &state)).unwrap();
        let buffer = terminal.backend().buffer();
        let lines: Vec<String> = (0..buffer.area.height)
            .map(|row| line_text(buffer, row))
            .collect();
        assert!(lines.iter().any(|line| line.contains("Streaming answer")));
        assert!(lines[22].contains("Ask a follow-up"));
        assert!(lines[23].contains("Esc close"));
    }

    #[test]
    fn chat_header_keeps_the_product_name_visible_in_a_compact_terminal() {
        let mut state = ChatState::new("a very long provider name / model identifier".into());
        state.busy = true;
        let backend = TestBackend::new(52, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render_chat(frame, &state)).unwrap();
        let header = line_text(terminal.backend().buffer(), 0);
        assert!(header.contains("STOIC CODEX"), "{header}");
        assert!(header.contains("LIVE"), "{header}");
    }

    #[test]
    fn chat_wraps_long_turns_without_hiding_the_input_dock() {
        let mut state = ChatState::new("model".into());
        state.messages.push(ChatMessage {
            assistant: true,
            body: "word ".repeat(180),
        });
        let backend = TestBackend::new(52, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render_chat(frame, &state)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(line_text(buffer, 12).contains("Ask a follow-up"));
        assert!(line_text(buffer, 13).contains("Esc close"));
    }

    #[test]
    fn chat_footer_uses_a_live_badge_and_fits_a_compact_width() {
        let line = chat_footer("thinking", true, "streaming response", 52);
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("LIVE"), "{text}");
        assert!(text.contains("Esc close"), "{text}");
        assert!(line.width() <= 52);
        assert_eq!(line.spans[0].style.bg, Some(t::RED));
        for width in 28..=120 {
            for busy in [false, true] {
                let line = chat_footer("complete", busy, "streaming · 120 tokens", width);
                assert!(line.width() <= width as usize, "width {width}: {line:?}");
            }
        }
    }

    #[test]
    fn chat_history_keeps_only_a_bounded_tail_of_messages() {
        let mut messages = (0..CHAT_MESSAGE_LIMIT + 3)
            .map(|i| ChatMessage {
                assistant: i % 2 == 1,
                body: format!("turn {i}"),
            })
            .collect::<Vec<_>>();
        trim_chat_history(&mut messages);
        assert!(messages.len() <= CHAT_MESSAGE_LIMIT);
        assert_eq!(
            messages.first().map(|message| message.assistant),
            Some(false)
        );
        assert_eq!(
            messages.last().map(|message| message.assistant),
            Some(false)
        );
    }

    #[test]
    fn strips_ansi_from_status_lines_and_keeps_the_latest_message() {
        let status = b"\x1b[2mretry\x1b[0m\n\x1b[31mHTTP 429\x1b[0m\n";
        assert_eq!(last_status(status).as_deref(), Some("HTTP 429"));
    }

    #[test]
    fn mission_reducer_tracks_plan_names_deployments_verdicts_and_completion() {
        let mut state = state("ship it", 2);
        reduce_mission_event(
            &mut state,
            MissionEvent::Planning {
                task: "ship it".into(),
            },
            0,
        );
        reduce_mission_event(
            &mut state,
            MissionEvent::Named {
                renames: vec![("slot-1".into(), "x1-write".into())],
            },
            1,
        );
        reduce_mission_event(
            &mut state,
            MissionEvent::Planned {
                plan: Plan {
                    assignments: vec![Assignment::with_code(
                        "x1-write",
                        "x1-write",
                        "write the parser",
                    )],
                    source: PlanSource::Strategos,
                    raw: String::new(),
                },
            },
            2,
        );
        reduce_mission_event(
            &mut state,
            MissionEvent::Deployed {
                codename: "x1-write".into(),
                branch: "stoic-codex/x1-write".into(),
                worktree: "/repo/.git/stoic-codex-missions/x1-write".into(),
            },
            3,
        );
        reduce_mission_event(
            &mut state,
            MissionEvent::Reviewed {
                plan: Plan {
                    assignments: vec![],
                    source: PlanSource::Strategos,
                    raw: String::new(),
                },
                verdicts: vec![("x1-write".into(), AgentVerdict::Accepted, "clean".into())],
            },
            4,
        );
        reduce_mission_event(
            &mut state,
            MissionEvent::Done {
                applied: 1,
                conflicts: 0,
            },
            5,
        );

        let operator = state
            .mission
            .agents
            .iter()
            .find(|agent| agent.id == "x1-write")
            .unwrap();
        assert_eq!(operator.task, "write the parser");
        assert_eq!(operator.branch.as_deref(), Some("stoic-codex/x1-write"));
        assert_eq!(
            operator.worktree.as_deref(),
            Some("/repo/.git/stoic-codex-missions/x1-write")
        );
        assert_eq!(operator.verdict, Verdict::Accepted);
        assert_eq!(state.mission.boss().unwrap().state, AgentState::Done);
        assert_eq!(state.mission.activity.len(), 6);
    }

    #[test]
    fn returned_and_failed_verification_are_visible_without_fake_success() {
        let mut state = state("task", 1);
        reduce_mission_event(
            &mut state,
            MissionEvent::Planning {
                task: "task".into(),
            },
            0,
        );
        reduce_mission_event(
            &mut state,
            MissionEvent::Deployed {
                codename: "slot-1".into(),
                branch: "stoic-codex/slot-1".into(),
                worktree: "/repo/.git/stoic-codex-missions/slot-1".into(),
            },
            1,
        );
        reduce_mission_event(
            &mut state,
            MissionEvent::Verified {
                codename: "slot-1".into(),
                ok: false,
            },
            2,
        );
        reduce_mission_event(
            &mut state,
            MissionEvent::Returned {
                codename: "slot-1".into(),
                state: OperatorState::Succeeded,
            },
            3,
        );
        let operator = state
            .mission
            .agents
            .iter()
            .find(|agent| agent.id == "slot-1")
            .unwrap();
        assert_eq!(operator.state, AgentState::Done);
        assert_eq!(operator.reason.as_deref(), Some("verification failed"));
        assert!(state
            .mission
            .activity
            .iter()
            .any(|a| a.text == "verification failed"));
    }

    #[test]
    fn final_snapshot_reconciles_an_operator_that_returned_without_an_event() {
        let mut state = state("task", 1);
        reconcile_mission_final(
            &mut state,
            MissionUiFinal {
                outcome: "partial success".into(),
                operators: vec![MissionUiOperator {
                    codename: "slot-1".into(),
                    task: "inspect the module".into(),
                    state: OperatorState::Failed,
                    verdict: AgentVerdict::Unreviewed,
                    reason: "sandbox could not start".into(),
                    input_tokens: 18,
                    output_tokens: 7,
                    elapsed_ms: 1_250,
                }],
                applied: 1,
                conflicts: 0,
            },
            9,
        );
        let operator = state
            .mission
            .agents
            .iter()
            .find(|agent| agent.id == "slot-1")
            .unwrap();
        assert_eq!(operator.state, AgentState::Failed);
        assert_eq!(operator.reason.as_deref(), Some("sandbox could not start"));
        assert_eq!((operator.input_tokens, operator.output_tokens), (18, 7));
        assert_eq!(operator.elapsed_secs, 1);
        assert_eq!(operator.verdict, Verdict::Unreviewed);
    }

    #[test]
    fn mission_screen_render_does_not_require_a_real_terminal() {
        let mut state = state("event-driven", 3);
        reduce_mission_event(
            &mut state,
            MissionEvent::Planning {
                task: "event-driven".into(),
            },
            0,
        );
        let backend = TestBackend::new(120, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| mission_board::render_screen(frame, frame.area(), &state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .map(|row| line_text(buffer, row))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Strategos"));
        assert!(rendered.contains("event-driven"));
        assert!(rendered.contains("cost —"));
        assert!(rendered.contains("— (no cap)"));
    }
}
