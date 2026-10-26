//! `stoic-codex mission run` — Strategos plans, the Phalanx executes, Strategos reviews.
//!
//! The command is a thin shell around `stoic_codex_agent::run_mission`. What it adds is the part
//! the library deliberately refuses to own: resolving providers, printing progress a human
//! can follow while three agents type at once, and deciding an exit code.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use stoic_codex_agent::{
    provisional_name, run_mission, AgentConfig, Event, MissionConfig, MissionEvent, MissionOutcome,
    Operator, OperatorState, Repo, Verdict, VerifyCommand, PHALANX, STRATEGOS,
};

use crate::args::Parsed;
use crate::cmd::{CmdError, CmdResult};
use crate::config::Config;
use crate::model::RemoteModel;
use crate::paths::Paths;
use crate::Ctx;

pub fn run(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    match parsed.rest().first().map(String::as_str) {
        Some("run") => mission(parsed, paths, ctx),
        Some("plan") => plan_only(parsed, paths, ctx),
        Some(other) => Err(CmdError::usage(format!(
            "unknown `mission` subcommand `{other}` (run, plan)"
        ))),
        None => Err(CmdError::usage(
            "usage: stoic-codex mission run <task…> [--agents 3] [--repo DIR] [--verify CMD]\n\
             \x20      stoic-codex mission plan <task…>         show what Strategos would assign",
        )),
    }
}

struct Resolved {
    provider: String,
    model: String,
    repo: Repo,
}

fn resolve(parsed: &Parsed, paths: &Paths) -> Result<(Config, Resolved), CmdError> {
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
    let dir = PathBuf::from(parsed.value("repo").unwrap_or("."));
    let repo = Repo::discover(&dir).map_err(|e| CmdError::failed(e.to_string()))?;
    Ok((
        cfg,
        Resolved {
            provider,
            model,
            repo,
        },
    ))
}

fn agent_count(parsed: &Parsed) -> Result<usize, CmdError> {
    let n: usize = match parsed.value("agents") {
        Some(v) => v
            .parse()
            .map_err(|_| CmdError::usage("--agents must be a number"))?,
        None => 3,
    };
    if n == 0 || n > 8 {
        return Err(CmdError::usage("--agents must be between 1 and 8"));
    }
    Ok(n)
}

fn agent_config(
    parsed: &Parsed,
    cfg: &Config,
    model: &str,
    repo: &Repo,
) -> Result<AgentConfig, CmdError> {
    let mut agent = AgentConfig::new(model);
    agent.system = Some(crate::cmd::agent::system_prompt(repo.root()));
    agent.max_tokens = cfg.providers.max_tokens;
    agent.temperature = cfg.providers.temperature;
    if let Some(v) = parsed.value("max-steps") {
        agent.max_steps = v
            .parse()
            .map_err(|_| CmdError::usage("--max-steps must be a number"))?;
    }
    if agent.max_steps == 0 {
        return Err(CmdError::usage("--max-steps must be at least 1"));
    }
    Ok(agent)
}

