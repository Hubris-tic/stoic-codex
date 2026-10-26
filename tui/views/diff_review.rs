//! Diff review: a file rail on the left, side-by-side hunks on the right.
//!
//! The interesting decisions live in `zenithal_ui_core::diff` (hunk pairing, folding); this
//! module positions them. Layout: `│ old# │ old text ┃ new# │ new text │`, with the changed
//! characters marked inline.
//!
//! Colour follows the one rule that matters here: **a removed line is red, an added line is
//! jade, and context is grey**. Red is the primary and dominates the screen; the added side
//! is a single quiet hue so the eye can find it without the screen turning into a
//! Christmas tree.

use zenithal_ui_core::diff::{self, FileDiff, LineKind, SideCell, SideRow};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::fit;
use crate::chrome::{self, Chrome, Segment, TAB_DIFF};
use crate::theme as t;

pub struct DiffReviewState {
    pub files: Vec<FileDiff>,
    pub file_index: usize,
    pub scroll: u16,
    /// Lines of unchanged context kept around each change.
    pub context: u32,
}

impl DiffReviewState {
    pub fn new(files: Vec<FileDiff>) -> Self {
        DiffReviewState { files, file_index: 0, scroll: 0, context: 3 }
    }

    pub fn current(&self) -> Option<&FileDiff> {
        self.files.get(self.file_index)
    }

    pub fn rows(&self) -> Vec<SideRow> {
        self.current().map(|f| diff::side_by_side(f, self.context)).unwrap_or_default()
    }

    pub fn next_file(&mut self) {
        if !self.files.is_empty() {
            self.file_index = (self.file_index + 1) % self.files.len();
            self.scroll = 0;
        }
    }

    pub fn prev_file(&mut self) {
        if !self.files.is_empty() {
            self.file_index = if self.file_index == 0 { self.files.len() - 1 } else { self.file_index - 1 };
            self.scroll = 0;
        }
    }

    pub fn total_counts(&self) -> (u32, u32) {
        self.files
            .iter()
            .fold((0, 0), |(a, d), f| {
                let (fa, fd) = f.counts();
                (a + fa, d + fd)
            })
    }
}

/// One side of a row: line number gutter, marker, text.
///
/// A removed line gets the deep-red tint behind it. That tint is the only background on the
/// whole screen, which is exactly why it reads instantly.
fn cell_line(cell: &SideCell, half: usize, lineno_w: usize) -> Line<'static> {
    match cell {
        SideCell::Blank => Line::from(Span::styled(" ".repeat(half), Style::default())),
        SideCell::Fold { count } => {
            let txt = if *count == 0 {
                "    ⋯ hunk ⋯".to_string()
            } else {
                format!("    ⋯ {count} unchanged ⋯")
            };
            Line::from(Span::styled(fit(&txt, half), t::faint()))
        }
        SideCell::Line { lineno, text, kind } => {
            let num = match lineno {
                Some(n) => format!("{n:>lineno_w$} "),
                None => " ".repeat(lineno_w + 1),
            };
            let (style, marker) = match kind {
                LineKind::Added => (t::good(), '+'),
                LineKind::Removed => (t::accent().bg(t::RED_DEEP), '-'),
                LineKind::Context => (t::muted(), ' '),
            };
            let body_w = half.saturating_sub(lineno_w + 2);
            let marker_style = match kind {
                LineKind::Added => t::good(),
                LineKind::Removed => t::accent(),
                LineKind::Context => t::faint(),
            };
            Line::from(vec![
                Span::styled(num, t::faint()),
                Span::styled(marker.to_string(), marker_style),
                Span::styled(
                    format!("{:<width$}", fit(text, body_w), width = body_w),
                    style,
                ),
            ])
        }
    }
}

/// Widths of the two halves and the line-number gutters, for a given area.
fn geometry(area: Rect) -> (usize, usize, usize, usize) {
    // 3 = divider (" │ ") between the halves.
    let half = (area.width.saturating_sub(3) / 2) as usize;
    let old_w = 4.min(half.saturating_sub(2));
    let new_w = 4.min(half.saturating_sub(2));
    (half, half, old_w, new_w)
}

/// The file rail: every changed file with its counts, selected one in red.
fn render_rail(frame: &mut Frame, area: Rect, state: &DiffReviewState) {
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(format!(" {}", fit("FILES", area.width as usize - 1)), t::faint())),
    ];
    let inner = area.width.saturating_sub(2) as usize;
    for (i, file) in state.files.iter().enumerate() {
        let selected = i == state.file_index;
        let (add, del) = file.counts();
        let name = fit(file.display_path(), inner.saturating_sub(9));
        let pad = inner.saturating_sub(name.chars().count() + 9);
        lines.push(Line::from(vec![
            Span::styled(if selected { "▏" } else { " " }, if selected { t::accent() } else { t::faint() }),
            Span::styled(" ", Style::default()),
            Span::styled(
                name,
                if selected { t::title() } else { t::muted() },
            ),
            Span::styled(" ".repeat(pad), Style::default()),
            Span::styled(format!("+{add}"), t::good()),
            Span::styled(" ", Style::default()),
            Span::styled(format!("-{del}"), t::accent()),
        ]));
    }
    if state.files.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(" working tree clean", t::faint())));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// The hunk body, without chrome.
