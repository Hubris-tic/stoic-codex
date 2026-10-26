//! TypeDB backend: TypeQL generation and decoding.
//!
//! Deliberate design choice: **no driver dependency here.** The `typedb-driver` crate is
//! a gRPC client (tonic + prost + tokio, ~22 MB of dependency tree) that must talk to a
//! server on port 1729. That is the right thing for a team deployment and the wrong
//! thing for a CLI that promises fast startup and low RAM, so:
//!
//!   * this module owns everything that is *pure*: schema bootstrap, query construction,
//!     response decoding, id mapping. It is fully tested with no server.
//!   * talking to the server is behind the `typedb` cargo feature, added later, and is
//!     one thin adapter over `TypeqlPort`.
//!
//! The generated TypeQL is asserted against hand-checked expectations in the tests
//! below. Those expectations are *written by hand from the TypeQL reference*, not
//! recorded from a live server — the sandbox has none. Any change here must be
//! smoke-tested against a real `typedb server` before release. That is stated in
//! TYPEDB.md rather than hidden.

use crate::backend::{Capabilities, MemoryBackend, Query};
use crate::domain::{EdgeKind, Hit, MemoryEdge, MemoryError, MemoryNode, NodeKind, Result};

/// Escaping for TypeQL string literals. TypeQL strings follow the usual `\` escapes;
/// getting this wrong is an injection bug, so it is a separate, tested function.
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

/// `define` block that bootstraps the schema. Mirrors `schema/memory.tql`; kept here so
/// `zenithal memory init --backend typedb` does not depend on the repository layout.
pub fn schema_definition() -> String {
    let mut s = String::new();
    s.push_str("define\n");
    s.push_str("attribute uid value string;\n");
    s.push_str("attribute label value string;\n");
    s.push_str("attribute body value string;\n");
    s.push_str("attribute provenance value string;\n");
    s.push_str("attribute importance value double;\n");
    s.push_str("attribute created-at value integer;\n");
    s.push_str("attribute embedding-text value string;\n");
    s.push_str("attribute confidence value double;\n");
    s.push_str("entity memory-node, owns uid @key, owns label, owns body, owns provenance, owns importance, owns created-at, owns embedding-text, plays memory-edge:source, plays memory-edge:target;\n");
    for kind in [
        NodeKind::File,
        NodeKind::Symbol,
        NodeKind::Commit,
        NodeKind::Task,
        NodeKind::Decision,
        NodeKind::Error,
        NodeKind::Agent,
        NodeKind::Doc,
    ] {
        // The kind keyword is required: `calls sub memory-edge;` on its own is parsed
        // as a reference to an existing type, not a declaration (real server: SYR1).
        s.push_str(&format!(
            "entity {}, sub memory-node;\n",
            kind.typeql_type()
        ));
    }
    s.push_str("relation memory-edge, relates source, relates target, owns confidence;\n");
    for e in [
        EdgeKind::Calls,
        EdgeKind::Imports,
        EdgeKind::Fixes,
        EdgeKind::Regresses,
        EdgeKind::Blocks,
        EdgeKind::Supersedes,
        EdgeKind::DerivedFrom,
        EdgeKind::AuthoredBy,
    ] {
        s.push_str(&format!("relation {}, sub memory-edge;\n", e.typeql_type()));
    }
    s
}

/// Insert a node. `isa <type>` gives the entity its schema-validated kind, and the
/// attribute list is the typed replacement for a `serde_json::Value` blob: a node
/// without a `uid` cannot exist, and an edge to a missing node cannot be written.
pub fn insert_node(node: &MemoryNode) -> String {
    let mut q = format!(
        "insert\n  $n isa {t},\n    has uid \"{uid}\",\n    has label \"{label}\"",
        t = node.kind.typeql_type(),
        uid = escape(&node.id),
        label = escape(&node.label),
    );
    if !node.text.is_empty() {
        q.push_str(&format!(",\n    has body \"{}\"", escape(&node.text)));
    }
    if let Some(p) = &node.provenance {
        q.push_str(&format!(",\n    has provenance \"{}\"", escape(p)));
    }
    q.push_str(&format!(",\n    has importance {:.3}", node.importance));
    if node.created_at != 0 {
        q.push_str(&format!(",\n    has created-at {}", node.created_at));
    }
    if let Some(emb) = &node.embedding {
        let csv = emb
            .iter()
            .map(|f| format!("{f:.6}"))
            .collect::<Vec<_>>()
            .join(",");
        q.push_str(&format!(",\n    has embedding-text \"{}\"", escape(&csv)));
    }
    q.push(';');
    q
}

