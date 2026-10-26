//! `stoic-codex tools` — what an agent can do on this machine, printed as the model sees it.
//!
//! Showing the schemas rather than prose is the point: if a model is failing to call a
//! tool correctly, this is the spec it was given.

use std::io::Write;
use std::sync::Arc;

use stoic_codex_agent::tools::shell::ShellPolicy;
use stoic_codex_agent::{Sandbox, ToolRegistry};

use crate::args::Parsed;
use crate::cmd::{CmdError, CmdResult};
use crate::Ctx;

pub fn run(parsed: &Parsed, ctx: &mut Ctx) -> CmdResult {
    match parsed.rest().first().map(String::as_str) {
        Some("list") | None => list(parsed, ctx),
        Some(other) => Err(CmdError::usage(format!(
            "unknown `tools` subcommand `{other}` (list)"
        ))),
    }
}

/// The registry an agent would get with every option on, rooted at `path`.
fn full_registry(path: &std::path::Path, shell: bool) -> Result<ToolRegistry, CmdError> {
    let sandbox = Arc::new(Sandbox::new(path).map_err(|e| CmdError::failed(e.to_string()))?);
    let registry = ToolRegistry::editing(sandbox.clone());
    Ok(if shell {
        registry.with_shell(sandbox, ShellPolicy::build_tools())
    } else {
        registry
    })
}

pub fn list(parsed: &Parsed, ctx: &mut Ctx) -> CmdResult {
    // The schemas are identical for every root; use the current directory only to satisfy
    // the sandbox constructor.
    let dir = std::env::current_dir().map_err(|e| CmdError::failed(e.to_string()))?;
    let registry = full_registry(&dir, true)?;
    let json = parsed.switch("json");

    if json {
        let specs = registry.specs();
        let _ = writeln!(
            ctx.out,
            "{}",
            serde_json::to_string_pretty(&specs).unwrap_or_default()
        );
        return Ok(());
    }

    let _ = writeln!(ctx.out, "{:<12} {} ", "TOOL", "DESCRIPTION");
    for spec in registry.specs() {
        let description = spec.description.clone().unwrap_or_default();
        let first_line = description.split(". ").next().unwrap_or(&description);
        let _ = writeln!(ctx.out, "{:<12} {}", spec.name, first_line);
    }
    let _ = writeln!(
        ctx.out,
        "\n{} tool(s). Every path is sandboxed to the working directory; the shell tool is \
         only present when a command asks for it with --allow-shell.",
        registry.len()
    );
    Ok(())
}
