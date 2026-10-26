//! `stoic-codex` — wiring, not plumbing.
//!
//! This crate is deliberately thin: it parses arguments, resolves local paths, opens the
//! memory backend, and prints. The logic lives in `stoic-codex-providers` (wire adapters) and
//! `stoic-codex-memory` (graph + vectors), which are both independently testable.
//!
//! Design choice: **no CLI framework**. Argument parsing here is ~100 lines with tests,
//! which keeps the dependency tree flat and the binary fast to start — the same reason
//! the provider layer hand-rolls SSE instead of pulling a streaming library.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub mod args;
pub mod cmd;
pub mod config;
pub mod model;
pub mod net;
pub mod paths;
pub mod retry;
pub mod session;
pub mod transcript;

/// A write sink that can be cloned and shared across threads.
///
/// This exists because of one requirement: a squad has three agents running at once and the
/// user wants to watch them. A `Box<dyn Write>` cannot be handed to worker threads (it is
/// neither `Sync` nor cloneable), so output goes through an `Arc<Mutex<..>>` instead. The
/// cost is one uncontended lock per line, which is nothing next to an HTTP round trip, and
/// the benefit is that progress is live rather than buffered until the end.
#[derive(Clone)]
pub struct Sink(Arc<Mutex<Box<dyn Write + Send>>>);

impl Sink {
    pub fn new(writer: impl Write + Send + 'static) -> Sink {
        Sink(Arc::new(Mutex::new(Box::new(writer))))
    }

    /// A sink that discards everything — for `--json` runs and tests that only want stdout.
    pub fn null() -> Sink {
        Sink::new(std::io::sink())
    }
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.0.lock() {
            Ok(mut writer) => writer.write(buf),
            Err(_) => Ok(buf.len()),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self.0.lock() {
            Ok(mut writer) => writer.flush(),
            Err(_) => Ok(()),
        }
    }
}

/// Everything a command needs, injected so tests never touch the real `$HOME` or the
/// real stdout.
pub struct Ctx {
    pub home: PathBuf,
    pub out: Sink,
    pub err: Sink,
}

impl Ctx {
    /// Test/embedding helper: a context writing into buffers.
    pub fn buffered(home: impl Into<PathBuf>) -> (Ctx, Buffers) {
        let out = Shared::default();
        let err = Shared::default();
        (
            Ctx {
                home: home.into(),
                out: Sink::new(out.clone()),
                err: Sink::new(err.clone()),
            },
            Buffers { out, err },
        )
    }
}

/// A `Write` that appends into a shared buffer, so a test can read the output afterwards.
#[derive(Clone, Default)]
pub struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct Buffers {
    out: Shared,
    err: Shared,
}

impl Buffers {
    pub fn out(&self) -> String {
        String::from_utf8_lossy(&self.out.0.lock().unwrap()).to_string()
    }
    pub fn err(&self) -> String {
        String::from_utf8_lossy(&self.err.0.lock().unwrap()).to_string()
    }
    /// Forget everything captured so far, so each invocation can be checked on its own.
    pub fn clear(&self) {
        self.out.0.lock().unwrap().clear();
        self.err.0.lock().unwrap().clear();
    }
}

/// Exit codes. Chosen so scripts can distinguish "you asked something impossible" from
/// "the tool is unhappy", which a plain 1 cannot.
pub const OK: i32 = 0;
pub const FAILED: i32 = 1;
pub const USAGE: i32 = 2;

