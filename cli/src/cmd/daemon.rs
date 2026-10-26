//! `zenithal daemon status` — what the local database situation actually is.
//!
//! Read-only on purpose: starting a daemon is done implicitly by the commands that need
//! one, so there is no `daemon start` that could leave an orphan behind.

use std::io::Write;

use zenithal_memory::daemon::{self, LaunchPolicy};

use crate::args::Parsed;
use crate::cmd::{CmdError, CmdResult};
use crate::config::Config;
use crate::paths::Paths;
use crate::Ctx;

pub fn run(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    match parsed.rest().first().map(String::as_str) {
        Some("status") | None => status(paths, ctx),
        Some(other) => Err(CmdError::usage(format!(
            "unknown `daemon` subcommand `{other}` (status)"
        ))),
    }
}

fn status(paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let cfg = Config::load_for(paths).map_err(CmdError::failed)?;
    cfg.validate().map_err(CmdError::failed)?;
    let address = &cfg.memory.daemon_address;
    let running = daemon::port_open(address);
    let binary = daemon::find_binary();
    let has_binary = binary.is_some();
    let policy = if cfg.memory.allow_daemon {
        LaunchPolicy::Auto
    } else {
        LaunchPolicy::Never
    };

    let _ = writeln!(ctx.out, "config      {}", paths.config_file().display());
    let _ = writeln!(ctx.out, "backend     {}", cfg.memory.backend);
    let _ = writeln!(
        ctx.out,
        "address     {address} ({})",
        if daemon::is_loopback(address) {
            "loopback"
        } else {
            "NOT LOOPBACK"
        }
    );
    let _ = writeln!(
        ctx.out,
        "running     {}",
        if running { "yes" } else { "no" }
    );
    let _ = writeln!(
        ctx.out,
        "binary      {}",
        binary
            .map(|b| b.display().to_string())
            .unwrap_or_else(|| "not found (set ZENITHAL_TYPEDB_BIN)".into())
    );
    let _ = writeln!(ctx.out, "policy      {policy:?}");
    let _ = writeln!(ctx.out, "data        {}", paths.typedb_data_dir().display());
    let _ = writeln!(ctx.out, "file store  {}", paths.memory_file().display());
    let effective = if running {
        "typedb (attached)"
    } else if has_binary && cfg.memory.allow_daemon && cfg.memory.backend != "file" {
        "typedb (would be started)"
    } else {
        "file"
    };
    let _ = writeln!(ctx.out, "effective   {effective}");
    Ok(())
}