pub fn render(frame: &mut Frame, area: Rect, state: &DiffReviewState) {
    let Some(file) = state.current() else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("  no changes to review", t::faint()))),
            area,
        );
        return;
    };

    // A one-line title for the file being read, then the hunks. No border.
    let (add, del) = file.counts();
    let title = Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::styled(fit(file.display_path(), area.width as usize / 2), t::title()),
        Span::styled(format!("  +{add} "), t::good()),
        Span::styled(format!("-{del} "), t::accent()),
        Span::styled(
            format!("  [{}/{}]", state.file_index + 1, state.files.len()),
            t::faint(),
        ),
    ]);
    let chunks = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(Paragraph::new(title), chunks[0]);

    let (half, _half2, old_w, new_w) = geometry(chunks[1]);
    let rows = state.rows();
    let visible = chunks[1].height as usize;
    let lines: Vec<Line> = rows
        .iter()
        .skip(state.scroll as usize)
        .take(visible)
        .map(|r| {
            let mut l = cell_line(&r.left, half, old_w);
            l.spans.push(Span::styled(" │ ", t::rule()));
            l.spans.extend(cell_line(&r.right, half, new_w).spans);
            l
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), chunks[1]);
}

/// The body: rail, hairline, hunks.
pub fn render_body(frame: &mut Frame, area: Rect, state: &DiffReviewState) {
    let (rail, divider, diff_area) = if area.width >= 80 {
        let chunks = Layout::default()
            .direction(ratatui::layout::Direction::Horizontal)
            .constraints([
                Constraint::Length(28),
                Constraint::Length(1),
                Constraint::Min(30),
            ])
            .split(area);
        (Some(chunks[0]), Some(chunks[1]), chunks[2])
    } else {
        (None, None, area)
    };
    if let Some(r) = rail {
        render_rail(frame, r, state);
    }
    if let Some(d) = divider {
        frame.render_widget(Block::default().borders(Borders::LEFT).border_style(t::rule()), d);
    }
    render(frame, diff_area, state);
}

/// Whole-screen version, with the shared frame around it.
pub fn render_screen(frame: &mut Frame, area: Rect, state: &DiffReviewState) {
    let (add, del) = state.total_counts();
    let chrome = Chrome::new("main", &chrome::TABS, TAB_DIFF)
        .with_branch_status()
        .status(Segment::new(format!("+{add}"), t::good()))
        .status(Segment::new(format!("-{del}"), t::accent()))
        .footer(Segment::chip(format!("{} file(s) changed", state.files.len())))
        .footer(Segment::chip(format!("{} context", state.context)))
        .hint("n/N file · j/k scroll · space fold · q quit");

    let body = chrome::frame(frame, area, &chrome);
    render_body(frame, body, state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_line_diff() -> Vec<FileDiff> {
        diff::parse("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n keep\n-old\n+new\n").unwrap()
    }

    #[test]
    fn navigation_wraps_both_directions() {
        let files = two_line_diff();
        let mut s = DiffReviewState::new(vec![files[0].clone(), files[0].clone()]);
        assert_eq!(s.file_index, 0);
        s.prev_file();
        assert_eq!(s.file_index, 1, "prev from 0 wraps to the end");
        s.next_file();
        assert_eq!(s.file_index, 0, "next from the end wraps to 0");
    }

    #[test]
    fn empty_state_reports_no_changes_without_panicking() {
        let s = DiffReviewState::new(vec![]);
        assert!(s.current().is_none());
        assert!(s.rows().is_empty());
        assert_eq!(s.total_counts(), (0, 0));
    }

    #[test]
    fn rows_come_from_the_core_model() {
        let s = DiffReviewState::new(two_line_diff());
        let rows = s.rows();
        assert!(rows.iter().any(|r| matches!(r.left, SideCell::Line { kind: LineKind::Removed, .. })));
        assert!(rows.iter().any(|r| matches!(r.right, SideCell::Line { kind: LineKind::Added, .. })));
    }

    #[test]
    fn a_removed_line_is_the_only_tinted_thing_on_the_screen() {
        let s = DiffReviewState::new(two_line_diff());
        let area = Rect { x: 0, y: 0, width: 100, height: 20 };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|f| {
                render_body(f, area, &s);
            })
            .unwrap();

        let mut tinted = 0;
        for cell in terminal.backend().buffer().content() {
            if cell.bg == t::RED_DEEP {
                tinted += 1;
            }
        }
        assert!(tinted > 0, "a removed line must be tinted");
        for cell in terminal.backend().buffer().content() {
            if let ratatui::style::Color::Rgb(r, g, b) = cell.bg {
                let is_tint = (r, g, b) == {
                    match t::RED_DEEP {
                        ratatui::style::Color::Rgb(r, g, b) => (r, g, b),
                        _ => unreachable!(),
                    }
                };
                assert!(is_tint || (r == 0 && g == 0 && b == 0), "an unexpected background: ({r},{g},{b})");
            }
        }
    }

    #[test]
    fn every_rendered_row_fits_the_pane_it_is_given() {
        let s = DiffReviewState::new(two_line_diff());
        for width in [40u16, 60, 80, 120, 160] {
            let area = Rect { x: 0, y: 0, width, height: 10 };
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 10))
                .unwrap();
            // A panic here means a row overflowed and ratatui rejected it.
            terminal
                .draw(|f| {
                    render_body(f, area, &s);
                })
                .unwrap();
        }
    }

    #[test]
    fn the_rail_lists_every_file_with_its_counts() {
        let files = two_line_diff();
        let s = DiffReviewState::new(files);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 10)).unwrap();
        terminal
            .draw(|f| {
                render_screen(f, f.area(), &s);
            })
            .unwrap();
        let screen: String =
            terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(screen.contains("a.rs"), "{screen}");
        assert!(screen.contains("FILES"), "{screen}");
        assert!(screen.contains("diff"), "the frame names the active tab: {screen}");
    }
}
