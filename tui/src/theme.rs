//! The Stoic Codex palette: **a black hole, drawn in red**.
//!
//! The identity is one object — a black hole — so the palette is the physics of one:
//!
//! * **Void** is the page and the event horizon. Near-black with a cold blue bias (`#05060a`),
//!   not grey: the screen should look like it is absorbing light, not like a dim window.
//! * **Ember / red** is the accretion disk and everything live: the mark, the active tab, the
//!   selected row, a failing state. Red is still the only *primary* colour, as it has been
//!   throughout this project — the black hole just gave it a reason to be orange at the hot
//!   end.
//! * **Photon** is the light that escaped: a cold blue-white used for exactly one thing per
//!   screen — accepted work, an added line, the photon ring.
//! * **Grey** carries ~90% of the glyphs. That is what keeps a dense screen calm.
//!
//! Every colour is an explicit RGB triple, never the terminal's `Red`/`Green` slots: a
//! 16-colour theme would render this design as something else entirely on someone else's
//! machine.

use ratatui::style::{Color, Modifier, Style};

// ── the void ────────────────────────────────────────────────────────────────────────────

/// The page, and the inside of the horizon. Cold, because space is.
pub const VOID: Color = Color::Rgb(0x05, 0x06, 0x0a);
/// One step up: a selected row. Almost invisible on purpose.
pub const RAISED: Color = Color::Rgb(0x12, 0x14, 0x1d);
/// Hairlines: rules, dividers, gutters.
pub const LINE: Color = Color::Rgb(0x23, 0x26, 0x33);
/// A red-tinted hairline, for the rule under the boss row or an active boundary.
pub const LINE_RED: Color = Color::Rgb(0x46, 0x1c, 0x22);

// ── the accretion disk: red, and its hot end ─────────────────────────────────────────────

/// Primary red. Identity, active state, focus.
pub const RED: Color = Color::Rgb(0xe5, 0x48, 0x4d);
/// Red for text that must read as a failure without shouting.
pub const RED_HOT: Color = Color::Rgb(0xff, 0x64, 0x5c);
/// Deep red: backgrounds behind primary text, and "gone" in a diff.
pub const RED_DEEP: Color = Color::Rgb(0x35, 0x13, 0x18);
/// The hot inner edge of the disk. Used sparingly — a few glyphs, never a paragraph.
pub const EMBER: Color = Color::Rgb(0xff, 0x9a, 0x4d);

// ── the light that escaped ───────────────────────────────────────────────────────────────

/// The photon ring. Cold blue-white: the one cool accent, for accepted work and additions.
pub const PHOTON: Color = Color::Rgb(0x9c, 0xc7, 0xff);
/// Darker photon blue, for the far side of the ring and for "unreviewed".
pub const PHOTON_DIM: Color = Color::Rgb(0x5a, 0x7f, 0xb8);
/// Relativistic jet blue, used only by the animation.
pub const JET: Color = Color::Rgb(0x6f, 0x8b, 0xd8);

// ── what carries the words ──────────────────────────────────────────────────────────────

/// Primary text. Warm off-white: white on black is a headache at 2am.
pub const BONE: Color = Color::Rgb(0xe8, 0xe4, 0xe0);
/// Secondary text: values, labels, anything read once.
pub const GREY: Color = Color::Rgb(0x99, 0x97, 0xa3);
/// Tertiary: metadata, hints, line numbers, gutters.
pub const FAINT: Color = Color::Rgb(0x5e, 0x5c, 0x68);

// ── semantic styles ─────────────────────────────────────────────────────────────────────
//
// Views ask for meaning ("this is the selected row", "this is a value"), never for a colour.
// That is what keeps the palette swappable and the meaning consistent across screens.

/// The product mark: red, bold, no decoration.
pub fn mark() -> Style {
    Style::default().fg(RED).add_modifier(Modifier::BOLD)
}

/// The word after the mark, and screen titles.
pub fn title() -> Style {
    Style::default().fg(BONE).add_modifier(Modifier::BOLD)
}

/// Body text.
pub fn text() -> Style {
    Style::default().fg(BONE)
}

/// Values, secondary reading.
pub fn muted() -> Style {
    Style::default().fg(GREY)
}

/// Labels, metadata, hints.
pub fn faint() -> Style {
    Style::default().fg(FAINT)
}

/// Anything the user is on right now.
pub fn focus() -> Style {
    Style::default().fg(RED).add_modifier(Modifier::BOLD)
}

/// A row that is selected: raised background, bone text.
pub fn selected() -> Style {
    Style::default().fg(BONE).bg(RAISED)
}

/// A field label: the word "branch" before the branch.
pub fn label() -> Style {
    Style::default().fg(FAINT)
}

/// Hairlines.
pub fn rule() -> Style {
    Style::default().fg(LINE)
}

/// The hairline that separates the boss from its Operator.
pub fn rule_accent() -> Style {
    Style::default().fg(LINE_RED)
}

/// Accepted / added. Cold, so it never competes with the disk.
pub fn good() -> Style {
    Style::default().fg(PHOTON)
}

