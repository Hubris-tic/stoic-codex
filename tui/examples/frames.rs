//! Dump one turn of the black hole as a strip of frames, so the animation can be watched
//! without a terminal.
//!
//! `cargo run --example snapshot` proves what one screen looks like. This proves that the
//! screen *moves*: the same view, rendered at a series of clock values, written as the same
//! JSON cell grid the snapshot example uses. `render_preview.py --gif` turns the strip into
//! `preview/hole.gif`.
//!
//!     cargo run --example frames
//!     python3 render_preview.py --gif

use std::fs;
use std::path::Path;

use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::{Frame, Terminal};
use stoic_codex_tui_views::views::blackhole::{self, BlackHoleAnim};

/// A small frame: a GIF of a 132x34 screen is a 40 MB file, and the point is the motion.
const W: u16 = 84;
const H: u16 = 22;
/// One full swirl before the pattern repeats. 48 frames is 7.5 degrees of phase a frame:
/// enough that the shear reads as motion rather than as a slideshow.
const FRAMES: usize = 48;

fn main() {
    let out = Path::new("preview/frames");
    fs::create_dir_all(out).expect("create preview/frames");
    let mut anim = BlackHoleAnim::new();
    for index in 0..FRAMES {
        anim.phase = std::f64::consts::TAU * index as f64 / FRAMES as f64;
        let mut terminal = Terminal::new(TestBackend::new(W, H)).expect("test backend");
        terminal
            .draw(|frame: &mut Frame| {
                let area = frame.area();
                blackhole::render_screen(frame, area, &mut anim);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let mut rows: Vec<String> = Vec::with_capacity(H as usize);
        for y in 0..H {
            let mut row: Vec<String> = Vec::with_capacity(W as usize);
            for x in 0..W {
                let cell = &buffer[(x, y)];
                row.push(format!(
                    "[{},{},{},{}]",
                    quote(cell.symbol()),
                    quote(&colour(cell.fg).unwrap_or_else(|| "#e8e4e0".into())),
                    quote(&colour(cell.bg).unwrap_or_else(|| "#05060a".into())),
                    if cell.modifier.contains(ratatui::style::Modifier::BOLD) { 1 } else { 0 }
                ));
            }
            rows.push(format!("    [{}]", row.join(",")));
        }
        let json = format!(
            "{{\n  \"name\": \"hole-frame-{index:02}\",\n  \"width\": {W},\n  \"height\": {H},\n  \"cells\": [\n{}\n  ]\n}}\n",
            rows.join(",\n")
        );
        fs::write(out.join(format!("frame-{index:02}.json")), json).expect("write frame");
    }
    println!("wrote {FRAMES} frames to {}/", out.display());
}

fn colour(c: Color) -> Option<String> {
    match c {
        Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
        Color::Reset => None,
        other => Some(format!("{other:?}")),
    }
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
