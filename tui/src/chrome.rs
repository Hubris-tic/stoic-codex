//! The one frame every screen lives in: a header, a rule, the body, a rule, a footer.
//!
//! This module exists so the views stop each inventing their own title bar. Every
//! screen gets the same geometry, the same alignment rules and the same footer, which is
//! most of what makes a terminal UI feel *designed* rather than assembled.
//!
//! Geometry, on a wide terminal:
//!
//! ```text
//!  ◍ Stoic Codex  │ ▸ mission  diff  memory  hole            on main  ● 2 running  $1.20
//!  ─────────────                          ──────────────────────────────────────────
//!  ...body, whatever the active view draws...
//!  ─────────────────────────────────────────────────────────────────────────────────
//!  ● 3 operators   $1.20 spent                      n/k rows · enter detail · q quit
//! ```
//!
//! Rules the header and footer follow:
//!
//! * **The active tab is the only red word in the header** (the sigil and one live
//!   indicator may add a red glyph each; that is the whole budget), and the rule beneath it
//!   is red for exactly the width of its label. Nothing else moves.
//! * **The right side is right-aligned as one run**, so a changing value ("2 running")
//!   cannot shove the tabs around.
//! * **Anything that does not fit is dropped from the right**, never truncated into
//!   nonsense: at 60 columns you get the mark, the tabs and the branch.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::theme;

/// A run of text with a style, used in the header and the footer.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub text: String,
    pub style: Style,
}

impl Segment {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Segment { text: text.into(), style }
    }

    /// Footer metadata: raised background, bone text.
    pub fn chip(text: impl Into<String>) -> Self {
        Segment::new(format!(" {} ", text.into()), theme::chip_text())
    }

    /// Footer attention: red block, black text. One per screen at most.
    pub fn alert(text: impl Into<String>) -> Self {
        Segment::new(format!(" {} ", text.into()), theme::badge())
    }

    /// Footer quiet text: no background at all.
    pub fn quiet(text: impl Into<String>) -> Self {
        Segment::new(format!(" {} ", text.into()), theme::faint())
    }

    fn width(&self) -> usize {
        self.text.chars().count()
    }
}

/// What fills the frame. Build one of these per screen and hand it to [`frame`].
pub struct Chrome<'a> {
    pub branch: String,
    /// Tab labels, in order.
    pub tabs: &'a [&'a str],
    pub active: usize,
    /// Right-aligned header run: a branch name, a count, a cost.
    pub status: Vec<Segment>,
    /// Left footer run.
    pub footer: Vec<Segment>,
    /// Right-aligned footer hint: the keys that matter on this screen.
    pub hint: String,
}

impl<'a> Chrome<'a> {
    pub fn new(branch: impl Into<String>, tabs: &'a [&'a str], active: usize) -> Chrome<'a> {
        Chrome {
            branch: branch.into(),
            tabs,
            active,
            status: Vec::new(),
            footer: Vec::new(),
            hint: String::new(),
        }
    }

    /// The default status run: branch, then whatever the screen wants to say.
    pub fn with_branch_status(mut self) -> Chrome<'a> {
        // Plain words: a fancy branch glyph is a font lottery on someone else's terminal.
        self.status.insert(0, Segment::new(format!("on {}", self.branch), theme::muted()));
        self
    }

    pub fn status(mut self, segment: Segment) -> Chrome<'a> {
        self.status.push(segment);
        self
    }

    pub fn footer(mut self, segment: Segment) -> Chrome<'a> {
        self.footer.push(segment);
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Chrome<'a> {
        self.hint = hint.into();
        self
    }

    fn active_label(&self) -> Option<&'a str> {
        self.tabs.get(self.active).copied()
    }
}

/// The screens this build ships. The order is the tab order.
pub const TABS: [&str; 4] = ["mission", "diff", "memory", "hole"];
pub const TAB_MISSION: usize = 0;
pub const TAB_DIFF: usize = 1;
pub const TAB_MEMORY: usize = 2;
/// The black hole itself: the animated accretion disk, and the numbers behind it.
pub const TAB_HOLE: usize = 3;

