//! The backend seam.
//!
//! Two very different engines have to be interchangeable here:
//!   * **SQLite** — one file, in-process, zero ops. The default for the single-user CLI.
//!   * **TypeDB** — typed hypergraph with schema validation and inference functions.
//!     A server (gRPC, port 1729) in Community Edition; genuinely better at
//!     "which decisions transitively affect this file" questions.
//!
//! The trait is deliberately synchronous and small. The TypeDB driver is async and
//! gRPC-based; an adapter holds a runtime handle and blocks on the executor. Keeping
//! this trait sync is what lets the in-memory double and SQLite be tested with no
//! runtime at all, which is most of the test suite.

use crate::domain::{EdgeKind, Hit, MemoryEdge, MemoryError, MemoryNode, Result};

/// Query shape for retrieval.
#[derive(Debug, Clone)]
pub struct Query {
    /// Natural-language or code text; embedded by the caller if the backend supports it.
    pub text: String,
    /// Query embedding, when the caller already computed one.
    pub embedding: Option<Vec<f32>>,
    /// Restrict to these kinds (empty = all).
    pub kinds: Vec<crate::domain::NodeKind>,
    /// Node to bias results toward, i.e. the file the agent is working on.
    pub focus: Option<String>,
    /// How many hops of graph neighbourhood to consider.
    pub hops: u32,
    pub limit: usize,
}

impl Query {
    pub fn new(text: impl Into<String>) -> Self {
        Query {
            text: text.into(),
            embedding: None,
            kinds: Vec::new(),
            focus: None,
            hops: 2,
            limit: 10,
        }
    }

    pub fn with_focus(mut self, id: impl Into<String>) -> Self {
        self.focus = Some(id.into());
        self
    }

    pub fn with_embedding(mut self, e: Vec<f32>) -> Self {
        self.embedding = Some(e);
        self
    }
}

/// What a backend can actually do. The router and the TUI must never assume a
/// capability; they ask. This is how the same code runs on SQLite (no inference) and
/// TypeDB (inference available) without branching on type names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub vector_search: bool,
    pub lexical_search: bool,
    pub graph_traversal: bool,
    /// Declared inference (TypeDB functions). SQLite emulates with recursive CTEs or not at all.
    pub inference: bool,
    /// True when the backend is a separate process that must be reachable.
    pub requires_server: bool,
    /// Schema is validated on write (TypeDB) rather than by the application.
    pub schema_validated: bool,
}

pub trait MemoryBackend {
    /// Stable backend id: `sqlite`, `typedb`, `memory` (test double).
    fn id(&self) -> &'static str;

    fn capabilities(&self) -> Capabilities;

    fn upsert_node(&mut self, node: MemoryNode) -> Result<()>;

    fn upsert_edge(&mut self, edge: MemoryEdge) -> Result<()>;

    fn get_node(&self, id: &str) -> Result<Option<MemoryNode>>;

    /// Neighbours within `hops`, with the edge kind used to reach each one.
    /// `via` in the returned hits carries the path for explanation in the TUI.
    fn neighbours(&self, id: &str, hops: u32, kinds: &[EdgeKind]) -> Result<Vec<Hit>>;

    /// Hybrid retrieval. Implementations score what they can and leave other signals at 0.
    fn search(&self, query: &Query) -> Result<Vec<Hit>>;

    /// Delete one node and any edge touching it.
    ///
    /// Defaults to `Unsupported` so a new backend cannot silently keep a node the user
    /// asked to forget.
    fn forget(&mut self, id: &str) -> Result<()> {
        Err(MemoryError::Unsupported {
            feature: format!("forgetting `{id}` on this backend"),
        })
    }

    /// Drop everything. Used by tests and by `zenithal memory reset`.
    fn clear(&mut self) -> Result<()>;

    fn len(&self) -> Result<usize>;

    fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_defaults_are_sane() {
        let q = Query::new("where is auth handled");
        assert_eq!(q.limit, 10);
        assert_eq!(q.hops, 2);
        assert!(q.focus.is_none());
    }

    #[test]
    fn capabilities_differ_between_intended_backends() {
        // Documents the design intent; concrete backends assert these in their own tests.
        let sqlite = Capabilities {
            vector_search: true,
            lexical_search: true,
            graph_traversal: true,
            inference: false,
            requires_server: false,
            schema_validated: false,
        };
        let typedb = Capabilities {
            vector_search: true, // alpha upstream; see TYPEDB.md
            lexical_search: true,
            graph_traversal: true,
            inference: true,
            requires_server: true,
            schema_validated: true,
        };
        assert!(!sqlite.requires_server);
        assert!(typedb.inference && typedb.schema_validated);
    }
}
