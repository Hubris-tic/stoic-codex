//! End-to-end local stack: start a TypeDB daemon on loopback, use it as the memory
//! backend, then stop it. Nothing leaves the machine and no key is involved.
//!
//! ```bash
//! ZENITHAL_TYPEDB_BIN=/path/to/typedb cargo run --features typedb --example local_stack
//! ```
//!
//! With the feature off, the same program runs on the in-memory backend.

use zenithal_memory::MemoryStore;
// The store types are only touched by the daemon-backed branch, so without the feature
// they would be unused imports.
#[cfg(feature = "typedb")]
use zenithal_memory::{EdgeKind, MemoryEdge, MemoryNode, NodeKind, Query};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "typedb")]
    {
        let mut config = zenithal_memory::daemon::DaemonConfig::default();
        config.address =
            std::env::var("ZENITHAL_DEMO_ADDR").unwrap_or_else(|_| "127.0.0.1:1730".into());
        config.database = "zenithal_demo".into();
        if let Ok(bin) = std::env::var("ZENITHAL_TYPEDB_BIN") {
            println!("using daemon binary from ZENITHAL_TYPEDB_BIN");
            let _ = bin;
        }

        let (mut mem, daemon) = MemoryStore::open_local(&config)?;
        match &daemon {
            Some(d) => println!(
                "backend={} daemon={} provenance={:?}",
                mem.backend_id(),
                d.address(),
                d.provenance()
            ),
            None => println!("backend={} (no daemon; in-memory)", mem.backend_id()),
        }

        mem.remember(
            MemoryNode::new("file:auth.rs", NodeKind::File, "auth.rs")
                .with_text("token verification and session handling")
                .with_embedding(vec![0.9, 0.1])
                .with_provenance("src/auth.rs:1"),
        )?;
        mem.remember(
            MemoryNode::new("commit:4f2c", NodeKind::Commit, "fix expired-token bypass")
                .with_text("expiry compared against the wrong clock")
                .with_embedding(vec![0.7, 0.3]),
        )?;
        mem.link(MemoryEdge::new(
            "commit:4f2c",
            "file:auth.rs",
            EdgeKind::Fixes,
        ))?;

        let hits = mem.recall(
            &Query::new("token expiry")
                .with_embedding(vec![0.9, 0.1])
                .with_focus("file:auth.rs"),
        )?;
        println!("\n--- packed context ---\n{}", mem.context(&hits, 600));
        println!("--- why ---");
        for line in mem.explain(&hits) {
            println!("{line}");
        }

        drop(daemon);
        println!("\ndaemon stopped");
        return Ok(());
    }

    #[cfg(not(feature = "typedb"))]
    {
        let mem = MemoryStore::local();
        println!(
            "backend={} (built without the `typedb` feature)",
            mem.backend_id()
        );
        Ok(())
    }
}
