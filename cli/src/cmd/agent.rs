//! `stoic-codex agent run` — one agent, with tools, in this repository.
//!
//! The single-agent path exists for two reasons: it is the smallest thing that proves the
//! tool loop works against a real provider, and it is what most tasks actually need. The
//! squad is for work that is worth racing.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use stoic_codex_agent::tools::shell::ShellPolicy;
use stoic_codex_agent::{run_agent, AgentConfig, Event, Sandbox, ToolRegistry};

use crate::args::Parsed;
use crate::cmd::{CmdError, CmdResult};
use crate::config::Config;
use crate::model::RemoteModel;
use crate::paths::Paths;
use crate::Ctx;

pub fn run(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    match parsed.rest().first().map(String::as_str) {
        Some("run") => one(parsed, paths, ctx),
        Some("tools") => crate::cmd::tools::list(parsed, ctx),
        Some(other) => Err(CmdError::usage(format!(
            "unknown `agent` subcommand `{other}` (run, tools)"
        ))),
        None => Err(CmdError::usage(
            "usage: stoic-codex agent <run|tools> [options]",
        )),
    }
}

fn one(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let rest = parsed.rest();
    let task = rest[1..].join(" ").trim().to_string();
    if task.is_empty() {
        return Err(CmdError::usage(
            "usage: stoic-codex agent run <task…> [--provider ID] [--model M] [--repo DIR] \
             [--allow-shell a,b] [--read-only] [--max-steps N] [--max-attempts N]",
        ));
    }

    let mut cfg = Config::load_for(paths).map_err(CmdError::failed)?;
    cfg.validate().map_err(CmdError::failed)?;
    crate::cmd::run::apply_retry_override(parsed, &mut cfg)?;

    let provider = parsed
        .value("provider")
        .map(str::to_string)
        .or_else(|| std::env::var("STOIC_CODEX_PROVIDER").ok())
        .or_else(|| std::env::var("ZENITHAL_PROVIDER").ok())
        .or_else(|| cfg.providers.default.clone())
        .ok_or_else(|| {
            CmdError::usage(format!(
                "no provider selected: pass --provider <id> or set providers.default in {}",
                paths.config_file().display()
            ))
        })?;
    let model = parsed
        .value("model")
        .map(str::to_string)
        .or_else(|| cfg.providers.model.clone())
        .ok_or_else(|| CmdError::usage("no model selected: pass --model <name>"))?;

    let repo = PathBuf::from(parsed.value("repo").unwrap_or("."));
    let repo = if repo.exists() {
        repo.canonicalize().unwrap_or(repo)
    } else {
        return Err(CmdError::failed(format!(
            "--repo {} does not exist",
            repo.display()
        )));
    };

    // Tools: read-only by default? No — an agent that cannot edit is a chat client. The
    // default is read+write inside the repo, with no shell. `--allow-shell` is explicit.
    let sandbox = Arc::new(Sandbox::new(&repo).map_err(|e| CmdError::failed(e.to_string()))?);
    let mut tools = if parsed.switch("read-only") {
        ToolRegistry::readonly(sandbox.clone())
    } else {
        ToolRegistry::editing(sandbox.clone())
    };
    if let Some(list) = parsed.value("allow-shell") {
        let allow: Vec<String> = list
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if allow.is_empty() {
            return Err(CmdError::usage(
                "--allow-shell needs at least one program name",
            ));
        }
        tools = tools.with_shell(
            sandbox.clone(),
            ShellPolicy {
                allow,
                ..ShellPolicy::locked()
            },
        );
    }

    let model_client =
        RemoteModel::new(&provider, paths, &cfg, Some(&model)).map_err(CmdError::failed)?;

    let mut agent_cfg = AgentConfig::new(model.clone());
    agent_cfg.system = Some(system_prompt(&repo));
    agent_cfg.max_tokens = cfg.providers.max_tokens;
    agent_cfg.temperature = cfg.providers.temperature;
    if let Some(v) = parsed.value("max-steps") {
        agent_cfg.max_steps = v
            .parse()
            .map_err(|_| CmdError::usage("--max-steps must be a number"))?;
    }
    if agent_cfg.max_steps == 0 {
        return Err(CmdError::usage("--max-steps must be at least 1"));
    }
    let quiet = parsed.switch("quiet");
    let json = parsed.switch("json");

    let mut on_event = |event: Event| {
        if quiet || json {
            return;
        }
        match event {
            Event::ToolCall { name, args } => {
                let _ = writeln!(ctx.err, "→ {name} {args}");
            }
            Event::ToolResult {
                name,
                ok,
                summary,
                ms,
            } => {
                let _ = writeln!(
                    ctx.err,
                    "{} {name} ({}ms) {}",
                    if ok { "  ok" } else { "  !! " },
                    ms,
                    summary
                );
            }
            Event::Finished {
                reason,
                text_chars,
                tool_calls,
            } => {
                let _ = writeln!(
                    ctx.err,
                    "— {}, {tool_calls} tool calls, {text_chars} chars",
                    reason.label()
                );
            }
            _ => {}
        }
    };

    let outcome = run_agent(&model_client, &tools, &task, &agent_cfg, &mut on_event)
        .map_err(|e| CmdError::failed(e.to_string()))?;

    if json {
        let payload = serde_json::json!({
            "provider": provider,
            "model": model,
            "text": outcome.text,
            "stop": outcome.stop.label(),
            "steps": outcome.steps,
            "tool_calls": outcome.tool_calls,
            "usage": {
                "input_tokens": outcome.usage.input_tokens,
                "output_tokens": outcome.usage.output_tokens,
            },
            "repo": repo.to_string_lossy(),
        });
        let _ = writeln!(
            ctx.out,
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else if !outcome.text.trim().is_empty() {
        let _ = writeln!(ctx.out, "{}", outcome.text.trim_end());
    }

    if !outcome.stop.is_complete() {
        // Not a crash: the run stopped at a limit and the work so far is on disk. Say so
        // and use the failure code, because scripts should notice.
        return Err(CmdError::failed(format!(
            "the agent stopped early: {}",
            outcome.stop.label()
        )));
    }
    Ok(())
}

/// Where the agent is, and what it must not assume. Short on purpose: every token here is
/// paid for on every turn, and the tools carry most of the guidance.
pub fn system_prompt(repo: &std::path::Path) -> String {
    format!(
        "You are {} working in a git repository at {}.\n\n\
         Work by reading, not guessing: use search and read_file before you change anything, \
         and list_dir when you are unsure of the layout. Make the smallest change that \
         finishes the task, then check it with git_diff.\n\n\
         Every path you pass to a tool is relative to the repository root. You cannot reach \
         outside it, and `.git` is off limits. When you are done, answer in one short \
         paragraph describing what you changed — no code blocks, no restating the task.",
        stoic_codex_brand::NAME,
        repo.display()
    )
}
