//! Live integration test against a **local** TypeDB daemon.
//!
//! Runs only when `STOIC_CODEX_TYPEDB_ADDR` is set (e.g. `127.0.0.1:1729`), so a normal
//! `cargo test` — and CI without a daemon — stays green and hermetic. There is no
//! network access and no hosted service involved: TypeDB CE is a local binary the user
//! starts (or which `stoic-codex` starts for them).
//!
//! ```bash
//! typedb server --server.listen-address 127.0.0.1:1729 \
//!               --storage.data-directory ~/.stoic-codex/typedb &
//! STOIC_CODEX_TYPEDB_ADDR=127.0.0.1:1729 cargo test --features typedb --test typedb_live
//! ```

#![cfg(feature = "typedb")]

use stoic_codex_memory::typedb_driver::LocalTypeDb;
use stoic_codex_memory::MemoryBackend;
use stoic_codex_memory::{EdgeKind, MemoryEdge, MemoryNode, NodeKind, Query};

fn backend(test: &str) -> Option<LocalTypeDb> {
    let addr = std::env::var("STOIC_CODEX_TYPEDB_ADDR")
        .or_else(|_| std::env::var("ZENITHAL_TYPEDB_ADDR"))
        .ok()?;
    // One database per test: cargo runs tests in parallel, and a shared database means
    // each test's `clear()` wipes another test's fixtures mid-run.
    let db = format!("stoic_codex_memory_test_{}_{}", std::process::id(), test);
    let mut b = LocalTypeDb::connect(&addr, &db)
        .unwrap_or_else(|e| panic!("connect to local TypeDB at {addr}: {e}"));
    b.bootstrap()
        .unwrap_or_else(|e| panic!("bootstrap schema: {e}"));
    let _ = b.clear();
    Some(b)
}

macro_rules! live {
    ($name:expr) => {
        match backend($name) {
            Some(b) => b,
            None => {
                eprintln!("skipped: set STOIC_CODEX_TYPEDB_ADDR to run live TypeDB tests");
                return;
            }
        }
    };
}

fn node(id: &str, kind: NodeKind, label: &str, text: &str) -> MemoryNode {
    MemoryNode::new(id, kind, label)
        .with_text(text)
        .with_importance(0.7)
}

#[test]
fn schema_bootstraps_and_is_idempotent() {
    let b = live!("schema");
    // Second bootstrap on an existing database must not fail.
    b.bootstrap().expect("bootstrap must be idempotent");
    b.probe().expect("schema must be present");
}

#[test]
fn round_trips_nodes_edges_and_embeddings() {
    let mut b = live!("roundtrip");
    b.upsert_node(
        node("file:auth.rs", NodeKind::File, "auth.rs", "verify token")
            .with_embedding(vec![0.9, 0.1, 0.0]),
    )
    .unwrap();
    b.upsert_node(
        node(
            "sym:verify",
            NodeKind::Symbol,
            "verify_token",
            "check jwt signature",
        )
        .with_embedding(vec![0.8, 0.2, 0.0]),
    )
    .unwrap();
    b.upsert_edge(MemoryEdge::new(
        "sym:verify",
        "file:auth.rs",
        EdgeKind::Calls,
    ))
    .unwrap();

    assert_eq!(
        b.len().unwrap(),
        2,
        "both nodes must be stored exactly once"
    );

    let got = b
        .get_node("file:auth.rs")
        .unwrap()
        .expect("node must come back");
    assert_eq!(
        got.kind,
        NodeKind::File,
        "the schema type must round-trip as the node kind"
    );
    assert_eq!(got.label, "auth.rs");
    assert_eq!(got.text, "verify token");
    assert_eq!(
        got.embedding.as_deref(),
        Some(&[0.9f32, 0.1, 0.0][..]),
        "embeddings survive the text encoding"
    );

    // Upsert replaces, it does not duplicate.
    b.upsert_node(node(
        "file:auth.rs",
        NodeKind::File,
        "auth.rs",
        "verify token, v2",
    ))
    .unwrap();
    assert_eq!(b.len().unwrap(), 2);
    assert_eq!(
        b.get_node("file:auth.rs").unwrap().unwrap().text,
        "verify token, v2"
    );
}

#[test]
fn an_edge_to_a_missing_node_is_refused() {
    let mut b = live!("missing_edge");
    b.upsert_node(node("file:solo.rs", NodeKind::File, "solo.rs", "alone"))
        .unwrap();
    let err = b.upsert_edge(MemoryEdge::new("file:solo.rs", "ghost", EdgeKind::Calls));
    assert!(
        err.is_err(),
        "an edge to a non-existent node must be refused"
    );
    let msg = err.unwrap_err().to_string();
    assert!(msg.contains("not found"), "{msg}");
}

