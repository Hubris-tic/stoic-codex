//! Render the real views into a JSON cell grid, so they can be turned into a picture.
//!
//! A terminal app in a sandbox has no screenshot button, and a hand-drawn mockup drifts
//! from the code within a day. So: the views draw into ratatui's `TestBackend`, the buffer
//! is dumped cell by cell (symbol, foreground, background, weight), and
//! `tui/render_preview.py` rasterises it. What you see in `preview/*.png` is the output of
//! the shipped rendering code, not an artist's impression of it.
//!
//!     cargo run --example snapshot            # writes tui/preview/*.json
//!     python3 render_preview.py               # writes tui/preview/*.png

use std::fs;
use std::path::Path;

use stoic_codex_tui_views::views::{
    blackhole::BlackHoleAnim, diff_review::DiffReviewState, memory_graph::MemoryGraphState,
    mission_board::MissionBoardState,
};
use stoic_codex_ui_core::{
    diff,
    graph::{EdgeKind, Graph, Node, NodeKind},
    mission::{Activity, ActivityKind, Agent, AgentState, Mission, Verdict},
};
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::{Frame, Terminal};

const W: u16 = 132;
const H: u16 = 34;
/// A laptop terminal with the sidebar folded away: the board must still be readable.
const W_NARROW: u16 = 84;
const H_NARROW: u16 = 22;

fn colour(c: Color) -> Option<String> {
    match c {
        Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
        Color::Reset => None,
        other => Some(format!("{other:?}")),
    }
}

/// Draw one screen and dump it.
fn snapshot(name: &str, draw: impl FnOnce(&mut Frame)) {
    snapshot_at(name, W, H, draw)
}

fn snapshot_at(name: &str, w: u16, h: u16, draw: impl FnOnce(&mut Frame)) {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("test backend");
    terminal.draw(draw).expect("draw");
    let buffer = terminal.backend().buffer();

    // One JSON array per cell: [symbol, foreground, background, bold]. A packed string
    // would break the moment a symbol is a wide character or empty.
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(h as usize);
    for y in 0..h {
        let mut row = Vec::with_capacity(w as usize);
        for x in 0..w {
            let cell = &buffer[(x, y)];
            // Defaults are the palette's own: unset text is bone, unset background is the
            // void. A preview that falls back to a different black is a preview of a
            // different product.
            let fg = colour(cell.fg).unwrap_or_else(|| "#e8e4e0".into());
            let bg = colour(cell.bg).unwrap_or_else(|| "#05060a".into());
            let bold = cell.modifier.contains(ratatui::style::Modifier::BOLD);
            row.push(format!(
                "[{},{},{},{}]",
                serde_json_string(cell.symbol()),
                serde_json_string(&fg),
                serde_json_string(&bg),
                if bold { 1 } else { 0 }
            ));
        }
        rows.push(row);
    }

    let plain: Vec<String> = (0..h)
        .map(|y| (0..w).map(|x| buffer[(x, y)].symbol()).collect::<String>())
        .collect();

    let json = format!(
        "{{\n  \"name\": {},\n  \"width\": {},\n  \"height\": {},\n  \"cells\": [\n{}\n  ]\n}}\n",
        serde_json_string(name),
        w,
        h,
        rows.iter()
            .map(|row| format!("    [{}]", row.join(",")))
            .collect::<Vec<_>>()
            .join(",\n")
    );

    let dir = Path::new("preview");
    fs::create_dir_all(dir).expect("preview dir");
    fs::write(dir.join(format!("{name}.json")), json).expect("write json");
    fs::write(dir.join(format!("{name}.txt")), plain.join("\n")).expect("write txt");
    println!("wrote preview/{name}.json  ({w}x{h})");
}