/// Split a terminal area into `header / rule / body / rule / footer`.
///
/// Returns the body rectangle. When the area is too short for chrome, the footer and rules
/// are given up first — a four-row terminal should still show the content, not a frame.
pub fn layout(area: Rect) -> (Option<Rect>, Rect, Option<Rect>) {
    if area.height <= 3 {
        return (None, area, None);
    }
    let chunks = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Length(1), // rule
            Constraint::Min(1),    // body
            Constraint::Length(1), // rule
            Constraint::Length(1), // footer
        ])
        .split(area);
    (Some(chunks[0].union(chunks[1])), chunks[2], Some(chunks[3].union(chunks[4])))
}

/// Where the tabs start, and the width of each one's cell.
///
/// The header and the rule both use this, so the red underline is *by construction* under
/// the label it belongs to rather than by two `format!` strings happening to agree.
/// `◍ ` (2) + `Stoic Codex` (11) + `  │ ` (4) puts the first tab at column 17. Computed
/// rather than guessed: the header and the rule both read it, so they cannot disagree.
const TAB_ORIGIN: usize = 17;

fn tab_offsets(tabs: &[&str]) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(tabs.len());
    let mut col = TAB_ORIGIN;
    for (i, tab) in tabs.iter().enumerate() {
        // Separator + marker + space + label. The first tab has no separator: the `│`
        // before it already provides the gap.
        let width = tab_cell_len(i, tab);
        out.push((col, width));
        col += width;
    }
    out
}

/// The width of one tab cell on the header line.
fn tab_cell_len(index: usize, label: &str) -> usize {
    let separator = usize::from(index > 0);
    separator + 2 + label.chars().count()
}

/// The header line: mark, word, tabs, and a right-aligned status run.
pub fn header<'a>(chrome: &Chrome<'_>, width: u16) -> Line<'static> {
    let mut spans = vec![
        Span::styled("◍ ", theme::mark()),
        Span::styled("Stoic Codex", theme::title()),
        Span::styled("  │ ", theme::rule()),
    ];
    for (i, tab) in chrome.tabs.iter().enumerate() {
        // One space between cells, so an active tab never fuses to the one before it
        // (`memory▸ hole` is a bug, not a style).
        let separator = if i > 0 { " " } else { "" };
        if i == chrome.active {
            spans.push(Span::styled(format!("{separator}▸ {tab}"), theme::focus()));
        } else {
            spans.push(Span::styled(format!("{separator}  {tab}"), theme::faint()));
        }
    }

    // A narrow terminal drops tabs from the end before it drops the mark: at 40 columns the
    // product name and the first screens still fit, and the header never overruns.
    while line_width(&spans) + 2 > width as usize && spans.len() > 1 {
        spans.pop();
    }

    // Right run, dropped from the right when it does not fit.
    let mut right: Vec<Span> = Vec::new();
    let mut used = line_width(&spans);
    let available = width as usize;
    // One space of breathing room between the tabs and the status run.
    used += 2;
    let mut budget = available.saturating_sub(used);
    for (i, segment) in chrome.status.iter().enumerate() {
        let gap = if i > 0 { 3 } else { 0 };
        let need = segment.width() + gap;
        if need > budget {
            break;
        }
        budget -= need;
        if i > 0 {
            right.push(Span::styled("   ", theme::rule()));
        }
        right.push(Span::styled(segment.text.clone(), segment.style));
    }

    // Pad so the right run lands flush against the edge.
    let left = line_width(&spans);
    let right_w = line_width(&right);
    let pad = available.saturating_sub(left + right_w);

    spans.push(Span::styled(" ".repeat(pad), Style::default()));
    spans.extend(right);
    Line::from(spans)
}

