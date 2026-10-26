//! The real TypeDB backend: a **local daemon**, no cloud, no paid service.
//!
//! TypeDB Community Edition is MPL-2.0 and runs as a single local binary. This module
//! talks to it over loopback gRPC and refuses any other address: the memory store is
//! on the user's machine or it does not exist. There is no key, no account, no quota.
//!
//! Compiled only with `--features typedb`, because it pulls `tonic`/`prost`/`tokio`
//! (~22 MB) and most users never need it: the in-memory backend is the default and the
//! TypeDB path is opted into per project.
//!
//! ## Why `sync`
//!
//! The driver's `sync` feature wraps its async core in a background runtime, so this
//! module can implement the synchronous `MemoryBackend` trait directly. No async leaks
//! into the agent loop or the TUI.

use std::collections::BTreeMap;

use typedb_driver::{
    answer::QueryAnswer, concept::Concept, Addresses, Credentials, DriverOptions, DriverTlsConfig,
    Promise, TransactionType, TypeDBDriver,
};

use crate::backend::{Capabilities, MemoryBackend, Query};
use crate::domain::{EdgeKind, Hit, MemoryEdge, MemoryError, MemoryNode, NodeKind, Result};

/// True for addresses that stay on this machine. Deliberately strict: anything else is
/// rejected before a socket is opened.
pub fn is_loopback(address: &str) -> bool {
    let host = address.rsplit_once(':').map(|(h, _)| h).unwrap_or(address);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

pub struct LocalTypeDb {
    driver: TypeDBDriver,
    database: String,
    address: String,
}

impl std::fmt::Debug for LocalTypeDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalTypeDb")
            .field("address", &self.address)
            .field("database", &self.database)
            .finish()
    }
}

fn map_err(context: &str, e: typedb_driver::Error) -> MemoryError {
    MemoryError::Backend {
        detail: format!("{context}: {e}"),
    }
}

