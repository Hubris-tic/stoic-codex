//! `stoic-codex` — the entry point. Everything below `main` is testable without a terminal:
//! see `src/lib.rs` (`run`) and `tests/cli.rs`.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use stoic_codex_cli::{args, Ctx, Sink};
use stoic_codex_tui_views::app::{ChatStream, ChatTui};

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let parsed = args::parse(&argv).ok();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let interactive = io::stdout().is_terminal() && io::stderr().is_terminal();

    if interactive && should_start_chat(parsed.as_ref()) {
        if let Some(parsed) = parsed.as_ref() {
            let prompt = parsed.rest().join(" ").trim().to_string();
            let title = run_title(parsed);
            match ChatTui::enter(title) {
                Ok(mut tui) => {
                    if let Err(error) = tui.begin_turn(&prompt) {
                        drop(tui);
                        eprintln!("could not draw the chat screen ({error}); using plain output");
                        std::process::exit(execute_plain(&argv, &home));
                    }
                    let mut code = execute_chat_turn(&argv, &home, &mut tui);
                    let mut input_error = None;
                    let mut session_available = !parsed.switch("no-session")
                        && (code == stoic_codex_cli::OK
                            || parsed.switch("continue")
                            || parsed.value("resume").is_some());
                    loop {
                        match tui.next_prompt() {
                            Ok(Some(prompt)) => {
                                let next = follow_up_args(&argv, &prompt, session_available);
                                if let Err(error) = tui.begin_turn(&prompt) {
                                    input_error =
                                        Some(format!("could not refresh the chat screen: {error}"));
                                    code = stoic_codex_cli::FAILED;
                                    break;
                                }
                                code = execute_chat_turn(&next, &home, &mut tui);
                                session_available = !parsed.switch("no-session")
                                    && (code == stoic_codex_cli::OK || session_available);
                            }
                            Ok(None) => break,
                            Err(error) => {
                                input_error = Some(format!("chat input failed: {error}"));
                                code = stoic_codex_cli::FAILED;
                                break;
                            }
                        }
                    }
                    drop(tui);
                    if let Some(error) = input_error {
                        eprintln!("{error}");
                    }
                    std::process::exit(code);
                }
                Err(error) => {
                    eprintln!("interactive chat unavailable ({error}); using plain output");
                }
            }
        }
    }

    let mut effective_argv = argv.clone();
    if interactive && should_start_mission_tui(parsed.as_ref()) && !has_flag(&effective_argv, "tui")
    {
        effective_argv.push("--tui".into());
    }
    let code = execute_plain(&effective_argv, &home);
    std::process::exit(code);
}

fn execute_plain(argv: &[String], home: &PathBuf) -> i32 {
    let mut ctx = Ctx {
        home: home.clone(),
        out: Sink::new(io::stdout()),
        err: Sink::new(io::stderr()),
    };
    let code = stoic_codex_cli::run(argv, &mut ctx);
    let _ = ctx.out.flush();
    let _ = ctx.err.flush();
    code
}

fn execute_chat_turn(argv: &[String], home: &PathBuf, tui: &mut ChatTui) -> i32 {
    let mut ctx = Ctx {
        home: home.clone(),
        out: Sink::new(tui.writer(ChatStream::Assistant)),
        err: Sink::new(tui.writer(ChatStream::Status)),
    };
    let code = stoic_codex_cli::run(argv, &mut ctx);
    let _ = ctx.out.flush();
    let _ = ctx.err.flush();
    let _ = tui.finish_turn(code);
    code
}

fn should_start_chat(parsed: Option<&args::Parsed>) -> bool {
    let Some(parsed) = parsed else { return false };
    parsed.command() == Some("run")
        && !parsed.rest().is_empty()
        && !parsed.switch("json")
        && !parsed.switch("dry-run")
        && !parsed.switch("no-tui")
}