fn serde_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A mission mid-flight: the boss has reviewed, one operator landed, one is still working,
/// one collided with another agent's file.
fn mission() -> Mission {
    let mut m = Mission::new("refactor-auth", "stoic-codex");
    m.base = "main".into();
    m.budget_micros = 3_000_000;

    let mut boss = Agent::strategos("replace sha256 with argon2 and fix the timing leak");
    boss.state = AgentState::Done;
    boss.model = "anthropic:claude-sonnet".into();
    boss.input_tokens = 12_400;
    boss.output_tokens = 1_800;
    boss.micros = 110_000;
    boss.progress = 100;

    // Three operators enter as bare slots, because nothing knows yet what the work is.
    // Strategos names them, and only then does a branch exist — the branch is derived from
    // the code. That is the orchestrator's order, so it is the fixture's order too.
    let mut one = Agent::slot(1, "hash and verify with argon2 in src/auth.rs");
    one.state = AgentState::Done;
    one.verdict = Verdict::Accepted;
    one.progress = 100;
    one.input_tokens = 21_400;
    one.output_tokens = 3_200;
    one.micros = 420_000;
    one.elapsed_secs = 134;
    one.reason = Some("the diff is exactly the assigned change".into());

    let mut two = Agent::slot(2, "rotate the signing key without a sleep in src/keys.rs");
    two.state = AgentState::Running;
    two.progress = 61;
    two.input_tokens = 18_900;
    two.output_tokens = 2_600;
    two.micros = 380_000;
    two.elapsed_secs = 61;

    let mut three = Agent::slot(3, "rewrite the error mapping in src/error.rs");
    three.state = AgentState::Failed;
    three.verdict = Verdict::Rejected;
    three.progress = 34;
    three.input_tokens = 9_100;
    three.output_tokens = 1_400;
    three.micros = 290_000;
    three.elapsed_secs = 48;
    three.reason = Some("src/error.rs is owned by x1-write; work kept on its branch".into());

    m.push(boss);
    m.push(one);
    m.push(two);
    m.push(three);

    let renames: Vec<(String, String)> = vec![
        ("slot-1".into(), "x1-write".into()),
        ("slot-2".into(), "x7-trace".into()),
        ("slot-3".into(), "x5-patch".into()),
    ];
    assert_eq!(m.name_phalanx(&renames), 3, "the fixture must rename the board");
    for code in ["x1-write", "x7-trace", "x5-patch"] {
        if let Some(a) = m.agents.iter_mut().find(|a| a.id == code) {
            a.branch = Some(format!("stoic-codex/{code}"));
            a.worktree = Some(format!(".git/stoic-codex-missions/{code}"));
        }
    }

    // The feed, exactly as the app shell would build it from `MissionEvent`s — the naming
    // lines come from `Mission::naming_log`, so they cannot drift from the table's labels.
    let feed: &[(u64, ActivityKind, Option<&str>, &str)] = &[
        (0, ActivityKind::Plan, None, "planning the mission"),
        (2, ActivityKind::Plan, None, "3 assignments, one per code"),
        (3, ActivityKind::Deploy, Some("x1-write"), "deployed on stoic-codex/x1-write"),
        (3, ActivityKind::Deploy, Some("x7-trace"), "deployed on stoic-codex/x7-trace"),
        (4, ActivityKind::Deploy, Some("x5-patch"), "deployed on stoic-codex/x5-patch"),
        (8, ActivityKind::Tool, Some("x1-write"), "write_file src/auth.rs"),
        (11, ActivityKind::Result, Some("x1-write"), "ok write_file (2ms) 41 lines"),
        (14, ActivityKind::Tool, Some("x7-trace"), "search \"sleep(Duration\" src/"),
        (19, ActivityKind::Result, Some("x7-trace"), "ok search (6ms) 2 matches in src/keys.rs"),
        (23, ActivityKind::Tool, Some("x5-patch"), "write_file src/error.rs"),
        (26, ActivityKind::Conflict, Some("x5-patch"), "src/error.rs also written by x1-write; x1-write wins"),
        (31, ActivityKind::Tool, Some("x7-trace"), "edit_file src/keys.rs"),
        (38, ActivityKind::Verdict, None, "Strategos reviewing 3 reports"),
        (44, ActivityKind::Verdict, Some("x1-write"), "accepted — the diff is exactly the assigned change"),
        (45, ActivityKind::Verdict, Some("x5-patch"), "rejected — src/error.rs is owned by x1-write"),
        (61, ActivityKind::Tool, Some("x7-trace"), "run_command cargo test auth::"),
        (64, ActivityKind::Done, None, "1 file applied, 1 conflict reported"),
    ];
    // Plan first, then the naming, then the deploys — the order a mission happens in.
    for (secs, kind, codename, text) in &feed[..2] {
        m.log(Activity::new(*secs, *kind, codename.map(|c| c.to_string()), *text));
    }
    for line in Mission::naming_log(2, &renames) {
        m.log(line);
    }
    for (secs, kind, codename, text) in &feed[2..] {
        m.log(Activity::new(*secs, *kind, codename.map(|c| c.to_string()), *text));
    }
    m
}


