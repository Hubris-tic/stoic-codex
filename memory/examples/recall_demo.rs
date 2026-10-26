//! Runs the README example and prints both the packed prompt block and the score
//! breakdown, so the memory layer can be inspected without a server, an API key, or a
//! model. `cargo run --example recall_demo`.

use stoic_codex_memory::{
    EdgeKind, MemoryEdge, MemoryError, MemoryNode, MemoryStore, NodeKind, Query,
};

fn main() -> Result<(), MemoryError> {
    let mut mem = MemoryStore::local();

    mem.remember(
        MemoryNode::new("file:auth.rs", NodeKind::File, "auth.rs")
            .with_text("token verification, session cookies, jwt signing")
            .with_embedding(vec![0.9, 0.1, 0.0])
            .with_provenance("src/auth.rs:1"),
    )?;
    mem.remember(
        MemoryNode::new("sym:verify", NodeKind::Symbol, "verify_token")
            .with_text("verify a jwt signature and check its expiry claim")
            .with_embedding(vec![0.85, 0.15, 0.0])
            .with_provenance("src/auth.rs:88"),
    )?;
    mem.remember(
        MemoryNode::new("commit:4f2c", NodeKind::Commit, "fix expired-token bypass")
            .with_text("expiry was compared against the wrong clock; tokens stayed valid")
            .with_embedding(vec![0.7, 0.2, 0.1])
            .with_provenance("4f2c1ab"),
    )?;
    mem.remember(
        MemoryNode::new(
            "decision:worktrees",
            NodeKind::Decision,
            "isolate agents in worktrees",
        )
        .with_text("each agent gets its own git worktree so concurrent edits never collide")
        .with_importance(1.0)
        .with_embedding(vec![0.1, 0.2, 0.9])
        .with_provenance("docs/adr/0007"),
    )?;
    mem.remember(
        MemoryNode::new("file:render.rs", NodeKind::File, "render.rs")
            .with_text("draw the status line and squad board")
            .with_provenance("src/tui/render.rs:1"),
    )?;

    mem.link(MemoryEdge::new(
        "sym:verify",
        "file:auth.rs",
        EdgeKind::Calls,
    ))?;
    mem.link(MemoryEdge::new(
        "commit:4f2c",
        "file:auth.rs",
        EdgeKind::Fixes,
    ))?;

    let q = Query::new("is the token expiry check correct")
        .with_embedding(vec![0.92, 0.08, 0.0])
        .with_focus("file:auth.rs");

    let hits = mem.recall(&q)?;
    println!(
        "backend: {}  capabilities: {:?}",
        mem.backend_id(),
        mem.capabilities()
    );
    println!("\n--- packed context (budget 600 chars) ---");
    let ctx = mem.context(&hits, 600);
    print!("{ctx}");
    println!("--- {} chars of a 600 budget ---\n", ctx.chars().count());

    println!("--- score breakdown ---");
    for line in mem.explain(&hits) {
        println!("{line}");
    }

    println!("\n--- one hop from file:auth.rs ---");
    for h in mem.related("file:auth.rs", 1)? {
        println!("{} ({:?}) via {:?}", h.node.id, h.node.kind, h.via);
    }
    Ok(())
}