fn should_start_mission_tui(parsed: Option<&args::Parsed>) -> bool {
    let Some(parsed) = parsed else { return false };
    parsed.command() == Some("mission")
        && parsed.rest().first().map(String::as_str) == Some("run")
        && parsed.rest().len() > 1
        && !parsed.switch("json")
        && !parsed.switch("quiet")
        && !parsed.switch("no-tui")
}

fn run_title(parsed: &args::Parsed) -> String {
    let provider = parsed
        .value("provider")
        .map(str::to_string)
        .or_else(|| std::env::var("STOIC_CODEX_PROVIDER").ok())
        .unwrap_or_else(|| "configured provider".into());
    let model = parsed
        .value("model")
        .map(str::to_string)
        .unwrap_or_else(|| "configured model".into());
    format!("{provider} / {model}")
}

fn follow_up_args(original: &[String], prompt: &str, continue_session: bool) -> Vec<String> {
    let parsed = args::parse(original).unwrap_or_default();
    let mut next = vec!["run".to_string(), prompt.to_string()];
    for name in [
        "provider",
        "model",
        "max-tokens",
        "max-attempts",
        "temperature",
    ] {
        if let Some(value) = parsed.value(name) {
            next.push(format!("--{name}"));
            next.push(value.to_string());
        }
    }
    for name in ["no-session", "no-stream", "no-memory", "no-remember"] {
        if parsed.switch(name) {
            next.push(format!("--{name}"));
        }
    }
    if continue_session && !parsed.switch("no-session") {
        next.push("--continue".into());
    }
    next
}

fn has_flag(argv: &[String], name: &str) -> bool {
    argv.iter()
        .any(|arg| arg == &format!("--{name}") || arg.starts_with(&format!("--{name}=")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(words: &[&str]) -> args::Parsed {
        args::parse(
            &words
                .iter()
                .map(|word| word.to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn json_dry_run_and_explicit_no_tui_preserve_the_plain_run_path() {
        assert!(!should_start_chat(Some(&parsed(&[
            "run", "hello", "--json"
        ]))));
        assert!(!should_start_chat(Some(&parsed(&[
            "run",
            "hello",
            "--dry-run"
        ]))));
        assert!(!should_start_chat(Some(&parsed(&[
            "run", "hello", "--no-tui"
        ]))));
        assert!(!should_start_chat(Some(&parsed(&["run", "--help"]))));
    }

    #[test]
    fn mission_tui_is_only_selected_for_interactive_text_missions() {
        assert!(should_start_mission_tui(Some(&parsed(&[
            "mission", "run", "task"
        ]))));
        assert!(!should_start_mission_tui(Some(&parsed(&[
            "mission", "plan", "task"
        ]))));
        assert!(!should_start_mission_tui(Some(&parsed(&[
            "mission", "run", "task", "--json"
        ]))));
        assert!(!should_start_mission_tui(Some(&parsed(&[
            "mission", "run", "task", "--quiet"
        ]))));
        assert!(!should_start_mission_tui(Some(&parsed(&[
            "mission", "run", "task", "--no-tui"
        ]))));
    }

    #[test]
    fn follow_up_turn_preserves_model_options_and_continues_the_local_session() {
        let base = [
            "run".to_string(),
            "first task".to_string(),
            "--provider".to_string(),
            "gemini".to_string(),
            "--model=flash".to_string(),
            "--max-attempts".to_string(),
            "3".to_string(),
            "--resume".to_string(),
            "old-session".to_string(),
        ];
        let next = follow_up_args(&base, "second task", true);
        assert_eq!(
            next.iter().map(String::as_str).collect::<Vec<_>>(),
            [
                "run",
                "second task",
                "--provider",
                "gemini",
                "--model",
                "flash",
                "--max-attempts",
                "3",
                "--continue",
            ]
        );
    }

    #[test]
    fn no_session_follow_up_does_not_enable_continue() {
        let base = [
            "run".to_string(),
            "first".to_string(),
            "--no-session".to_string(),
        ];
        let next = follow_up_args(&base, "second", true);
        assert_eq!(
            next.iter().map(String::as_str).collect::<Vec<_>>(),
            ["run", "second", "--no-session"]
        );
    }
}