/// Needs attention: amber-white, the colour of something about to fall in.
pub fn warn() -> Style {
    Style::default().fg(EMBER)
}

/// Failed.
pub fn bad() -> Style {
    Style::default().fg(RED_HOT)
}

/// Primary red text without the weight of `focus()`.
pub fn accent() -> Style {
    Style::default().fg(RED)
}

/// A solid red block carrying void-black text — at most one segment per screen.
pub fn badge() -> Style {
    Style::default().fg(VOID).bg(RED).add_modifier(Modifier::BOLD)
}

/// A dim filled segment, for the footer.
pub fn chip() -> Style {
    Style::default().fg(GREY).bg(RAISED)
}

/// Grey text on the raised chip.
pub fn chip_text() -> Style {
    Style::default().fg(BONE).bg(RAISED)
}

/// Map a heat from 0.0 (outer disk, cool) to 1.0 (inner edge, white hot) onto the disk ramp.
///
/// This is the palette as a function: the animation asks for "how hot is this bit of the
/// disk" and gets a colour back, instead of inventing one. The ramp is red → ember → bone,
/// so the hottest part of the disk is the closest thing on screen to the photon ring.
pub fn heat(h: f32) -> Color {
    let h = h.clamp(0.0, 1.0);
    if h < 0.45 {
        blend(RED_DEEP, RED, h / 0.45)
    } else if h < 0.8 {
        blend(RED, EMBER, (h - 0.45) / 0.35)
    } else {
        blend(EMBER, BONE, (h - 0.8) / 0.2)
    }
}

/// Linear blend between two RGB colours, for the ramp above.
fn blend(a: Color, b: Color, t: f32) -> Color {
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    let t = t.clamp(0.0, 1.0);
    Color::Rgb(
        (ar as f32 + (br as f32 - ar as f32) * t) as u8,
        (ag as f32 + (bg as f32 - ag as f32) * t) as u8,
        (ab as f32 + (bb as f32 - ab as f32) * t) as u8,
    )
}

fn rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luma(c: Color) -> f32 {
        let (r, g, b) = rgb(c);
        0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32
    }

    #[test]
    fn every_colour_is_an_explicit_rgb_triple() {
        for (name, colour) in [
            ("void", VOID),
            ("raised", RAISED),
            ("line", LINE),
            ("red", RED),
            ("ember", EMBER),
            ("photon", PHOTON),
            ("bone", BONE),
            ("grey", GREY),
            ("faint", FAINT),
        ] {
            assert!(
                matches!(colour, Color::Rgb(..)),
                "{name} must be RGB, not a terminal slot"
            );
        }
    }

    #[test]
    fn the_page_is_the_void_and_it_is_cold() {
        let (r, g, b) = rgb(VOID);
        assert!(r < 16 && g < 16 && b < 20, "the page must be near-black");
        assert!(b >= r, "the void is cold: blue at least as strong as red");
    }

    #[test]
    fn the_primary_is_still_red() {
        let (r, g, b) = rgb(RED);
        assert!(r > 180 && g < 110 && b < 110, "red must stay red: ({r},{g},{b})");
        // The hot end of the same wheel: more green than the primary, still no blue to speak of.
        let (er, eg, eb) = rgb(EMBER);
        assert!(er > 200 && eg > 110 && eb < 120, "ember is the hot end of red");
    }

    #[test]
    fn body_text_is_readable_on_the_void() {
        for (name, colour) in [("bone", BONE), ("grey", GREY)] {
            let contrast = (luma(colour) + 5.0) / (luma(VOID) + 5.0);
            assert!(contrast > 5.5, "{name} is too dim on the void: {contrast:.1}:1");
        }
        let contrast = (luma(FAINT) + 5.0) / (luma(VOID) + 5.0);
        assert!(contrast > 3.0, "faint must still be legible: {contrast:.1}:1");
    }

    #[test]
    fn the_light_that_escaped_is_cool() {
        let (r, _g, b) = rgb(PHOTON);
        assert!(b > r + 40, "the photon ring must read cold against a red disk");
    }

    #[test]
    fn the_heat_ramp_runs_from_deep_red_to_white_hot() {
        let cold = rgb(heat(0.0));
        let mid = rgb(heat(0.6));
        let hot = rgb(heat(1.0));
        assert!(cold.0 < mid.0 || cold.2 < mid.2, "cold end is darker than the middle");
        assert!(luma(heat(1.0)) > luma(heat(0.6)), "the middle is dimmer than the hot end");
        assert!(luma(heat(0.6)) > luma(heat(0.0)), "the ramp is monotonic in brightness");
        // Out-of-range heat is clamped, not extrapolated into nonsense.
        assert_eq!(heat(-3.0), heat(0.0));
        assert_eq!(heat(9.0), heat(1.0));
        let _ = hot;
    }

    #[test]
    fn a_badge_is_void_on_red() {
        assert_eq!(badge().fg, Some(VOID));
        assert_eq!(badge().bg, Some(RED));
    }
}