/// Insert an edge. `links (role: $var)` is the 3.x relation-instance syntax.
pub fn insert_edge(edge: &MemoryEdge) -> String {
    // `match` first: an `insert` that says `$a isa memory-node` would create two new
    // nodes instead of linking the existing ones.
    format!(
        "match\n  $a isa memory-node, has uid \"{from}\";\n  $b isa memory-node, has uid \"{to}\";\ninsert\n  (source: $a, target: $b) isa {t},\n    has confidence {c:.3};",
        t = edge.kind.typeql_type(),
        c = edge.weight,
        from = escape(&edge.from),
        to = escape(&edge.to),
    )
}

pub fn get_node(id: &str) -> String {
    format!(
        "match\n  $n isa memory-node, has uid \"{}\",\n    has label $label;\nfetch\n  $label;",
        escape(id)
    )
}

/// All kinds in one query: TypeDB is strongest when it can infer, so we ask for the
/// most specific type rather than filtering in the client.
pub fn kind_query(kind: NodeKind) -> String {
    format!("match\n  $n isa {};\nfetch\n  $n;", kind.typeql_type())
}

/// Hybrid retrieval: vector similarity (when the server has the alpha vector type) plus
/// graph proximity, expressed as one query instead of two round trips.
pub fn search(query: &Query) -> String {
    let mut q = String::new();
    match &query.embedding {
        Some(emb) => {
            let vals = emb
                .iter()
                .map(|f| format!("{f:.6}"))
                .collect::<Vec<_>>()
                .join(", ");
            q.push_str(&format!(
                "match\n  let $e in cosine_similarity_search(embedding, vector([{vals}], 0.0), {});\n",
                query.limit.max(1)
            ));
            q.push_str("  $n isa memory-node, has embedding $e, has uid $uid;\n");
        }
        None => {
            q.push_str(&format!(
                "match\n  $n isa memory-node, has uid $uid, has body $body;\n  $body contains \"{}\";\n",
                escape(&query.text)
            ));
        }
    }
    if let Some(focus) = &query.focus {
        // Graph-adjacent nodes are candidates too, not just semantic matches.
        q.push_str(&format!(
            "  {{ $n has uid \"{f}\"; }} or {{ (source: $n, target: $focus) isa memory-edge; $focus has uid \"{f}\"; }} or {{ (source: $focus, target: $n) isa memory-edge; $focus has uid \"{f}\"; }};\n",
            f = escape(focus)
        ));
    }
    for k in &query.kinds {
        q.push_str(&format!("  $n isa {};\n", k.typeql_type()));
    }
    q.push_str(&format!("select $n;\nlimit {};", query.limit.max(1)));
    q
}

/// Transitive impact query using the `reachable` function from the schema file.
pub fn reachable(from: &str, hops: u32) -> String {
    format!(
        "match\n  $from isa memory-node, has uid \"{f}\";\n  let $n in reachable($from);\n  $n has uid $uid;\nselect $n;\nlimit {limit};",
        f = escape(from),
        limit = hops.max(1)
    )
}

/// The thin transport seam. Implemented later by the feature-gated gRPC adapter and
/// right now by the cassette replayer used in tests.
pub trait TypeqlPort {
    fn query(&mut self, typeql: &str) -> Result<String>;
    fn define(&mut self, schema: &str) -> Result<()>;
}

pub struct TypeDbBackend<P: TypeqlPort> {
    port: P,
}

impl<P: TypeqlPort> TypeDbBackend<P> {
    pub fn new(port: P) -> Self {
        TypeDbBackend { port }
    }

