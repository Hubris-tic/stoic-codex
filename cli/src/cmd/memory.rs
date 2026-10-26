//! `stoic-codex memory …` and `stoic-codex context …`.
//!
//! Every command opens the store through one helper, so the backend rules (local daemon →
//! file store → nothing) are applied identically everywhere and the choice is always
//! reported rather than assumed.

use std::io::Write;

use stoic_codex_memory::{
    BackendChoice, EdgeKind, MemoryEdge, MemoryNode, NodeKind, OpenedMemory, Query,
};

use crate::args::Parsed;
use crate::cmd::{CmdError, CmdResult};
use crate::config::Config;
use crate::paths::Paths;
use crate::session;
use crate::Ctx;

/// Open the store described by the config, keeping the config for the caller's own use.
fn open(paths: &Paths) -> Result<(Config, OpenedMemory), CmdError> {
    let cfg = Config::load_for(paths).map_err(CmdError::failed)?;
    cfg.validate().map_err(CmdError::failed)?;
    let opened = crate::cmd::open_store(paths)?;
    Ok((cfg, opened))
}

pub fn run(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    match parsed.rest().first().map(String::as_str) {
        Some("init") => init(paths, ctx),
        Some("remember") => remember(parsed, paths, ctx),
        Some("link") => link(parsed, paths, ctx),
        Some("recall") => recall(parsed, paths, ctx, false),
        Some("why") => recall(parsed, paths, ctx, true),
        Some("forget") => forget(parsed, paths, ctx),
        Some("stats") => stats(parsed, paths, ctx),
        Some(other) => Err(CmdError::usage(format!(
            "unknown `memory` subcommand `{other}` (init, remember, link, recall, why, forget, stats)"
        ))),
        None => Err(CmdError::usage(
            "usage: stoic-codex memory <init|remember|link|recall|why|forget|stats>",
        )),
    }
}

fn init(paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let (_cfg, opened) = open(paths)?;
    let _ = writeln!(ctx.out, "backend  {}", opened.label());
    let _ = writeln!(
        ctx.out,
        "entries  {}",
        opened
            .store
            .len()
            .map_err(|e| CmdError::failed(e.to_string()))?
    );
    if opened.choice == BackendChoice::File {
        let _ = writeln!(ctx.out, "file     {}", paths.memory_file().display());
    }
    Ok(())
}

fn remember(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let rest = parsed.rest();
    let id = rest.get(1).ok_or_else(|| {
        CmdError::usage("usage: stoic-codex memory remember <id> <label> [--kind file] [--text T] [--importance 0..1]")
    })?;
    let label = rest
        .get(2)
        .ok_or_else(|| CmdError::usage("usage: stoic-codex memory remember <id> <label>"))?;

    let kind_name = parsed.value("kind").unwrap_or("doc");
    let kind = NodeKind::parse(kind_name).ok_or_else(|| {
        CmdError::usage(format!(
            "unknown --kind `{kind_name}` (file, symbol, commit, task, decision, error, agent, doc)"
        ))
    })?;

    let mut node = MemoryNode::new(id.clone(), kind, label.clone());
    // `--text` supplies the body; `--from` reads it from a file, which is how you point
    // memory at a design note without retyping it.
    if let Some(text) = parsed.value("text") {
        node = node.with_text(text);
    } else if let Some(path) = parsed.value("from") {
        let text = std::fs::read_to_string(path)
            .map_err(|e| CmdError::failed(format!("cannot read {path}: {e}")))?;
        node = node.with_text(text);
    }
    if let Some(p) = parsed.value("provenance") {
        node = node.with_provenance(p);
    }
    if let Some(i) = parsed.value("importance") {
        let value: f32 = i
            .parse()
            .map_err(|_| CmdError::usage(format!("--importance must be a number, got `{i}`")))?;
        if !(0.0..=1.0).contains(&value) {
            return Err(CmdError::usage("--importance must be between 0 and 1"));
        }
        node = node.with_importance(value);
    }

    let (_cfg, mut opened) = open(paths)?;
    opened
        .store
        .remember(node)
        .map_err(|e| CmdError::failed(e.to_string()))?;
    let _ = writeln!(
        ctx.out,
        "remembered {id} ({}) in {} — {} entries",
        kind.as_str(),
        opened.choice.as_str(),
        opened
            .store
            .len()
            .map_err(|e| CmdError::failed(e.to_string()))?
    );
    Ok(())
}

