//! Memory graph view: the hybrid vector+graph store, rendered as a readable layered
//! neighbourhood instead of an unreadable force-directed cloud.
//!
//! Layout comes from `zenithal_ui_core::graph::layout` (deterministic, tested). This
//! module draws ranks as rows with an edge gutter, so a decision → file → symbol chain
//! reads top-to-bottom like a call tree.
//!
//! ```text
//! ◆ use argon2              derived_from
//!   └──▪ src/auth.rs        imports        0.91  ⟵ focus
//!        └──ƒ fn verify     regresses
//!             └──✗ timing leak
//! ```

use zenithal_ui_core::graph::{self, EdgeKind, Graph, Layout, NodeKind};
use ratatui::layout::{Constraint, Rect};
use ratatui::layout::Layout as Chunks;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::fit;
use crate::chrome::{self, Chrome, Segment, TAB_MEMORY};
use crate::theme as t;

pub struct MemoryGraphState {
    pub graph: Graph,
    pub focus: String,
    pub depth: u32,
    pub max_nodes: usize,
    pub scroll: u16,
    cached: Option<Layout>,
}

impl MemoryGraphState {
    pub fn new(graph: Graph, focus: impl Into<String>) -> Self {
        MemoryGraphState {
            graph,
            focus: focus.into(),
            depth: 2,
            max_nodes: 24,
            scroll: 0,
            cached: None,
        }
    }

    /// Layout is memoized: recomputing on every redraw at 60fps would be the one place
    /// this view could cost real CPU.
    pub fn layout(&mut self) -> &Layout {
        if self.cached.is_none() {
            self.cached = Some(graph::layout(&self.graph, &self.focus, self.depth, self.max_nodes));
        }
        self.cached.as_ref().expect("just set")
    }

    pub fn invalidate(&mut self) {
        self.cached = None;
    }

    /// What to *show* for the focus. `focus` is an id (`file:auth.rs`); the board and the
    /// frame want the node's label, and fall back to the id if it is not in the graph yet.
    pub fn focus_label(&self) -> String {
        self.graph
            .nodes
            .iter()
            .find(|n| n.id == self.focus)
            .map(|n| n.label.clone())
            .unwrap_or_else(|| self.focus.clone())
    }

    pub fn deeper(&mut self) {
        self.depth = (self.depth + 1).min(6);
        self.invalidate();
    }

    pub fn shallower(&mut self) {
        self.depth = self.depth.saturating_sub(1).max(1);
        self.invalidate();
    }
}

fn kind_style(kind: NodeKind) -> Style {
    // Red carries identity and failure; the rest of the graph is grey on purpose, so a
    // symbol or a decision does not compete with the focus node for attention.
    match kind {
        NodeKind::File | NodeKind::Agent => t::accent(),
        NodeKind::Task | NodeKind::Doc => t::text(),
        NodeKind::Symbol => t::good(),
        NodeKind::Decision => t::warn(),
        NodeKind::Error => t::bad(),
        NodeKind::Commit => t::faint(),
    }
}

fn indent_for(rank: u32) -> String {
    // Two columns per rank, plus the edge gutter.
    "  ".repeat(rank as usize)
}

/// Draw the graph body. `state.layout()` is taken as a parameter so this function stays
/// pure w.r.t. the state's mutability.
pub fn render_with(frame: &mut Frame, area: Rect, state: &MemoryGraphState, layout: &Layout) {
    // No border: a title line, a rule, then the rows.
    let header = Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::styled(fit(&state.focus_label(), 34), t::title()),
        Span::styled(format!("   depth {}", state.depth), t::faint()),
        Span::styled(format!("   {} nodes", layout.placed.len()), t::faint()),
        if layout.omitted > 0 {
            Span::styled(format!("   {} hidden", layout.omitted), t::warn())
        } else {
            Span::styled("", Style::default())
        },
    ]);
    let chunks = Chunks::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(Paragraph::new(header), chunks[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("─".repeat(chunks[1].width as usize), t::rule()))),
        chunks[1],
    );

    let inner = chunks[2];
    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for (rank, row) in layout.rows() {
        // Edge gutter for the first link leaving this rank, if any.
        let rule = layout
            .links
            .iter()
            .find(|l| l.from.rank == rank)
            .map(|l| l.kind.rule())
            .unwrap_or("  ");
        for (i, placed) in row.iter().enumerate() {
            let prefix = if i == 0 { rule } else { "  " };
            let is_focus = placed.rank == 0;
            let glyph_style = kind_style(placed.node.kind);
            let mut spans = vec![
                Span::styled(" ", Style::default()),
                Span::styled(format!("{prefix} "), t::faint()),
                Span::styled(indent_for(rank), t::rule()),
                Span::styled(
                    format!("{} ", placed.node.kind.glyph()),
                    if is_focus { glyph_style.add_modifier(Modifier::BOLD) } else { glyph_style },
                ),
                Span::styled(
                    fit(&placed.node.label, width.saturating_sub(30)),
                    if is_focus { t::title() } else { t::text() },
                ),
            ];
            // Score, right-aligned at a fixed column so a column of numbers reads as one.
            let label_w = fit(&placed.node.label, width.saturating_sub(30)).chars().count();
            let pad = width
                .saturating_sub(2 + prefix.chars().count() + 1 + indent_for(rank).chars().count() + 2 + label_w + 8)
                .max(1);
            spans.push(Span::styled(" ".repeat(pad), Style::default()));
            spans.push(Span::styled(format!("{:.2}", placed.node.score), t::faint()));
            if is_focus {
                spans.push(Span::styled("  ←", t::accent()));
            }
            lines.push(Line::from(spans));
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  nothing linked to this focus yet — the graph fills as sessions run",
            t::faint(),
        )));
    }

    // Legend, pinned to the last line when it fits. One line, grey, no colour at all: it is
    // a key, not content.
    if inner.height as usize > lines.len() + 1 {
        lines.push(Line::from(""));
        let mut legend = vec![Span::styled("  ", Style::default())];
        for (name, style) in [
            ("imports", t::faint()),
            (EdgeKind::Imports.rule(), t::faint()),
            ("  calls", t::faint()),
            (EdgeKind::Calls.rule(), t::faint()),
            ("  derived_from", t::faint()),
            (EdgeKind::DerivedFrom.rule(), t::faint()),
            ("  regresses", t::faint()),
            (EdgeKind::Regresses.rule(), t::faint()),
            ("   +/- depth", t::faint()),
        ] {
            legend.push(Span::styled(name.to_string(), style));
        }
        lines.push(Line::from(legend));
    }

    frame.render_widget(Paragraph::new(lines).scroll((state.scroll, 0)), inner);
}