/// `stoic-codex mission plan` — one Strategos call, printed. No worktrees, no agents, no spend
/// beyond a single planning turn.
fn plan_only(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let task = parsed.rest()[1.min(parsed.rest().len())..]
        .join(" ")
        .trim()
        .to_string();
    if task.is_empty() {
        return Err(CmdError::usage(
            "usage: stoic-codex mission plan <task…> [--agents 3] [--repo DIR] [--max-attempts N]",
        ));
    }
    let (cfg, resolved) = resolve(parsed, paths)?;
    let n = agent_count(parsed)?;
    let codenames: Vec<String> = (1..=n).map(provisional_name).collect();
    let layout = stoic_codex_agent::strategos::repo_layout(resolved.repo.root());

    let strategos = RemoteModel::new(&resolved.provider, paths, &cfg, Some(&resolved.model))
        .map_err(CmdError::failed)?;
    let mut request =
        stoic_codex_agent::strategos::strategos_plan_prompt(&task, &layout, &codenames);
    request.model = resolved.model.clone();
    request.max_tokens = cfg.providers.max_tokens;

    use stoic_codex_agent::Model;
    let response = strategos
        .complete(&request)
        .map_err(|e| CmdError::failed(format!("{STRATEGOS} could not plan: {e}")))?;
    let mut renamed = stoic_codex_agent::strategos::parse_plan(&response.text(), &codenames, &task);
    // Naming is part of planning, so a preview shows the codes the mission would deploy.
    let renames = stoic_codex_agent::strategos::name_codes(&codenames, &mut renamed);

    if parsed.switch("json") {
        let payload = serde_json::json!({
            "source": renamed.source.label(),
            "commander": STRATEGOS,
            "phalanx": PHALANX,
            "assignments": renamed.assignments.iter()
                .map(|a| serde_json::json!({"code": a.codename, "task": a.task}))
                .collect::<Vec<_>>(),
        });
        let _ = writeln!(
            ctx.out,
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
        return Ok(());
    }

    let _ = writeln!(
        ctx.out,
        "{}",
        row(
            "[strategos]",
            "plan",
            &format!("source={}", renamed.source.label())
        )
    );
    for (old, code) in &renames {
        let _ = writeln!(
            ctx.out,
            "{}",
            row(
                "[strategos]",
                "name",
                &format!("{old:<10} → {}", stoic_codex_agent::code_line(code))
            )
        );
    }
    let _ = writeln!(ctx.out);
    for a in &renamed.assignments {
        let _ = writeln!(
            ctx.out,
            "{}",
            row(&format!("[{}]", a.codename), "task", &a.task)
        );
    }
    if renamed.source == stoic_codex_agent::PlanSource::Fallback {
        let _ = writeln!(
            ctx.err,
            "note: the plan was not machine-readable, so every {PHALANX} would be given the \
             whole task. Raw reply: {}",
            renamed.raw.lines().next().unwrap_or("(empty)")
        );
    }
    Ok(())
}

fn mission(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let task = parsed.rest()[1.min(parsed.rest().len())..]
        .join(" ")
        .trim()
        .to_string();
    if task.is_empty() {
        return Err(CmdError::usage(
            "usage: stoic-codex mission run <task…> [--agents 3] [--repo DIR] [--base REF]\n\
             \x20      [--verify \"cargo test\"] [--no-review] [--no-commit] [--keep-worktrees]\n\
             \x20      [--keep-branches] [--parallel N] [--provider ID] [--model M] [--max-attempts N] [--json]",
        ));
    }
    let (cfg, resolved) = resolve(parsed, paths)?;
    let n = agent_count(parsed)?;

    // Strategos and every Operator share one retry deadline, while each client gets an
    // independent jitter stream. The cap begins at the first retryable error, not mission start.
    let retry_window = crate::retry::MissionRetryWindow::new(Duration::from_secs(
        cfg.providers.mission_retry_window_secs,
    ));
    let quiet = parsed.switch("quiet");
    let json = parsed.switch("json");
    let retry_sink = ctx.err.clone();
    let retry_notice: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |message| {
        if quiet || json {
            return;
        }
        let mut sink = retry_sink.clone();
        let _ = writeln!(sink, "{}", row("[mission]", "retry", message));
    });

    // Strategos gets its own client: a plan cannot be confused with an Operator turn.
    let strategos = RemoteModel::new_for_mission(
        &resolved.provider,
        paths,
        &cfg,
        Some(&resolved.model),
        retry_window.client(STRATEGOS),
        retry_notice.clone(),
    )
    .map_err(CmdError::failed)?;
    let operators = Operator::squad(n, |codename| {
        Box::new(
            RemoteModel::new_for_mission(
                &resolved.provider,
                paths,
                &cfg,
                Some(&resolved.model),
                retry_window.client(codename),
                retry_notice.clone(),
            )
            .expect("provider resolved for the mission"),
        )
    });

    let verify = match parsed.value("verify") {
        Some(raw) => Some(VerifyCommand::parse(raw).ok_or_else(|| {
            CmdError::usage("--verify needs a command, e.g. --verify \"cargo test\"")
        })?),
        None => None,
    };

    let mut mission_cfg = MissionConfig {
        agent: agent_config(parsed, &cfg, &resolved.model, &resolved.repo)?,
        base: parsed.value("base").map(str::to_string),
        verify,
        commit: !parsed.switch("no-commit"),
        keep_worktrees: parsed.switch("keep-worktrees"),
        keep_branches: parsed.switch("keep-branches"),
        worktree_dir: None,
        review: !parsed.switch("no-review"),
        max_parallel: parsed
            .value("parallel")
            .map(|v| {
                v.parse::<usize>()
                    .map_err(|_| CmdError::usage("--parallel must be a number"))
            })
            .transpose()?
            .unwrap_or(n),
    };
    mission_cfg.max_parallel = mission_cfg.max_parallel.max(1);

    // A cloned sink: `Sink` is `Send + Sync` and serialises its own writes, so three agents
    // can report at once without interleaving half-lines. The clone happens inside the
    // closure so it stays `Fn` (the mission may call it from several threads).
    let sink = ctx.err.clone();
    let observe = move |event: MissionEvent| {
        if quiet || json {
            return;
        }
        let mut w = sink.clone();
        // Events with nothing to say (token deltas) print nothing at all, not a blank line:
        // an interleaved log is hard enough to read without gaps in it.
        let line = log_line(&event);
        if !line.is_empty() {
            let _ = writeln!(w, "{line}");
        }
    };

    let report = run_mission(
        &resolved.repo,
        &strategos,
        operators,
        &task,
        &mission_cfg,
        &observe,
    )
    .map_err(|e| CmdError::failed(e.to_string()))?;

    if json {
        let payload = serde_json::json!({
            "task": report.task,
            "base": report.base,
            "strategos": STRATEGOS,
            "plan": {
                "source": report.plan.source.label(),
                "assignments": report.plan.assignments.iter()
                    .map(|a| serde_json::json!({"codename": a.codename, "task": a.task}))
                    .collect::<Vec<_>>(),
            },
            "operators": report.operators.iter().map(|f| serde_json::json!({
                "codename": f.codename,
                "task": f.task,
                "state": f.state.label(),
                "verdict": f.verdict.label(),
                "reason": f.reason,
                "files": f.files.iter().map(|c| c.path.clone()).collect::<Vec<_>>(),
                "applied": f.applied,
                "steps": f.steps,
                "tool_calls": f.tool_calls,
                "tokens": f.usage.total(),
                "verified": f.verified,
                "error": f.error,
                "elapsed_ms": f.elapsed_ms,
            })).collect::<Vec<_>>(),
            "outcome": report.outcome().code(),
            "applied": report.applied_count(),
            "conflicts": report.conflicts().iter()
                .map(|(f, owner, other)| serde_json::json!({"file": f, "owner": owner, "other": other}))
                .collect::<Vec<_>>(),
            "commit": report.commit,
            "commit_error": report.commit_error,
            "review_failed": report.review_failed,
            "cleanup_warnings": report.cleanup_warnings,
            "parallel_ms": report.parallel_ms,
            "review_note": report.review_note,
        });
        let _ = writeln!(
            ctx.out,
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
        return finish(&report);
    }

    let _ = writeln!(ctx.out, "{}", report.table());
    let outcome = report.outcome();
    let _ = writeln!(
        ctx.out,
        "\n{STRATEGOS} · base {base} · {n} operator(s) of {PHALANX} · {ms} ms in parallel · plan {source} · outcome {outcome}",
        base = report.base,
        n = report.operators.len(),
        ms = report.parallel_ms,
        source = report.plan.source.label(),
        outcome = outcome.label()
    );
    let _ = writeln!(ctx.out, "{}", report.review_note);
    for warning in &report.cleanup_warnings {
        let _ = writeln!(ctx.out, "warning  {warning}");
    }

    if report.applied_count() == 0 {
        let _ = writeln!(ctx.out, "applied  nothing");
    } else {
        let _ = writeln!(ctx.out, "applied  {} file(s)", report.applied_count());
        for f in &report.operators {
            for file in &f.applied {
                let _ = writeln!(ctx.out, "  {file}  ← {}", f.codename);
            }
        }
    }
    for (file, owner, other) in report.conflicts() {
        let _ = writeln!(
            ctx.out,
            "conflict {file}: {owner} and {other} both changed it; {owner} wins"
        );
    }
    if let Some(commit) = &report.commit {
        let short: String = commit.chars().take(8).collect();
        let _ = writeln!(ctx.out, "commit   {short}");
    }
    finish(&report)
}

/// Exit 1 only when nothing applicable landed. A partial mission stays exit 0 so useful work
/// is not discarded, while the summary/JSON outcome exposes that the mission was incomplete.
fn finish(report: &stoic_codex_agent::MissionReport) -> CmdResult {
    if report.outcome() == MissionOutcome::Failed {
        let failed = report
            .operators
            .iter()
            .filter(|f| f.state == OperatorState::Failed)
            .count();
        return Err(CmdError::failed(format!(
            "no operator of {PHALANX} produced an applicable change ({failed} failed, {} rejected)",
            report
                .operators
                .iter()
                .filter(|f| f.verdict == Verdict::Rejected)
                .count()
        )));
    }
    Ok(())
}

// ─── the log ─────────────────────────────────────────────────────────────────
//
// A mission prints while three operators type at once, so every line has to be scannable
// without being read: a bracketed code on the left says *who*, a verb says *what*, and the
// rest is `key=value`. That is what makes `[x1-write] tool write_file path=src/auth.rs` a
// log line rather than a sentence — and the codes are short precisely so the column holds.

/// Width of the code column. `[strategos]` is eleven characters; leave one for air.
const TAG_WIDTH: usize = 12;
/// Width of the verb column.
const VERB_WIDTH: usize = 8;

/// One aligned log row: `[code]  verb    body`.
fn row(tag: &str, verb: &str, body: &str) -> String {
    format!("{tag:<TAG_WIDTH$}{verb:<VERB_WIDTH$}{body}")
}

/// The whole of a mission event, as the operator's log prints it.
fn log_line(event: &MissionEvent) -> String {
    match event {
        MissionEvent::Planning { .. } => row("[strategos]", "plan", "reading the layout"),

        MissionEvent::Named { renames } => renames
            .iter()
            .map(|(slot, code)| {
                row(
                    "[strategos]",
                    "name",
                    &format!("{slot:<10} → {}", stoic_codex_agent::code_line(code)),
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),

        MissionEvent::Planned { plan } => {
            let rows: Vec<String> = plan
                .assignments
                .iter()
                .map(|a| {
                    let first: String = a.task.chars().take(72).collect();
                    row(&format!("[{}]", a.codename), "task", &first)
                })
                .collect();
            let mut out = row(
                "[strategos]",
                "planned",
                &format!(
                    "source={} assignments={}",
                    plan.source.label(),
                    plan.assignments.len()
                ),
            );
            for line in rows {
                out.push('\n');
                out.push_str(&line);
            }
            out
        }

        MissionEvent::Deployed { codename, branch } => row(
            "[strategos]",
            "deploy",
            &format!("{codename:<12} branch={branch}"),
        ),

        MissionEvent::Agent { codename, event } => match event {
            Event::ToolCall { name, args } => row(
                &format!("[{codename}]"),
                "tool",
                &format!("{name:<16} {args}"),
            ),
            Event::ToolResult {
                name,
                ok,
                summary,
                ms,
            } => row(
                &format!("[{codename}]"),
                if *ok { "result" } else { "error" },
                &format!("{name:<16} {summary} ({ms}ms)"),
            ),
            _ => String::new(),
        },

        MissionEvent::Verified { codename, ok } => row(
            &format!("[{codename}]"),
            "verify",
            if *ok { "pass" } else { "FAIL" },
        ),

        MissionEvent::Returned { codename, state } => row(
            &format!("[{codename}]"),
            "return",
            &format!("state={}", state.label().replace(' ', "_")),
        ),

        MissionEvent::Reviewing { reports } => {
            row("[strategos]", "review", &format!("reports={reports}"))
        }

        MissionEvent::Reviewed { verdicts, .. } => verdicts
            .iter()
            .map(|(codename, verdict, reason)| {
                let why: String = reason.chars().take(64).collect();
                row(
                    &format!("[{codename}]"),
                    "verdict",
                    &format!("{:<10} {why}", verdict.label()),
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),

        MissionEvent::Applying { codename, file } => {
            row("[strategos]", "apply", &format!("{file} ← {codename}"))
        }

        MissionEvent::Conflict { file, owner, other } => row(
            "[strategos]",
            "conflict",
            &format!("{file} owner={owner} other={other} owner_wins"),
        ),

        MissionEvent::Done { applied, conflicts } => row(
            "[strategos]",
            "done",
            &format!("applied={applied} conflicts={conflicts}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stoic_codex_agent::{Event, OperatorState, Plan, PlanSource, Verdict};

    fn plan() -> Plan {
        Plan {
            assignments: vec![
                stoic_codex_agent::Assignment::with_code("x1-write", "x1-write", "add the parser"),
                stoic_codex_agent::Assignment::with_code(
                    "x2-check",
                    "x2-check",
                    "add tests for it",
                ),
            ],
            source: PlanSource::Strategos,
            raw: String::new(),
        }
    }

    #[test]
    fn every_line_starts_with_a_bracketed_code_on_its_own_column() {
        let events = vec![
            MissionEvent::Planning { task: "t".into() },
            MissionEvent::Planned { plan: plan() },
            MissionEvent::Deployed {
                codename: "x1-write".into(),
                branch: "stoic-codex/x1-write".into(),
            },
            MissionEvent::Agent {
                codename: "x2-check".into(),
                event: Event::ToolCall {
                    name: "write_file".into(),
                    args: "path=src/t.rs".into(),
                },
            },
            MissionEvent::Returned {
                codename: "x1-write".into(),
                state: OperatorState::Succeeded,
            },
            MissionEvent::Reviewing { reports: 2 },
            MissionEvent::Reviewed {
                plan: plan(),
                verdicts: vec![("x1-write".into(), Verdict::Accepted, "good".into())],
            },
            MissionEvent::Applying {
                codename: "x1-write".into(),
                file: "src/parser.rs".into(),
            },
            MissionEvent::Conflict {
                file: "src/a.rs".into(),
                owner: "x1-write".into(),
                other: "x2-check".into(),
            },
            MissionEvent::Done {
                applied: 1,
                conflicts: 0,
            },
        ];
        for event in events {
            let text = log_line(&event);
            for line in text.lines() {
                assert!(line.starts_with('['), "no code tag: {line:?}");
                let close = line.find(']').expect("bracketed code");
                assert!(close < 12, "code too long for the column: {line:?}");
            }
        }
    }

    #[test]
    fn the_naming_line_says_what_the_code_means() {
        let text = log_line(&MissionEvent::Named {
            renames: vec![("slot-1".into(), "x1-write".into())],
        });
        assert!(text.starts_with("[strategos]"), "{text}");
        assert!(text.contains("slot-1"), "{text}");
        assert!(text.contains("x1-write"), "{text}");
        assert!(
            text.contains("the primary logic generator"),
            "the code must explain itself: {text}"
        );
    }

    #[test]
    fn a_plan_prints_one_row_per_operator() {
        let text = log_line(&MissionEvent::Planned { plan: plan() });
        assert_eq!(text.lines().count(), 3, "header plus two operators: {text}");
        assert!(text.contains("source=strategos"), "{text}");
        assert!(text.contains("[x2-check]"), "{text}");
    }

    #[test]
    fn a_conflict_names_a_winner() {
        let text = log_line(&MissionEvent::Conflict {
            file: "src/a.rs".into(),
            owner: "x1-write".into(),
            other: "x2-check".into(),
        });
        assert!(text.contains("owner=x1-write"), "{text}");
        assert!(text.contains("owner_wins"), "{text}");
    }
}
