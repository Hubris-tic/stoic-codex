//! The black hole, live, in a real terminal.
//!
//!     cargo run --release --example hole
//!
//! Keys: `q` or Esc quits, `space` pauses the flow, `j` toggles the jets, `r` re-traces the
//! image (useful after a resize on a terminal that does not report it well).
//!
//! This is the loop an app shell should run: poll for input, advance the clock by the time
//! that actually elapsed, draw. Note what is *not* in the loop — the image is traced once per
//! terminal size and cached inside `BlackHoleAnim`, so a frame costs a few thousand sines
//! rather than a few thousand geodesics.

use std::io::{self, stdout};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use zenithal_tui_views::views::blackhole::{self, BlackHoleAnim};

const TICK: Duration = Duration::from_millis(66);

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
    let result = run(&mut terminal);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let mut anim = BlackHoleAnim::new();
    let mut paused = false;
    let mut last = Instant::now();
    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            blackhole::render_screen(frame, area, &mut anim);
        })?;

        // Wait for a key or a frame's worth of time, whichever comes first. Wall-clock time,
        // not a fixed step: a slow terminal should animate slowly, not skip.
        let deadline = last + TICK;
        let mut redraw = false;
        while Instant::now() < deadline {
            if event::poll(Duration::from_millis(4))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Char(' ') => paused = !paused,
                        KeyCode::Char('j') => anim.jets = !anim.jets,
                        KeyCode::Char('r') => {
                            // Force a re-trace at the current size.
                            let (w, h) = terminal.size().map(|s| (s.width, s.height))?;
                            let _ = anim.raster(w, h);
                            redraw = true;
                        }
                        _ => {}
                    }
                }
            }
        }
        let now = Instant::now();
        if !paused {
            anim.tick(now.duration_since(last).as_secs_f64());
        }
        last = now;
        let _ = redraw;
    }
}