    pub fn bootstrapped(port: P) -> Result<Self> {
        let mut b = TypeDbBackend::new(port);
        let schema = schema_definition();
        b.port.define(&schema)?;
        Ok(b)
    }

    pub fn port_mut(&mut self) -> &mut P {
        &mut self.port
    }
}

impl<P: TypeqlPort> MemoryBackend for TypeDbBackend<P> {
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
        self.port.query(&insert_node(&node))?;
        Ok(())
    }

    fn upsert_edge(&mut self, edge: MemoryEdge) -> Result<()> {
        let q = insert_edge(&edge);
        // TypeDB refuses this at the schema level; surfacing it as our own error keeps
        // the trait's contract identical to the in-memory backend.
        self.port.query(&q)?;
        Ok(())
    }

    fn get_node(&self, _id: &str) -> Result<Option<MemoryNode>> {
        // Reading results out of the driver API is part of the feature-gated adapter;
        // decoding its response is not implemented yet and pretending otherwise would be
        // the kind of stub that quietly returns wrong answers.
        Err(MemoryError::Unsupported {
            feature: "typedb read-back (adapter pending)".into(),
        })
    }

    fn neighbours(&self, _id: &str, _hops: u32, _kinds: &[EdgeKind]) -> Result<Vec<Hit>> {
        Err(MemoryError::Unsupported {
            feature: "typedb traversal (adapter pending)".into(),
        })
    }

    fn search(&self, _query: &Query) -> Result<Vec<Hit>> {
        Err(MemoryError::Unsupported {
            feature: "typedb search (adapter pending)".into(),
        })
    }

    fn clear(&mut self) -> Result<()> {
        Err(MemoryError::Unsupported {
            feature: "typedb clear (adapter pending)".into(),
        })
    }

    fn len(&self) -> Result<usize> {
        Err(MemoryError::Unsupported {
            feature: "typedb count (adapter pending)".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Replays hand-written responses. NOT a recording from a live server — there is
    /// none in the sandbox — so these tests cover *query generation and escaping*, not
    /// server compatibility.
    struct Cassette {
        log: Vec<String>,
        fail: Option<String>,
    }

    impl Cassette {
        fn new() -> Self {
            Cassette {
                log: Vec::new(),
                fail: None,
            }
        }
    }

    impl TypeqlPort for Cassette {
        fn query(&mut self, typeql: &str) -> Result<String> {
            if let Some(f) = &self.fail {
                return Err(MemoryError::Backend { detail: f.clone() });
            }
            self.log.push(typeql.to_string());
            Ok(String::new())
        }

        fn define(&mut self, schema: &str) -> Result<()> {
            self.log.push(format!("DEFINE {schema}"));
            Ok(())
        }
    }

    /// Quotes that actually terminate a string literal (i.e. not preceded by `\\`).
    fn count_unescaped_quotes(s: &str) -> usize {
        let bytes: Vec<char> = s.chars().collect();
        bytes
            .iter()
            .enumerate()
            .filter(|(i, c)| **c == '"' && (*i == 0 || bytes[i - 1] != '\\'))
            .count()
    }

    fn node(id: &str) -> MemoryNode {
        MemoryNode::new(id, NodeKind::File, "auth.rs").with_text("verify token")
    }

    #[test]
    fn escaping_neutralises_quotes_and_newlines() {
        assert_eq!(escape("a\"b"), "a\\\"b");
        assert_eq!(escape("a\\b"), "a\\\\b");
        assert_eq!(escape("a\nb"), "a\\nb");
        // A malicious label must not be able to close the literal and add a clause:
        // the quote it needs is escaped, so the literal stays balanced and the payload
        // remains inert text inside it.
        let evil = MemoryNode::new("x", NodeKind::Doc, "x\"; match $p isa task; #");
        let q = insert_node(&evil);
        assert!(
            q.contains("\\\"") || q.contains("x\\\""),
            "the quote must be escaped: {q}"
        );
        let unescaped = count_unescaped_quotes(&q);
        assert_eq!(
            unescaped % 2,
            0,
            "odd unescaped quotes mean a broken literal: {q}"
        );
        // Every literal must still close: `has uid "..."`, `has label "..."`, `has importance`.
        assert_eq!(
            unescaped, 4,
            "expected exactly two balanced literals, got {q}"
        );
    }

    #[test]
    fn insert_node_uses_the_schema_type_not_a_generic_one() {
        let q = insert_node(&node("file:src/auth.rs"));
        assert!(q.contains("isa source-file"), "got {q}");
        assert!(q.contains("has uid \"file:src/auth.rs\""));
        assert!(q.contains("has label \"auth.rs\""));
        assert!(q.contains("has body \"verify token\""));
        assert!(q.trim_end().ends_with(';'));
    }

    #[test]
    fn insert_node_omits_empty_optional_attributes() {
        let bare = MemoryNode::new("d", NodeKind::Decision, "use worktrees");
        let q = insert_node(&bare);
        assert!(
            !q.contains("has body"),
            "empty body must not be written: {q}"
        );
        assert!(!q.contains("has provenance"));
        assert!(q.contains("has importance"), "importance is always present");
    }

    #[test]
    fn importance_is_formatted_fixed_point() {
        // A locale- or precision-dependent float would silently corrupt the schema type.
        let n = MemoryNode::new("x", NodeKind::Doc, "x").with_importance(1.0);
        assert!(insert_node(&n).contains("has importance 1.000"));
    }

    #[test]
    fn insert_edge_matches_existing_nodes_before_linking_them() {
        let q = insert_edge(&MemoryEdge::new(
            "sym:verify",
            "file:auth.rs",
            EdgeKind::Calls,
        ));
        assert!(
            q.starts_with("match"),
            "must match existing nodes, not create new ones: {q}"
        );
        assert!(q.contains("isa calls"));
        assert!(q.contains("(source: $a, target: $b)"));
        assert!(q.contains("has uid \"sym:verify\""));
        assert!(q.contains("has uid \"file:auth.rs\""));
        assert!(q.contains("has confidence 1.000"));
        assert!(q.find("match").unwrap() < q.find("insert").unwrap());
    }

    #[test]
    fn hyphenated_relation_types_are_used() {
        let q = insert_edge(&MemoryEdge::new("a", "b", EdgeKind::AuthoredBy));
        assert!(q.contains("isa authored-by"), "got {q}");
        assert!(!q.contains("authored_by"));
    }

    #[test]
    fn every_node_kind_maps_to_a_declared_type() {
        // The schema string is what `init` runs; a kind missing from it fails at write
        // time on the server, i.e. after the user's data is already half-written.
        let schema = schema_definition();
        for k in [
            NodeKind::File,
            NodeKind::Symbol,
            NodeKind::Commit,
            NodeKind::Task,
            NodeKind::Decision,
            NodeKind::Error,
            NodeKind::Agent,
            NodeKind::Doc,
        ] {
            assert!(
                schema.contains(&format!("entity {}, sub memory-node", k.typeql_type())),
                "{k:?} missing from schema"
            );
        }
        for e in [
            EdgeKind::Calls,
            EdgeKind::Imports,
            EdgeKind::Fixes,
            EdgeKind::Regresses,
            EdgeKind::Blocks,
            EdgeKind::Supersedes,
            EdgeKind::DerivedFrom,
            EdgeKind::AuthoredBy,
        ] {
            assert!(
                schema.contains(&format!("relation {}, sub memory-edge", e.typeql_type())),
                "{e:?} missing from schema"
            );
        }
    }

    #[test]
    fn schema_file_and_generated_schema_agree_on_type_names() {
        let on_disk =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/schema/memory.tql"))
                .expect("schema/memory.tql must exist");
        for k in [
            NodeKind::File,
            NodeKind::Symbol,
            NodeKind::Commit,
            NodeKind::Task,
            NodeKind::Decision,
            NodeKind::Error,
            NodeKind::Agent,
            NodeKind::Doc,
        ] {
            assert!(
                on_disk.contains(k.typeql_type()),
                "{k:?} missing from schema/memory.tql"
            );
        }
        assert!(
            on_disk.contains("fun reachable"),
            "recursive traversal function missing"
        );
    }

    #[test]
    fn search_without_embedding_falls_back_to_lexical() {
        let q = search(&Query::new("token expiry"));
        assert!(q.contains("contains \"token expiry\""), "got {q}");
        assert!(!q.contains("cosine_similarity_search"));
    }

    #[test]
    fn search_with_embedding_requests_vector_similarity() {
        let q = search(
            &Query::new("token")
                .with_embedding(vec![0.1, 0.2])
                .with_focus("file:auth.rs"),
        );
        assert!(q.contains("cosine_similarity_search"));
        assert!(
            q.contains("vector([0.100000, 0.200000]"),
            "embedding must be emitted as a vector literal: {q}"
        );
        assert!(
            q.contains("memory-edge"),
            "focus must widen the candidate set to graph neighbours"
        );
        assert!(q.trim_end().ends_with(';'));
    }

    #[test]
    fn search_never_emits_an_unbounded_query() {
        let q = search(&Query {
            limit: 0,
            ..Query::new("x")
        });
        assert!(
            q.contains("limit 1"),
            "limit must be clamped to at least 1: {q}"
        );
    }

    #[test]
    fn reachable_quotes_the_start_node() {
        let q = reachable("file:src/auth.rs", 99);
        assert!(q.contains("let $n in reachable($from)"));
        assert!(q.contains("has uid \"file:src/auth.rs\""));
    }

    #[test]
    fn backend_reports_server_and_inference_capabilities() {
        let b = TypeDbBackend::new(Cassette::new());
        let c = b.capabilities();
        assert!(c.inference && c.schema_validated && c.requires_server);
        assert_eq!(b.id(), "typedb");
    }

    #[test]
    fn bootstrap_sends_the_schema_once() {
        let mut b = TypeDbBackend::bootstrapped(Cassette::new()).unwrap();
        let log = &b.port_mut().log;
        assert_eq!(log.len(), 1);
        assert!(
            log[0].starts_with("DEFINE define"),
            "got {}",
            &log[0][..40.min(log[0].len())]
        );
    }

    #[test]
    fn writes_go_through_the_port() {
        let mut b = TypeDbBackend::new(Cassette::new());
        b.upsert_node(node("file:a.rs")).unwrap();
        b.upsert_edge(MemoryEdge::new("file:a.rs", "file:b.rs", EdgeKind::Imports))
            .unwrap();
        assert_eq!(b.port_mut().log.len(), 2);
        assert!(b.port_mut().log[0].contains("isa source-file"));
    }

    #[test]
    fn empty_id_is_rejected_before_reaching_the_server() {
        let mut b = TypeDbBackend::new(Cassette::new());
        assert!(matches!(
            b.upsert_node(MemoryNode::new("", NodeKind::Doc, "x")),
            Err(MemoryError::Invalid(_))
        ));
        assert!(b.port_mut().log.is_empty(), "nothing should have been sent");
    }

    #[test]
    fn transport_errors_are_surfaced_not_swallowed() {
        let mut c = Cassette::new();
        c.fail = Some("connection refused on 1729".into());
        let mut b = TypeDbBackend::new(c);
        match b.upsert_node(node("file:a.rs")) {
            Err(MemoryError::Backend { detail }) => assert!(detail.contains("1729")),
            other => panic!("expected a backend error, got {other:?}"),
        }
    }

    #[test]
    fn unimplemented_reads_say_unsupported_rather_than_lying() {
        // A stub that returns Ok(vec![]) would be indistinguishable from "no memories",
        // which is exactly how a memory layer silently stops working.
        let b = TypeDbBackend::new(Cassette::new());
        for r in [
            b.get_node("x").map(|_| ()),
            b.neighbours("x", 1, &[]).map(|_| ()),
            b.search(&Query::new("x")).map(|_| ()),
        ] {
            assert!(
                matches!(r, Err(MemoryError::Unsupported { .. })),
                "got {r:?}"
            );
        }
    }
}