fn diff_state() -> DiffReviewState {
    let patch = "\
diff --git a/src/auth.rs b/src/auth.rs
--- a/src/auth.rs
+++ b/src/auth.rs
@@ -41,5 +41,6 @@ pub fn verify(token: &str) -> bool {
 pub fn verify(token: &str) -> bool {
-    let hash = sha256(token);
-    hash == stored
+    let hash = argon2::hash(token)?;
+    argon2::verify(&hash, stored)
+        .map_err(|e| Error::Crypto(e))?
 }
 
 fn rotate_key() {
-    sleep(Duration::from_secs(1));
+    let Some(next) = next_key() else {
+        return Err(Error::Exhausted);
+    };
+    schedule_rotation(next);
";
    let files = diff::parse(patch).expect("the fixture patch parses");
    DiffReviewState::new(files)
}

fn memory() -> MemoryGraphState {
    let mut g = Graph::default();
    let nodes = [
        ("n1", NodeKind::Decision, "use argon2 over sha256", 0.94),
        ("n2", NodeKind::File, "src/auth.rs", 0.91),
        ("n3", NodeKind::Symbol, "fn verify", 0.66),
        ("n4", NodeKind::Error, "timing leak in verify", 0.61),
        ("n5", NodeKind::Symbol, "fn rotate_key", 0.44),
        ("n6", NodeKind::Commit, "4f2c1ab add argon2", 0.38),
        ("n7", NodeKind::Task, "refactor-auth", 0.33),
    ];
    for (id, kind, label, score) in nodes {
        g.add_node(Node { id: id.into(), kind, label: label.into(), score });
    }
    g.add_edge("n1", "n2", EdgeKind::DerivedFrom);
    g.add_edge("n2", "n3", EdgeKind::Imports);
    g.add_edge("n3", "n4", EdgeKind::Regresses);
    g.add_edge("n3", "n5", EdgeKind::Calls);
    g.add_edge("n2", "n6", EdgeKind::AuthoredBy);
    g.add_edge("n7", "n2", EdgeKind::DerivedFrom);
    MemoryGraphState::new(g, "n2")
}

fn main() {
    snapshot("mission", |f| {
        let state = MissionBoardState::new(mission());
        let area = f.area();
        stoic_codex_tui_views::views::mission_board::render_screen(f, area, &state);
    });

    // The same board on a small laptop terminal: branch column dropped, report pane folded
    // away, everything else still legible. Adapting is a design decision, so it gets a
    // snapshot of its own rather than a footnote.
    snapshot_at("mission-narrow", W_NARROW, H_NARROW, |f| {
        let state = MissionBoardState::new(mission());
        let area = f.area();
        stoic_codex_tui_views::views::mission_board::render_screen(f, area, &state);
    });

    snapshot("diff", |f| {
        let state = diff_state();
        let area = f.area();
        stoic_codex_tui_views::views::diff_review::render_screen(f, area, &state);
    });

    snapshot("memory", |f| {
        let mut state = memory();
        let area = f.area();
        stoic_codex_tui_views::views::memory_graph::render_screen(f, area, &mut state);
    });

    // The hole tab: real integrated geodesics, parked at a fixed phase. A snapshot has to be
    // the same picture twice, so the clock is set rather than started.
    let mut anim = BlackHoleAnim::new();
    anim.phase = 1.1;
    snapshot("hole", |f| {
        let area = f.area();
        stoic_codex_tui_views::views::blackhole::render_screen(f, area, &mut anim);
    });

    // Same hole, laptop-sized: the facts foot is dropped, the image is not.
    let mut narrow_anim = BlackHoleAnim::new();
    narrow_anim.phase = 2.4;
    snapshot_at("hole-narrow", W_NARROW, H_NARROW, |f| {
        let area = f.area();
        stoic_codex_tui_views::views::blackhole::render_screen(f, area, &mut narrow_anim);
    });
}