/// Run one command line. Returns the process exit code.
pub fn run(argv: &[String], ctx: &mut Ctx) -> i32 {
    let parsed = match args::parse(argv) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(ctx.err, "error: {e}");
            let _ = writeln!(ctx.err, "run `stoic-codex --help` for usage");
            return USAGE;
        }
    };

    if parsed.flag("version").is_some() {
        // Name once, then what it is: the attribution must be present (Apache-2.0) without
        // printing the product name twice.
        let _ = writeln!(
            ctx.out,
            "{}",
            stoic_codex_brand::version_header(env!("CARGO_PKG_VERSION"))
        );
        let _ = writeln!(ctx.out, "{}", stoic_codex_brand::TAGLINE);
        let _ = writeln!(
            ctx.out,
            "{}, {} — local only, no account, no telemetry",
            stoic_codex_brand::ORIGIN,
            stoic_codex_brand::LICENSE
        );
        return OK;
    }
    if parsed.flag("help").is_some() || parsed.positional.is_empty() {
        let _ = write!(ctx.out, "{}", help_text());
        return OK;
    }

    let paths = paths::Paths::from_env(ctx.home.clone());
    match paths.migrate_legacy_config() {
        Ok(paths::ConfigMigration::Migrated) => {
            let _ = writeln!(
                ctx.err,
                "migrated local config from {} to {}",
                paths
                    .home()
                    .join(stoic_codex_brand::LEGACY_CONFIG_DIR)
                    .display(),
                paths.config_dir().display()
            );
        }
        Ok(paths::ConfigMigration::BothExist) => {
            let _ = writeln!(
                ctx.err,
                "warning: both {} and {} exist; using the new Stoic Codex directory and leaving the legacy copy untouched",
                paths.config_dir().display(),
                paths.home().join(stoic_codex_brand::LEGACY_CONFIG_DIR).display()
            );
        }
        Ok(paths::ConfigMigration::Unchanged) => {}
        Err(error) => {
            let _ = writeln!(ctx.err, "error: cannot migrate local config: {error}");
            return FAILED;
        }
    }

    let result = match parsed.positional[0].as_str() {
        "run" => cmd::run::run(&parsed, &paths, ctx),
        "agent" => cmd::agent::run(&parsed, &paths, ctx),
        "mission" | "squad" => cmd::mission::run(&parsed, &paths, ctx),
        "tools" => cmd::tools::run(&parsed, ctx),
        "providers" => cmd::providers::run(&parsed, &paths, ctx),
        "memory" => cmd::memory::run(&parsed, &paths, ctx),
        "context" => cmd::memory::context(&parsed, &paths, ctx),
        "daemon" => cmd::daemon::run(&parsed, &paths, ctx),
        "version" => {
            let _ = writeln!(
                ctx.out,
                "{}",
                stoic_codex_brand::version_header(env!("CARGO_PKG_VERSION"))
            );
            let _ = writeln!(
                ctx.out,
                "{}, {}",
                stoic_codex_brand::ORIGIN,
                stoic_codex_brand::LICENSE
            );
            Ok(())
        }
        other => Err(cmd::CmdError::Usage(format!("unknown command `{other}`"))),
    };

    match result {
        Ok(()) => OK,
        Err(cmd::CmdError::Usage(msg)) => {
            let _ = writeln!(ctx.err, "error: {msg}");
            let _ = writeln!(ctx.err, "run `stoic-codex --help` for usage");
            USAGE
        }
        Err(cmd::CmdError::Failed(msg)) => {
            let _ = writeln!(ctx.err, "error: {msg}");
            FAILED
        }
    }
}

