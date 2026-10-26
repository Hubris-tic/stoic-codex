//! Diagnostic: exercise the TypeDB backend against a local daemon.
//! `ZENITHAL_TYPEDB_ADDR=127.0.0.1:1729 cargo run --features typedb --example typedb_probe`
#[cfg(feature = "typedb")]
fn main() {
    use zenithal_memory::typedb_driver::LocalTypeDb;
    use zenithal_memory::{MemoryBackend, MemoryNode, NodeKind};

    let addr = std::env::var("ZENITHAL_TYPEDB_ADDR").unwrap_or_else(|_| "127.0.0.1:1729".into());
    let mut b = LocalTypeDb::connect(&addr, "zenithal_probe_manual").expect("connect");
    b.clear().ok();
    b.bootstrap().expect("bootstrap");
    println!("len after bootstrap: {:?}", b.len());
    b.upsert_node(
        MemoryNode::new("file:auth.rs", NodeKind::File, "auth.rs").with_text("verify token"),
    )
    .unwrap();
    println!("len after insert: {:?}", b.len());
    println!("get: {:?}", b.get_node("file:auth.rs"));
}

#[cfg(not(feature = "typedb"))]
fn main() {
    println!("run with --features typedb");
}