impl LocalTypeDb {
    /// Connect to a TypeDB daemon on loopback.
    pub fn connect(address: &str, database: &str) -> Result<Self> {
        if !is_loopback(address) {
            return Err(MemoryError::Invalid(format!(
                "refusing to connect to `{address}`: Stoic Codex memory is local-only \
                 (use 127.0.0.1:<port>). Hosting a graph database is not required by this design."
            )));
        }
        let driver = TypeDBDriver::new(
            Addresses::try_from_address_str(address)
                .map_err(|e| MemoryError::Invalid(format!("bad address `{address}`: {e}")))?,
            // Local CE daemon: default credentials, TLS off on loopback.
            Credentials::new("admin", "password"),
            DriverOptions::new(DriverTlsConfig::disabled()),
        )
        .map_err(|e| map_err("connect", e))?;
        Ok(LocalTypeDb {
            driver,
            database: database.to_string(),
            address: address.to_string(),
        })
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    /// Create the database if needed and install the schema. Idempotent: safe to call on
    /// every start, which matters when the daemon is started and stopped by the CLI.
    pub fn bootstrap(&self) -> Result<()> {
        let created = match self.driver.databases().create(&self.database) {
            Ok(()) => true,
            Err(e) => {
                // "already exists" is the expected happy path on second run. Anything
                // else is a real failure and must not be swallowed.
                let msg = e.to_string().to_lowercase();
                if !msg.contains("exists") {
                    return Err(map_err("create database", e));
                }
                false
            }
        };
        if created {
            self.define_schema()?;
        } else if self.probe().is_err() {
            // Database exists but was created by something else (or an older schema).
            self.define_schema()?;
        }
        Ok(())
    }

    pub fn define_schema(&self) -> Result<()> {
        let schema = crate::typedb::schema_definition();
        let txn = self
            .driver
            .transaction(&self.database, TransactionType::Schema)
            .map_err(|e| map_err("open schema transaction", e))?;
        txn.query(&schema)
            .resolve()
            .map_err(|e| map_err("define schema", e))?;
        txn.commit()
            .resolve()
            .map_err(|e| map_err("commit schema", e))?;
        Ok(())
    }

    /// Cheap liveness + schema check.
    pub fn probe(&self) -> Result<()> {
        self.run_read("match $n isa memory-node; select $n; limit 1;")
            .map(|_| ())
    }

    fn run_read(&self, typeql: &str) -> Result<Vec<BTreeMap<String, Concept>>> {
        let txn = self
            .driver
            .transaction(&self.database, TransactionType::Read)
            .map_err(|e| map_err("open read transaction", e))?;
        let answer = txn
            .query(typeql)
            .resolve()
            .map_err(|e| map_err("query", e))?;
        let mut out = Vec::new();
        if let QueryAnswer::ConceptRowStream(_header, stream) = answer {
            for row in stream {
                let row = row.map_err(|e| map_err("read row", e))?;
                let mut m = BTreeMap::new();
                // Column names come from the row itself. Hardcoding the list silently
                // dropped every column not named in it — which is exactly how the
                // optional-attribute merge below read nothing back.
                for name in row.get_column_names() {
                    if let Ok(Some(c)) = row.get(name) {
                        m.insert(name.clone(), c.clone());
                    }
                }
                out.push(m);
            }
        }
        Ok(out)
    }

    fn run_write(&self, typeql: &str) -> Result<()> {
        let txn = self
            .driver
            .transaction(&self.database, TransactionType::Write)
            .map_err(|e| map_err("open write transaction", e))?;
        txn.query(typeql)
            .resolve()
            .map_err(|e| map_err("write", e))?;
        txn.commit().resolve().map_err(|e| map_err("commit", e))?;
        Ok(())
    }

    fn all_nodes(&self) -> Result<Vec<MemoryNode>> {
        // Required attributes first, then each optional one, because TypeQL has no
        // optional matches — merging by `uid` in the client is simpler and predictable.
        let base = self.run_read(
            "match $n isa memory-node; $n has uid $uid, has label $label; select $n, $uid, $label;",
        )?;
        let mut by_id: BTreeMap<String, MemoryNode> = BTreeMap::new();
        for row in base {
            let Some(uid) = row.get("uid").and_then(attr_string) else {
                continue;
            };
            let label = row.get("label").and_then(attr_string).unwrap_or_default();
            let kind = row
                .get("n")
                .and_then(|c| match c {
                    Concept::Entity(e) => e.type_().map(|t| t.label().to_string()),
                    Concept::Relation(r) => r.type_().map(|t| t.label().to_string()),
                    _ => None,
                })
                .and_then(|label| NodeKind::parse(&label))
                .unwrap_or(NodeKind::Doc);
            by_id.insert(uid.clone(), MemoryNode::new(uid, kind, label));
        }
        for (column, attr) in [
            ("body", "body"),
            ("provenance", "provenance"),
            ("importance", "importance"),
            ("embedding", "embedding-text"),
        ] {
            let q = format!(
                "match $n isa memory-node; $n has uid $uid, has {attr} $v; select $uid, $v;"
            );
            let rows = self.run_read(&q)?;
            for row in rows {
                let Some(uid) = row.get("uid").and_then(attr_string) else {
                    continue;
                };
                let Some(node) = by_id.get_mut(&uid) else {
                    continue;
                };
                let v = row.get("v");
                match column {
                    "body" => node.text = v.and_then(attr_string).unwrap_or_default(),
                    "provenance" => node.provenance = v.and_then(attr_string),
                    "importance" => node.importance = v.and_then(attr_number).unwrap_or(0.5) as f32,
                    "embedding" => {
                        node.embedding = v.and_then(attr_string).map(|s| {
                            s.split(',')
                                .filter_map(|x| x.trim().parse::<f32>().ok())
                                .collect::<Vec<f32>>()
                        })
                    }
                    _ => {}
                }
            }
        }
        Ok(by_id.into_values().collect())
    }

    fn all_edges(&self) -> Result<Vec<MemoryEdge>> {
        let rows = self.run_read(
            "match $r (source: $a, target: $b) isa memory-edge; \
             $a has uid $ua; $b has uid $ub; select $r, $ua, $ub;",
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (Some(from), Some(to)) = (
                row.get("ua").and_then(attr_string),
                row.get("ub").and_then(attr_string),
            ) else {
                continue;
            };
            // A pattern like `isa memory-edge` answers once per candidate type, so the
            // same edge comes back labelled with the supertype *and* its concrete type.
            // Parsing the label keeps the concrete kind and drops the supertype rows;
            // without this every edge was attributed to `derived-from`.
            let kind = match row.get("r") {
                Some(Concept::Relation(r)) => r.type_().map(|t| t.label().to_string()),
                Some(Concept::RelationType(t)) => Some(t.label().to_string()),
                _ => None,
            }
            .and_then(|label| EdgeKind::parse(&label));
            let Some(kind) = kind else { continue };
            let edge = MemoryEdge::new(from, to, kind);
            if !out.contains(&edge) {
                out.push(edge);
            }
        }
        Ok(out)
    }
}

fn attr_string(c: &Concept) -> Option<String> {
    match c {
        Concept::Attribute(a) => a.value.get_string().map(|s| s.to_string()),
        Concept::Value(v) => v.get_string().map(|s| s.to_string()),
        _ => None,
    }
}

fn attr_number(c: &Concept) -> Option<f64> {
    match c {
        Concept::Attribute(a) => a
            .value
            .get_double()
            .or_else(|| a.value.get_integer().map(|i| i as f64)),
        Concept::Value(v) => v.get_double().or_else(|| v.get_integer().map(|i| i as f64)),
        _ => None,
    }
}

impl MemoryBackend for LocalTypeDb {
    fn id(&self) -> &'static str {
        "typedb"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            vector_search: true,
            lexical_search: true,
            graph_traversal: true,
            inference: true,
            requires_server: true,
            schema_validated: true,
        }
    }

    fn upsert_node(&mut self, node: MemoryNode) -> Result<()> {
        if node.id.trim().is_empty() {
            return Err(MemoryError::Invalid("node id must not be empty".into()));
        }
        // Upsert = delete, then insert, as **two** queries.
        //
        // Not one `match … delete; insert …` query: a pipeline's insert stage runs once
        // per matched row, so on the common path (node does not exist yet) the match
        // yields nothing and the insert is silently skipped. That is a very quiet way to
        // lose every write, and it is why this is two statements rather than one clever
        // one. Verified against TypeDB 3.14.
        self.run_write(&format!(
            "match $n isa memory-node, has uid \"{}\"; delete $n;",
            crate::typedb::escape(&node.id)
        ))?;
        self.run_write(&crate::typedb::insert_node(&node))
    }

    fn upsert_edge(&mut self, edge: MemoryEdge) -> Result<()> {
        for end in [&edge.from, &edge.to] {
            if self.get_node(end)?.is_none() {
                return Err(MemoryError::NotFound(end.clone()));
            }
        }
        self.run_write(&crate::typedb::insert_edge(&edge))
    }

    fn get_node(&self, id: &str) -> Result<Option<MemoryNode>> {
        Ok(self.all_nodes()?.into_iter().find(|n| n.id == id))
    }

    fn neighbours(&self, id: &str, hops: u32, kinds: &[EdgeKind]) -> Result<Vec<Hit>> {
        if self.get_node(id)?.is_none() {
            return Err(MemoryError::NotFound(id.to_string()));
        }
        let nodes: BTreeMap<String, MemoryNode> = self
            .all_nodes()?
            .into_iter()
            .map(|n| (n.id.clone(), n))
            .collect();
        let mut edges = self.all_edges()?;
        if !kinds.is_empty() {
            edges.retain(|e| kinds.contains(&e.kind));
        }
        let adj = crate::rank::adjacency(&edges);
        let hops_map = crate::rank::hop_map(id, hops, &adj);
        let mut out: Vec<Hit> = hops_map
            .iter()
            .filter(|(k, h)| k.as_str() != id && **h > 0)
            .filter_map(|(k, h)| {
                nodes.get(k).map(|n| {
                    let proximity = crate::scoring::graph_proximity(*h);
                    Hit {
                        node: n.clone(),
                        score: proximity,
                        signals: crate::domain::Signals {
                            graph: proximity,
                            ..Default::default()
                        },
                        via: vec![format!("{h} hop(s) from {id}")],
                    }
                })
            })
            .collect();
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.node.id.cmp(&b.node.id))
        });
        Ok(out)
    }

    fn search(&self, query: &Query) -> Result<Vec<Hit>> {
        // Candidates come from the graph; scoring is shared with every other backend.
        // For a local single-user memory (thousands of nodes) loading the graph is
        // cheaper than five round trips to express the same thing in TypeQL, and it
        // keeps one scoring implementation.
        let nodes = self.all_nodes()?;
        let edges = self.all_edges()?;
        let adj = crate::rank::adjacency(&edges);
        let hops = match &query.focus {
            Some(focus) => crate::rank::hop_map(focus, query.hops, &adj),
            None => BTreeMap::new(),
        };
        Ok(crate::rank::rank(
            &nodes,
            query,
            &crate::scoring::Weights::default(),
            now(),
            &hops,
        ))
    }

    fn forget(&mut self, id: &str) -> Result<()> {
        if self.get_node(id)?.is_none() {
            return Err(MemoryError::NotFound(id.to_string()));
        }
        let uid = crate::typedb::escape(id);
        // Relations first: TypeDB refuses to delete an entity that still plays a role.
        // `isa` must precede the role players, otherwise the relation variable is read as
        // a type and `delete` fails with [REP1].
        for dir in ["source", "target"] {
            let other = if dir == "source" { "target" } else { "source" };
            self.run_write(&format!(
                "match $n isa memory-node, has uid \"{uid}\"; \
                 $r isa memory-edge ({dir}: $n, {other}: $m); delete $r;"
            ))?;
        }
        self.run_write(&format!(
            "match $n isa memory-node, has uid \"{uid}\"; delete $n;"
        ))
    }

    fn clear(&mut self) -> Result<()> {
        // Relations first: TypeDB refuses to delete entities that still play a role.
        self.run_write("match $r isa memory-edge; delete $r;")?;
        self.run_write("match $n isa memory-node; delete $n;")?;
        Ok(())
    }

    fn len(&self) -> Result<usize> {
        Ok(self.all_nodes()?.len())
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Sanity checks that need no server. The server-backed behaviour is covered by
/// `tests/typedb_live.rs`, which only runs when `STOIC_CODEX_TYPEDB_ADDR` is set.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_detection_accepts_local_addresses_only() {
        for ok in ["127.0.0.1:1729", "localhost:1729", "[::1]:1729", "::1:1729"] {
            assert!(is_loopback(ok), "{ok} must be treated as local");
        }
        for bad in [
            "typedb.example.com:1729",
            "10.0.0.5:1729",
            "0.0.0.0:1729",
            "https://cloud.typedb.com:1729",
            "192.168.1.2:1729",
        ] {
            assert!(
                !is_loopback(bad),
                "{bad} must be refused: memory is local-only"
            );
        }
    }

    #[test]
    fn remote_addresses_are_refused_before_connecting() {
        let err = LocalTypeDb::connect("typedb.example.com:1729", "db").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("local-only"), "{msg}");
        assert!(
            msg.contains("127.0.0.1"),
            "the error must show the accepted form: {msg}"
        );
    }
}
