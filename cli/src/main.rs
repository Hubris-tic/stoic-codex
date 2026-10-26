//! `stoic-codex` — the entry point. Everything below `main` is testable without a terminal:
//! see `src/lib.rs` (`run`) and `tests/cli.rs`.

use std::io::Write;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let mut ctx = stoic_codex_cli::Ctx {
        home,
        out: stoic_codex_cli::Sink::new(std::io::stdout()),
        err: stoic_codex_cli::Sink::new(std::io::stderr()),
    };
    let code = stoic_codex_cli::run(&argv, &mut ctx);
    let _ = ctx.out.flush();
    let _ = ctx.err.flush();
    std::process::exit(code);
}