fn link(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let rest = parsed.rest();
    let (Some(from), Some(to), Some(kind_name)) = (rest.get(1), rest.get(2), rest.get(3)) else {
        return Err(CmdError::usage(
            "usage: stoic-codex memory link <from-id> <to-id> <kind>  (kind: calls, imports, fixes, regresses, blocks, supersedes, derived_from, authored_by)",
        ));
    };
    let kind = EdgeKind::parse(kind_name)
        .ok_or_else(|| CmdError::usage(format!("unknown edge kind `{kind_name}`")))?;
    let (_cfg, mut opened) = open(paths)?;
    opened
        .store
        .link(MemoryEdge::new(from.clone(), to.clone(), kind))
        .map_err(|e| CmdError::failed(e.to_string()))?;
    let _ = writeln!(ctx.out, "linked {from} -> {to} ({})", kind.as_str());
    Ok(())
}

/// Build a query from `text` plus the shared tuning flags. The caller passes the query
/// text explicitly because `context <query>` and `memory recall <query>` sit at different
/// positions in their argument lists.
fn query_from(parsed: &Parsed, text: Option<&String>) -> Result<Query, CmdError> {
    let text = text.ok_or_else(|| {
        CmdError::usage("usage: stoic-codex memory recall <query> [--focus ID] [--limit N]")
    })?;
    let mut query = Query::new(text.clone());
    if let Some(focus) = parsed.value("focus") {
        query = query.with_focus(focus);
    }
    if let Some(limit) = parsed.value("limit") {
        query.limit = limit
            .parse()
            .map_err(|_| CmdError::usage(format!("--limit must be a number, got `{limit}`")))?;
        if query.limit == 0 {
            return Err(CmdError::usage("--limit must be at least 1"));
        }
    }
    if let Some(hops) = parsed.value("hops") {
        query.hops = hops
            .parse()
            .map_err(|_| CmdError::usage("--hops must be a number"))?;
    }
    Ok(query)
}