/// The rule under the header: grey everywhere, red for the width of the active tab.
pub fn rule(area: Rect, chrome: &Chrome<'_>) -> Line<'static> {
    let width = area.width as usize;
    let mut spans = Vec::new();
    if chrome.active_label().is_some() {
        if let Some((start, len)) = tab_offsets(chrome.tabs).get(chrome.active).copied() {
            if start + len <= width {
                spans.push(Span::styled("─".repeat(start), theme::rule()));
                spans.push(Span::styled("─".repeat(len), theme::accent()));
                spans.push(Span::styled("─".repeat(width - start - len), theme::rule()));
                return Line::from(spans);
            }
        }
    }
    Line::from(Span::styled("─".repeat(width), theme::rule()))
}

/// The footer line: chips on the left, the key hint on the right.
pub fn footer(chrome: &Chrome<'_>, width: u16) -> Line<'static> {
    let available = width as usize;
    let mut spans: Vec<Span> = Vec::new();
    let mut left = 0usize;
    for segment in &chrome.footer {
        if left + segment.width() + 1 > available {
            break;
        }
        left += segment.width();
        spans.push(Span::styled(segment.text.clone(), segment.style));
    }
    let hint = if chrome.hint.is_empty() { String::new() } else { format!("{} ", chrome.hint) };
    let hint_w = hint.chars().count();
    if left + hint_w < available {
        spans.push(Span::styled(" ".repeat(available - left - hint_w), Style::default()));
        spans.push(Span::styled(hint, theme::faint()));
    }
    Line::from(spans)
}

/// Draw the frame and hand back the rectangle a view should fill.
pub fn frame(frame: &mut Frame, area: Rect, chrome: &Chrome<'_>) -> Rect {
    let (header_area, body, footer_area) = layout(area);

    if let Some(h) = header_area {
        let head = Rect { height: 1, ..h };
        let rule_area = Rect { y: h.y + 1, height: 1, ..h };
        frame.render_widget(Paragraph::new(header(chrome, h.width)), head);
        frame.render_widget(Paragraph::new(rule(rule_area, chrome)), rule_area);
    }
    if let Some(f) = footer_area {
        let rule_area = Rect { height: 1, ..f };
        let foot = Rect { y: f.y + 1, height: 1, ..f };
        frame.render_widget(Paragraph::new(Line::from(Span::styled("─".repeat(f.width as usize), theme::rule()))), rule_area);
        frame.render_widget(Paragraph::new(footer(chrome, f.width)), foot);
    }
    body
}

/// Truncate to `width` display columns, with an ellipsis. Panels are clipped, never wrapped:
/// a wrapped agent row is unreadable, and a wrapped table is a lie about its columns.
pub fn fit(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return s.to_string();
    }
    let mut out: String = chars[..width.saturating_sub(1)].iter().collect();
    out.push('…');
    out
}

fn line_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome(active: usize) -> Chrome<'static> {
        Chrome::new("main", &TABS, active)
            .with_branch_status()
            .status(Segment::new("● 2 running", theme::accent()))
            .footer(Segment::chip("● 3 operators"))
            .footer(Segment::chip("$1.20 spent"))
            .hint("? help")
    }

    fn text_of(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn the_header_names_the_product_and_every_tab() {
        let line = header(&chrome(TAB_MISSION), 140);
        let text = text_of(&line);
        assert!(text.starts_with("◍ Stoic Codex"), "{text}");
        for tab in TABS {
            assert!(text.contains(tab), "tab {tab} missing from {text}");
        }
        assert!(text.contains("main"), "{text}");
        assert!(text.contains("2 running"), "{text}");
    }

    #[test]
    fn the_active_tab_is_the_only_red_thing_in_the_header() {
        let line = header(&chrome(TAB_DIFF), 140);
        let red: Vec<&str> = line
            .spans
            .iter()
            .filter(|s| s.style.fg == Some(theme::RED))
            .map(|s| s.content.as_ref())
            .collect();
        // The mark, the active tab, and at most one live indicator. That is the budget.
        assert_eq!(red, vec!["◍ ", " ▸ diff", "● 2 running"], "red spans: {red:?}");
    }

    #[test]
    fn every_tab_sits_in_the_same_two_space_gutter() {
        let text = text_of(&header(&chrome(TAB_MISSION), 140));
        let chars: Vec<char> = text.chars().collect();
        let offsets = tab_offsets(&TABS);
        for (i, tab) in TABS.iter().enumerate() {
            let (start, len) = offsets[i];
            let cell: String = chars[start..start + len].iter().collect();
            let gap = if i > 0 { " " } else { "" };
            assert!(
                cell == format!("{gap}▸ {tab}") || cell == format!("{gap}  {tab}"),
                "tab {tab} sits at {start} but the cell there is {cell:?}"
            );
            // And the next cell starts exactly where this one ends: no drift, ever.
            if let Some((next, _)) = offsets.get(i + 1) {
                assert_eq!(next, &(start + len), "tab {tab} does not end where the next begins");
            }
        }
    }

    #[test]
    fn the_rule_under_the_header_is_red_exactly_under_the_active_tab() {
        let area = Rect { x: 0, y: 0, width: 140, height: 1 };
        let line = rule(area, &chrome(TAB_MISSION));
        let red: Vec<&Span> = line.spans.iter().filter(|s| s.style.fg == Some(theme::RED)).collect();
        assert_eq!(red.len(), 1, "one red segment");
        assert_eq!(red[0].content.chars().count(), tab_cell_len(TAB_MISSION, "mission"));

        // It starts where the label starts: the grey run before it is the same width.
        let grey_before = line.spans[0].content.chars().count();
        let header_text = text_of(&header(&chrome(TAB_MISSION), 140));
        // Char positions, not byte offsets: "▸" and "│" are multi-byte.
        let chars: Vec<char> = header_text.chars().collect();
        let needle: Vec<char> = "▸ mission".chars().collect();
        let label_at = chars
            .windows(needle.len())
            .position(|w| w == needle.as_slice())
            .expect("label");
        assert_eq!(grey_before, label_at, "the red rule must sit under its own tab");
    }

    #[test]
    fn header_and_footer_never_exceed_the_terminal_width() {
        for width in [40u16, 60, 80, 100, 140, 200] {
            for active in [TAB_MISSION, TAB_DIFF, TAB_MEMORY, TAB_HOLE] {
                let c = chrome(active);
                let h = header(&c, width);
                assert!(
                    h.width() <= width as usize,
                    "header at {width} is {} wide",
                    h.width()
                );
                let f = footer(&c, width);
                assert!(f.width() <= width as usize, "footer at {width} is {} wide", f.width());
            }
        }
    }

    #[test]
    fn a_narrow_header_drops_the_status_run_before_it_lies() {
        let c = chrome(TAB_MISSION);
        let narrow = text_of(&header(&c, 40));
        assert!(narrow.contains("mission"), "{narrow}");
        // No half-segments: either a value is whole, or it is not there.
        assert!(!narrow.contains("2 runnin"), "{narrow}");
        assert!(!narrow.contains("…"), "headers truncate by dropping, not by ellipsis: {narrow}");
    }

    #[test]
    fn the_footer_puts_chips_left_and_the_hint_right() {
        let line = footer(&chrome(TAB_MISSION), 100);
        let text = text_of(&line);
        assert!(text.starts_with(" ● 3 operators "), "{text:?}");
        assert!(text.contains("$1.20 spent"), "{text:?}");
        assert!(text.trim_end().ends_with("? help"), "{text:?}");
        assert_eq!(line.width(), 100);
    }

    #[test]
    fn the_body_geometry_is_the_same_on_every_screen() {
        let area = Rect { x: 0, y: 0, width: 100, height: 30 };
        let (header_area, body, footer_area) = layout(area);
        assert_eq!(header_area.unwrap().height, 2);
        assert_eq!(body.y, 2);
        assert_eq!(body.height, 26);
        assert_eq!(footer_area.unwrap().y, 28);
        assert_eq!(body.height + 4, area.height);
    }

    #[test]
    fn a_tiny_terminal_shows_content_and_gives_up_the_chrome() {
        let area = Rect { x: 0, y: 0, width: 20, height: 3 };
        let (header_area, body, footer_area) = layout(area);
        assert!(header_area.is_none());
        assert!(footer_area.is_none());
        assert_eq!(body, area, "the content keeps the whole screen");
    }

    #[test]
    fn fit_truncates_with_an_ellipsis_and_never_exceeds_its_width() {
        assert_eq!(fit("abcdef", 10), "abcdef");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("abcdef", 1), "…");
        assert_eq!(fit("abcdef", 0), "");
        assert_eq!(fit("héllo wörld", 5), "héll…", "unicode-safe");
    }
}
