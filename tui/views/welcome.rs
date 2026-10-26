//! First-run landing screen: a small black-hole sigil, a clear mission model, and a quiet
//! route into the rest of the workspace. Status is shown only when the caller supplies it.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{blackhole, fit};
use crate::chrome::{self, Chrome, Segment};
use crate::theme as t;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WelcomeInfo {
    pub workspace: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub recent_mission: Option<String>,
}

fn centered(text: &str, width: usize, style: Style) -> Line<'static> {
    Line::from(Span::styled(fit(text, width), style))
}

fn detail(label: &str, value: &str, width: usize) -> Line<'static> {
    let label = format!("{label}  ");
    let label_width = label.chars().count().min(width);
    if label_width == width {
        return Line::from(Span::styled(fit(&label, width), t::faint()));
    }
    Line::from(vec![
        Span::styled(fit(&label, width), t::faint()),
        Span::styled(fit(value, width - label_width), t::muted()),
    ])
}

/// Draw the welcome body without a frame; `render_screen` supplies the shared borderless
/// chrome. At narrow sizes the copy shortens instead of wrapping into a wall of text.
pub fn render(frame: &mut Frame, area: Rect, info: &WelcomeInfo) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = area.width as usize;
    let compact = width < 64;
    let mut lines = vec![
        blackhole::sigil(),
        Line::from(""),
        centered(
            "STOIC CODEX",
            width,
            t::title().add_modifier(Modifier::BOLD),
        ),
        centered(
            if compact {
                "Local-first · BYOK"
            } else {
                "Local-first coding missions · bring your own models"
            },
            width,
            t::muted(),
        ),
        if compact {
            Line::from(vec![
                Span::styled("Plan", t::accent()),
                Span::styled(" · execute · ", t::muted()),
                Span::styled("verify", Style::default().fg(t::PHOTON)),
            ])
        } else {
            Line::from(vec![
                Span::styled("Strategos", t::accent()),
                Span::styled(" plans · The Phalanx executes · ", t::muted()),
                Span::styled("you verify", Style::default().fg(t::PHOTON)),
            ])
        },
        Line::from(Span::styled(
            "─".repeat(width.min(if compact { 24 } else { 36 })),
            t::rule_accent(),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Enter", t::accent().add_modifier(Modifier::BOLD)),
            Span::styled("  begin a mission", t::text()),
        ]),
        Line::from(vec![
            Span::styled("?", t::accent().add_modifier(Modifier::BOLD)),
            Span::styled("  open the key guide", t::muted()),
        ]),
    ];

    let mut details: Vec<(String, String)> = Vec::new();
    if let Some(workspace) = &info.workspace {
        details.push(("workspace".into(), workspace.clone()));
    }
    match (&info.provider, &info.model) {
        (Some(provider), Some(model)) => {
            details.push(("model".into(), format!("{provider} · {model}")))
        }
        (Some(provider), None) => details.push(("provider".into(), provider.clone())),
        (None, Some(model)) => details.push(("model".into(), model.clone())),
        (None, None) => {}
    }
    if let Some(recent) = &info.recent_mission {
        details.push(("recent".into(), recent.clone()));
    }
    if !details.is_empty() {
        lines.push(Line::from(""));
        for (label, value) in details {
            lines.push(detail(&label, &value, width));
        }
    }

    let content_height = (lines.len() as u16).min(area.height);
    let visible_start = if lines.len() as u16 > content_height {
        (lines.len() as u16 - content_height) / 2
    } else {
        0
    } as usize;
    let visible: Vec<Line<'static>> = lines
        .into_iter()
        .skip(visible_start)
        .take(content_height as usize)
        .collect();
    let target = Rect {
        y: area.y + (area.height - content_height) / 2,
        height: content_height,
        ..area
    };
    frame.render_widget(Paragraph::new(visible).alignment(Alignment::Center), target);
}

/// Whole-screen version used by the first-run route and the snapshot generator.
pub fn render_screen(frame: &mut Frame, area: Rect, info: &WelcomeInfo) {
    let chrome = Chrome::new("", &[], 0)
        .status(Segment::new("LOCAL · BYOK", t::muted()))
        .footer(Segment::chip("workspace stays yours"))
        .hint("Enter start · ? help · q quit");
    let body = chrome::frame(frame, area, &chrome);
    render(frame, body, info);
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
    fn welcome_screen_uses_the_black_hole_brand_and_compact_copy() {
        let mut terminal = Terminal::new(TestBackend::new(40, 18)).unwrap();
        terminal
            .draw(|frame| render_screen(frame, frame.area(), &WelcomeInfo::default()))
            .unwrap();
        let text = screen(&terminal);
        assert!(text.contains("STOIC CODEX"), "{text}");
        assert!(text.contains("Local-first"), "{text}");
        assert!(text.contains("verify"), "{text}");
        assert!(text.contains("begin a mission"), "{text}");
        assert!(text.contains("LOCAL · BYOK"), "{text}");
    }

    #[test]
    fn supplied_workspace_and_model_are_fitted_on_a_narrow_screen() {
        let info = WelcomeInfo {
            workspace: Some("/home/user/projects/very-long-workspace-name".into()),
            provider: Some("google".into()),
            model: Some("gemini-3.1-flash-lite".into()),
            recent_mission: Some("refactor-authentication-flow".into()),
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 22)).unwrap();
        terminal
            .draw(|frame| render_screen(frame, frame.area(), &info))
            .unwrap();
        let text = screen(&terminal);
        assert!(text.contains("workspace"), "{text}");
        assert!(text.contains("google"), "{text}");
        assert!(text.contains("recent"), "{text}");
    }
}