fn recall(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx, explain: bool) -> CmdResult {
    let query = query_from(parsed, parsed.rest().get(1))?;
    let (_cfg, opened) = open(paths)?;
    let hits = opened
        .store
        .recall(&query)
        .map_err(|e| CmdError::failed(e.to_string()))?;

    if parsed.switch("json") {
        let rows: Vec<serde_json::Value> = hits
            .iter()
            .map(|h| {
                serde_json::json!({
                    "id": h.node.id,
                    "kind": h.node.kind.as_str(),
                    "label": h.node.label,
                    "score": h.score,
                    "signals": {
                        "cosine": h.signals.cosine,
                        "bm25": h.signals.bm25,
                        "graph": h.signals.graph,
                        "recency": h.signals.recency,
                        "importance": h.signals.importance,
                    },
                    "via": h.via,
                })
            })
            .collect();
        let _ = writeln!(
            ctx.out,
            "{}",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
        return Ok(());
    }

    if hits.is_empty() {
        // An empty result is a normal outcome, and saying *why* saves the user from
        // concluding that memory is broken.
        let _ = writeln!(
            ctx.out,
            "no memories matched `{}` (backend {}, {} entries)",
            query.text,
            opened.choice.as_str(),
            opened
                .store
                .len()
                .map_err(|e| CmdError::failed(e.to_string()))?
        );
        return Ok(());
    }

    if explain {
        for line in opened.store.explain(&hits) {
            let _ = writeln!(ctx.out, "{line}");
        }
    } else {
        for hit in &hits {
            let _ = writeln!(
                ctx.out,
                "{:.2}  [{:<8}] {:<32} {}",
                hit.score,
                hit.node.kind.as_str(),
                hit.node.label,
                hit.node.id
            );
        }
    }
    Ok(())
}

fn forget(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let Some(id) = parsed.rest().get(1) else {
        return Err(CmdError::usage("usage: stoic-codex memory forget <id>"));
    };
    let (_cfg, mut opened) = open(paths)?;
    opened
        .store
        .forget(id)
        .map_err(|e| CmdError::failed(e.to_string()))?;
    let _ = writeln!(
        ctx.out,
        "forgot {id} — {} entries left",
        opened
            .store
            .len()
            .map_err(|e| CmdError::failed(e.to_string()))?
    );
    Ok(())
}

fn stats(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let (cfg, opened) = open(paths)?;
    let len = opened
        .store
        .len()
        .map_err(|e| CmdError::failed(e.to_string()))?;
    let caps = opened.store.capabilities();

    if parsed.switch("json") {
        let payload = serde_json::json!({
            "backend": opened.choice.as_str(),
            "entries": len,
            "persistent": opened.choice.persistent(),
            "inference": caps.inference,
            "config": paths.config_file().to_string_lossy(),
            "file_store": paths.memory_file().to_string_lossy(),
            "context_budget": cfg.memory.context_budget,
            "context_token_budget": cfg.memory.context_token_budget,
        });
        let _ = writeln!(
            ctx.out,
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
        return Ok(());
    }

    let _ = writeln!(ctx.out, "backend      {}", opened.label());
    let _ = writeln!(ctx.out, "entries      {len}");
    let _ = writeln!(
        ctx.out,
        "inference    {}",
        if caps.inference {
            "yes (TypeDB functions)"
        } else {
            "no"
        }
    );
    let _ = writeln!(
        ctx.out,
        "persistent   {}",
        if opened.choice.persistent() {
            "yes"
        } else {
            "no"
        }
    );
    let _ = writeln!(ctx.out, "config       {}", paths.config_file().display());
    let _ = writeln!(ctx.out, "file store   {}", paths.memory_file().display());
    let _ = writeln!(
        ctx.out,
        "budget       {} chars / {} tokens",
        cfg.memory.context_budget, cfg.memory.context_token_budget
    );
    Ok(())
}

/// `stoic-codex context <query>` — the whole point of the memory layer: a prompt block.
pub fn context(parsed: &Parsed, paths: &Paths, ctx: &mut Ctx) -> CmdResult {
    let query = query_from(parsed, parsed.rest().first())?;
    let (cfg, opened) = open(paths)?;
    let hits = opened
        .store
        .recall(&query)
        .map_err(|e| CmdError::failed(e.to_string()))?;

    let budget = match parsed.value("budget") {
        Some(b) => b
            .parse()
            .map_err(|_| CmdError::usage(format!("--budget must be a number, got `{b}`")))?,
        None => cfg.memory.context_budget,
    };
    let token_budget = match parsed.value("max-tokens") {
        Some(b) => b
            .parse()
            .map_err(|_| CmdError::usage(format!("--max-tokens must be a number, got `{b}`")))?,
        None => cfg.memory.context_token_budget,
    };
    if token_budget == 0 {
        return Err(CmdError::usage("--max-tokens must be at least 1"));
    }
    let block = session::pack_memory(&opened.store, &hits, budget, token_budget);
    let composed = session::compose(&query.text, &block);

    if parsed.switch("json") {
        let payload = serde_json::json!({
            "backend": opened.choice.as_str(),
            "budget": budget,
            "token_budget": token_budget,
            "recalled_entries": hits.len(),
            "omitted_entries": hits.len().saturating_sub(composed.memory_entries),
            "memory_entries": composed.memory_entries,
            "memory_characters": composed.memory_characters,
            "memory_tokens": composed.memory_tokens,
            "system": composed.system,
            "user": composed.user,
        });
        let _ = writeln!(
            ctx.out,
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
        return Ok(());
    }

    if parsed.switch("system-only") {
        let _ = write!(ctx.out, "{}", composed.system);
        return Ok(());
    }

    let _ = writeln!(ctx.out, "--- system ---");
    let _ = write!(ctx.out, "{}", composed.system);
    let _ = writeln!(ctx.out, "\n--- user ---");
    let _ = writeln!(ctx.out, "{}", composed.user);
    let _ = writeln!(
        ctx.err,
        "injected {} memor{} ({} chars ≈ {} tok; caps {} chars / {} tok) from {}",
        composed.memory_entries,
        if composed.memory_entries == 1 {
            "y"
        } else {
            "ies"
        },
        composed.memory_characters,
        composed.memory_tokens,
        budget,
        token_budget,
        opened.choice.as_str()
    );
    Ok(())
}