pub fn help_text() -> String {
    let mut h = String::new();
    h.push_str(&format!(
        "{} — {}\n",
        stoic_codex_brand::NAME,
        stoic_codex_brand::TAGLINE
    ));
    h.push_str("\nUSAGE\n  stoic-codex <command> [options]\n");

    h.push_str("\nRUN (memory → prompt → provider → answer → local transcript)\n");
    h.push_str("  stoic-codex run <task…>                    send a turn; transcript is local\n");
    h.push_str("                                        (--provider, --model, --max-tokens, --max-attempts N,\n");
    h.push_str(
        "                                         --temperature, --continue, --resume ID,\n",
    );
    h.push_str("                                         --no-session, --no-stream, --dry-run,\n");
    h.push_str(
        "                                         --no-memory, --no-remember, --json, --no-tui)\n",
    );
    h.push_str(
        "                                        interactive terminals open the full-height chat; --no-tui keeps one-shot output\n",
    );

    h.push_str("\nAGENT (one agent, with tools, in this repository)\n");
    h.push_str(
        "  stoic-codex agent run <task…>              read, edit and verify files with tools\n",
    );
    h.push_str(
        "                                        (--repo, --read-only, --allow-shell a,b,\n",
    );
    h.push_str("                                         --max-steps, --max-attempts N, --json, --quiet)\n");
    h.push_str("  stoic-codex tools                          the tools an agent is given\n");
    h.push_str(
        "  stoic-codex tools --json                   ...as the JSON Schema the model sees\n",
    );

    h.push_str("\nMISSION (Strategos plans; N Operator work in N isolated git worktrees)\n");
    h.push_str(
        "  stoic-codex mission run <task…>            plan, deploy --agents N worktrees, review,\n",
    );
    h.push_str("                                        apply what Strategos accepted\n");
    h.push_str(
        "                                        (--repo, --base REF, --verify \"cargo test\",\n",
    );
    h.push_str(
        "                                         --no-review, --no-commit, --parallel N,\n",
    );
    h.push_str(
        "                                         --keep-worktrees, --keep-branches, --max-attempts N, --json, --no-tui)\n",
    );
    h.push_str(
        "                                        interactive terminals show the live mission board; --no-tui keeps the text log\n",
    );
    h.push_str(
        "  stoic-codex mission plan <task…>           show the assignments and stop (--max-attempts N, --json)\n",
    );

    h.push_str("\nPROVIDERS (BYOK; built-in ids plus any spec you write)\n");
    h.push_str(
        "  stoic-codex providers list                 list built-in and user-defined providers\n",
    );
    h.push_str(
        "  stoic-codex providers add <file.json>      validate a provider spec and install it\n",
    );
    h.push_str(
        "  stoic-codex providers show <id>            show the exact request it would send\n",
    );
    h.push_str(
        "  stoic-codex providers models <id> [--free] [--max-attempts N] list the reported model catalogue\n",
    );
    h.push_str("  stoic-codex providers test <id> --model M  send one real completion request and report it\n");
    h.push_str("                                        (--max-tokens N, --max-attempts N)\n");
    h.push_str("  stoic-codex providers ids                  print every built-in provider id\n");

    h.push_str("\nMEMORY (local only: a local TypeDB daemon, or a file under ~/");
    h.push_str(stoic_codex_brand::CONFIG_DIR);
    h.push_str(")\n");
    h.push_str("  stoic-codex memory init                    create/open the store\n");
    h.push_str("  stoic-codex memory remember <id> <label>   store a memory (--kind, --text, --from, --importance)\n");
    h.push_str("  stoic-codex memory link <from> <to> <kind> connect two memories\n");
    h.push_str(
        "  stoic-codex memory recall <query>          rank memories (--focus, --limit, --json)\n",
    );
    h.push_str(
        "  stoic-codex memory why <query>             show the score breakdown for each hit\n",
    );
    h.push_str("  stoic-codex memory forget <id>             drop one memory and its edges\n");
    h.push_str("  stoic-codex memory stats                   backend, counts, paths\n");

    h.push_str("\nCONTEXT\n");
    h.push_str(
        "  stoic-codex context <query>                print the memory block injected into a prompt\n",
    );
    h.push_str("                                        (--budget chars, --max-tokens N, --system-only, --json)\n");

    h.push_str("\nDAEMON\n");
    h.push_str(
        "  stoic-codex daemon status                  is a local TypeDB running, and where\n",
    );

    h.push_str("\nGLOBAL\n");
    h.push_str("  --json            machine-readable output where supported\n");
    h.push_str("  -h, --help        this text\n");
    h.push_str("  -V, --version     version and attribution\n");
    h.push_str(
        "\nEverything is local. There is no hosted backend, no account, and no telemetry.\n",
    );
    h
}
