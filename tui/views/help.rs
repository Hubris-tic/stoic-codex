//! Contextual key guide: a quiet, borderless sheet that matches the active view.
//!
//! The app shell owns key dispatch; this module owns the visual sheet so the guide and the
//! footer can share one vocabulary. The panel uses a raised void, hairline rules, and no box.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use super::fit;
use crate::theme as t;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpContext {
    Welcome,
    Chat,
    Mission,
    Diff,
    Memory,
    Hole,
}

impl HelpContext {
    pub fn label(self) -> &'static str {
        match self {
            HelpContext::Welcome => "WELCOME",
            HelpContext::Chat => "CHAT",
            HelpContext::Mission => "MISSION",
            HelpContext::Diff => "DIFF REVIEW",
            HelpContext::Memory => "MEMORY GRAPH",
            HelpContext::Hole => "BLACK HOLE",
        }
    }
}

fn bindings(context: HelpContext) -> &'static [(&'static str, &'static str)] {
    match context {
        HelpContext::Welcome => &[
            ("Enter", "Start a mission"),
            ("?", "Open or close this key guide"),
            ("Esc", "Close this key guide"),
            ("q", "Quit Stoic Codex"),
        ],
        HelpContext::Chat => &[
            ("Enter", "Send the follow-up prompt"),
            ("PgUp / PgDn", "Scroll the conversation"),
            ("?", "Open or close this key guide"),
            ("Esc", "Close the guide or chat"),
            ("Ctrl+C", "Close the chat"),
        ],
        HelpContext::Mission => &[
            ("↑ / ↓ · j / k", "Select an operator"),
            ("Enter", "Open or close operator detail"),
            ("?", "Open or close this key guide"),
            ("Esc", "Close detail or the key guide"),
            ("q", "Close the mission board"),
            ("Ctrl+C", "Close the mission board"),
        ],
        HelpContext::Diff => &[
            ("n / N", "Next / previous file"),
            ("j / k", "Scroll the diff"),
            ("Space", "Fold or unfold context"),
            ("?", "Open or close this key guide"),
            ("q", "Quit Stoic Codex"),
        ],
        HelpContext::Memory => &[
            ("+ / -", "Increase / decrease graph depth"),
            ("j / k", "Scroll the graph"),
            ("Enter", "Open the focused node"),
            ("?", "Open or close this key guide"),
            ("q", "Quit Stoic Codex"),
        ],
        HelpContext::Hole => &[
            ("Space", "Pause or resume the flow"),
            ("j", "Toggle the relativistic jets"),
            ("?", "Open or close this key guide"),
            ("Esc", "Close this key guide"),
            ("q", "Quit Stoic Codex"),
        ],
    }
}

/// Draw the help sheet over the active view. It is centered, borderless, and adapts to a
/// narrow terminal by fitting descriptions rather than wrapping the key column.
pub fn render(frame: &mut Frame, area: Rect, context: HelpContext) {
    let entries = bindings(context);
    let panel_width = area.width.min(76);
    let desired_height = (entries.len() as u16).saturating_add(5);
    let panel_height = area.height.saturating_sub(2).min(desired_height);
    if panel_width < 28 || panel_height < 7 {
        return;
    }

    let panel = Rect {
        x: area.x + (area.width - panel_width) / 2,
        y: area.y + (area.height - panel_height) / 2,
        width: panel_width,
        height: panel_height,
    };
    frame.render_widget(Clear, panel);
    frame.render_widget(
        Block::default().style(Style::default().bg(t::RAISED)),
        panel,
    );

    let inner_width = panel.width.saturating_sub(4) as usize;
    let inner = |y, height| Rect {
        x: panel.x + 2,
        y,
        width: panel.width.saturating_sub(4),
        height,
    };

    let title_room = inner_width.saturating_sub("?  KEY GUIDE".chars().count());
    let title = Line::from(vec![
        Span::styled("? ", t::accent().add_modifier(Modifier::BOLD)),
        Span::styled("KEY GUIDE", t::title()),
        Span::styled(
            fit(&format!(" · {}", context.label()), title_room),
            t::faint(),
        ),
    ]);
    frame.render_widget(Paragraph::new(title), inner(panel.y + 1, 1));
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("─".repeat(inner_width), t::rule()))),
        inner(panel.y + 2, 1),
    );

    let visible = panel.height.saturating_sub(5) as usize;
    let key_width = 10.min(inner_width / 3);
    for (i, (key, action)) in entries.iter().take(visible).enumerate() {
        let key = fit(key, key_width);
        let key = format!("{key:<width$}", width = key_width);
        let description = fit(action, inner_width.saturating_sub(key_width));
        let line = Line::from(vec![
            Span::styled(key, t::accent().add_modifier(Modifier::BOLD)),
            Span::styled(description, t::muted()),
        ]);
        frame.render_widget(Paragraph::new(line), inner(panel.y + 3 + i as u16, 1));
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("? / Esc close", t::faint()))),
        inner(panel.y + panel.height - 2, 1),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn screen(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn mission_sheet_explains_the_contextual_keys_without_a_box() {
        let mut terminal = Terminal::new(TestBackend::new(40, 18)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), HelpContext::Mission))
            .unwrap();
        let text = screen(&terminal);
        assert!(text.contains("KEY GUIDE"), "{text}");
        assert!(text.contains("MISSION"), "{text}");
        assert!(text.contains("Select an operator"), "{text}");
        assert!(text.contains("? / Esc close"), "{text}");
    }

    #[test]
    fn chat_sheet_explains_the_follow_up_input_and_scroll_keys() {
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), HelpContext::Chat))
            .unwrap();
        let text = screen(&terminal);
        assert!(text.contains("CHAT"), "{text}");
        assert!(text.contains("Send the follow-up prompt"), "{text}");
        assert!(text.contains("Scroll the conversation"), "{text}");
    }

    #[test]
    fn narrow_help_sheet_covers_the_underlying_view_edge_to_edge() {
        let mut terminal = Terminal::new(TestBackend::new(40, 18)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                frame.render_widget(
                    Paragraph::new("~".repeat(area.width as usize)),
                    Rect {
                        x: area.x,
                        y: area.y + 8,
                        width: area.width,
                        height: 1,
                    },
                );
                render(frame, area, HelpContext::Mission);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut row = buffer.content().iter().skip(8 * 40).take(40);
        assert!(
            row.all(|cell| cell.symbol() != "~"),
            "underlying content leaked at the edge of a compact help sheet"
        );
    }

    #[test]
    fn help_sheet_fits_a_compact_terminal() {
        for (width, height) in [(40, 18), (60, 20), (120, 30)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| render(frame, frame.area(), HelpContext::Diff))
                .unwrap();
            assert!(
                screen(&terminal).contains("KEY GUIDE"),
                "missing sheet at {width}x{height}"
            );
        }
    }
}