/// Convenience wrapper that takes the state mutably so the cache can be populated.
pub fn render(frame: &mut Frame, area: Rect, state: &mut MemoryGraphState) {
    let layout = state.layout().clone();
    render_with(frame, area, state, &layout);
}

/// Whole-screen version, with the shared frame around it.
pub fn render_screen(frame: &mut Frame, area: Rect, state: &mut MemoryGraphState) {
    let nodes = state.layout().placed.len();
    let chrome = Chrome::new("main", &chrome::TABS, TAB_MEMORY)
        .with_branch_status()
        .status(Segment::new(format!("{} nodes", nodes), t::muted()))
        .footer(Segment::chip(format!("depth {}", state.depth)))
        .footer(Segment::chip(format!("focus {}", fit(&state.focus_label(), 24))))
        .hint("+/- depth · j/k scroll · enter open · q quit");
    let body = chrome::frame(frame, area, &chrome);
    render(frame, body, state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenithal_ui_core::graph::{EdgeKind, Node};

    fn state() -> MemoryGraphState {
        let mut g = Graph::default();
        g.add_node(Node { id: "f".into(), kind: NodeKind::File, label: "src/auth.rs".into(), score: 0.91 });
        g.add_node(Node { id: "s".into(), kind: NodeKind::Symbol, label: "fn verify".into(), score: 0.66 });
        g.add_edge("f", "s", EdgeKind::Imports);
        MemoryGraphState::new(g, "f")
    }

    #[test]
    fn depth_changes_are_clamped_and_invalidate_the_cache() {
        let mut st = state();
        let _ = st.layout().placed.len();
        assert!(st.cached.is_some());
        st.deeper();
        assert!(st.cached.is_none(), "depth change must invalidate the layout cache");
        assert_eq!(st.depth, 3);
        for _ in 0..10 {
            st.deeper();
        }
        assert_eq!(st.depth, 6, "depth is capped");
        for _ in 0..10 {
            st.shallower();
        }
        assert_eq!(st.depth, 1, "depth floor is 1");
    }

    #[test]
    fn layout_is_memoized_between_calls() {
        let mut st = state();
        let first = st.layout().placed.len();
        let second = st.layout().placed.len();
        assert_eq!(first, second);
        assert!(st.cached.is_some());
    }

    #[test]
    fn indent_is_two_columns_per_rank() {
        assert_eq!(indent_for(0), "");
        assert_eq!(indent_for(1), "  ");
        assert_eq!(indent_for(3), "      ");
    }

    #[test]
    fn the_focus_is_shown_by_label_not_by_id() {
        // The graph keys on ids; the screen must show the words.
        assert_eq!(state().focus_label(), "src/auth.rs", "the id `f` is the file node");

        let mut unknown = state();
        unknown.focus = "file:not-in-the-graph".into();
        assert_eq!(
            unknown.focus_label(),
            "file:not-in-the-graph",
            "an id with no node still prints something rather than panicking"
        );
    }

    #[test]
    fn the_graph_renders_inside_the_frame_without_overflowing() {
        let mut st = state();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 18)).unwrap();
        terminal.draw(|f| render_screen(f, f.area(), &mut st)).unwrap();
        let screen: String =
            terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(screen.contains("src/auth.rs"), "{screen}");
        assert!(screen.contains("fn verify"), "{screen}");
        assert!(screen.contains("memory"), "the frame names the active tab: {screen}");
        assert!(screen.contains("0.91"), "scores are shown: {screen}");
    }
}