#[test]
fn traverses_the_graph_in_both_directions() {
    let mut b = live!("traversal");
    b.upsert_node(node(
        "file:auth.rs",
        NodeKind::File,
        "auth.rs",
        "verify token",
    ))
    .unwrap();
    b.upsert_node(node(
        "sym:verify",
        NodeKind::Symbol,
        "verify_token",
        "check jwt",
    ))
    .unwrap();
    b.upsert_node(node(
        "commit:4f2c",
        NodeKind::Commit,
        "fix expiry",
        "clock bug",
    ))
    .unwrap();
    b.upsert_edge(MemoryEdge::new(
        "sym:verify",
        "file:auth.rs",
        EdgeKind::Calls,
    ))
    .unwrap();
    b.upsert_edge(MemoryEdge::new(
        "commit:4f2c",
        "file:auth.rs",
        EdgeKind::Fixes,
    ))
    .unwrap();

    let nbrs = b.neighbours("file:auth.rs", 1, &[]).unwrap();
    assert_eq!(nbrs.len(), 2, "both directions must be traversed: {nbrs:?}");

    let fixes = b.neighbours("file:auth.rs", 1, &[EdgeKind::Fixes]).unwrap();
    assert_eq!(fixes.len(), 1);
    assert_eq!(
        fixes[0].node.id, "commit:4f2c",
        "relation type must round-trip"
    );

    let two = b.neighbours("sym:verify", 2, &[]).unwrap();
    let ids: Vec<&str> = two.iter().map(|h| h.node.id.as_str()).collect();
    assert_eq!(
        ids.iter().filter(|i| **i == "commit:4f2c").count(),
        1,
        "{ids:?}"
    );
}

#[test]
fn hybrid_search_ranks_semantic_then_lexical() {
    let mut b = live!("hybrid");
    b.upsert_node(
        node(
            "file:auth.rs",
            NodeKind::File,
            "auth.rs",
            "token verification",
        )
        .with_embedding(vec![0.95, 0.05]),
    )
    .unwrap();
    b.upsert_node(node(
        "file:render.rs",
        NodeKind::File,
        "render.rs",
        "draw the status line",
    ))
    .unwrap();

    let semantic = b
        .search(&Query::new("authentication").with_embedding(vec![0.9, 0.1]))
        .unwrap();
    assert_eq!(semantic[0].node.id, "file:auth.rs");

    let lexical = b.search(&Query::new("status line")).unwrap();
    assert_eq!(lexical[0].node.id, "file:render.rs");

    for hit in semantic.iter().chain(lexical.iter()) {
        assert!(
            (0.0..=1.0).contains(&hit.score),
            "score out of range: {hit:?}"
        );
    }
}

#[test]
fn forget_removes_a_node_and_the_edges_that_touch_it() {
    let mut b = live!("forget");
    b.upsert_node(node("file:a.rs", NodeKind::File, "a.rs", "alpha"))
        .unwrap();
    b.upsert_node(node("file:b.rs", NodeKind::File, "b.rs", "beta"))
        .unwrap();
    b.upsert_edge(MemoryEdge::new("file:a.rs", "file:b.rs", EdgeKind::Imports))
        .unwrap();

    b.forget("file:b.rs").expect("forget must delete the node");
    assert!(
        b.get_node("file:b.rs").unwrap().is_none(),
        "the node is still there"
    );

    // TypeDB refuses to delete an entity that still plays a role, so the edges must go
    // first; if they did not, traversal would still find a ghost.
    let near = b.neighbours("file:a.rs", 1, &[]).unwrap();
    assert!(near.is_empty(), "a dangling edge survived: {near:?}");
    assert!(matches!(
        b.forget("file:b.rs"),
        Err(stoic_codex_memory::MemoryError::NotFound(_))
    ));
}

#[test]
fn clear_empties_the_store() {
    let mut b = live!("clear");
    b.upsert_node(node("file:a.rs", NodeKind::File, "a.rs", "a"))
        .unwrap();
    b.upsert_node(node("file:b.rs", NodeKind::File, "b.rs", "b"))
        .unwrap();
    b.upsert_edge(MemoryEdge::new("file:a.rs", "file:b.rs", EdgeKind::Imports))
        .unwrap();
    b.clear().unwrap();
    assert_eq!(b.len().unwrap(), 0);
    assert!(b.get_node("file:a.rs").unwrap().is_none());
}
