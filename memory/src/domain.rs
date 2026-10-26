//! Domain model, backend-agnostic.
//!
//! This is the contract every backend speaks. TypeQL types, SQLite tables and the
//! in-memory test double all map to and from these, so retrieval logic and the TUI
//! never depend on which store is underneath.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeKind {
    File,
    Symbol,
    Commit,
    Task,
    Decision,
    Error,
    Agent,
    Doc,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::File => "file",
            NodeKind::Symbol => "symbol",
            NodeKind::Commit => "commit",
            NodeKind::Task => "task",
            NodeKind::Decision => "decision",
            NodeKind::Error => "error",
            NodeKind::Agent => "agent",
            NodeKind::Doc => "doc",
        }
    }

    /// TypeQL type name. Must match `schema/memory.tql`.
    pub fn typeql_type(self) -> &'static str {
        match self {
            NodeKind::File => "source-file",
            NodeKind::Symbol => "code-symbol",
            NodeKind::Commit => "commit",
            NodeKind::Task => "task",
            NodeKind::Decision => "decision",
            NodeKind::Error => "failure",
            NodeKind::Agent => "agent",
            NodeKind::Doc => "document",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "file" | "source-file" => NodeKind::File,
            "symbol" | "code-symbol" => NodeKind::Symbol,
            "commit" => NodeKind::Commit,
            "task" => NodeKind::Task,
            "decision" => NodeKind::Decision,
            "error" | "failure" => NodeKind::Error,
            "agent" => NodeKind::Agent,
            "doc" | "document" => NodeKind::Doc,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeKind {
    Calls,
    Imports,
    Fixes,
    Regresses,
    Blocks,
    Supersedes,
    DerivedFrom,
    AuthoredBy,
}

impl EdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Calls => "calls",
            EdgeKind::Imports => "imports",
            EdgeKind::Fixes => "fixes",
            EdgeKind::Regresses => "regresses",
            EdgeKind::Blocks => "blocks",
            EdgeKind::Supersedes => "supersedes",
            EdgeKind::DerivedFrom => "derived_from",
            EdgeKind::AuthoredBy => "authored_by",
        }
    }

    /// TypeQL relation type. Must match `schema/memory.tql`.
    pub fn typeql_type(self) -> &'static str {
        match self {
            EdgeKind::Calls => "calls",
            EdgeKind::Imports => "imports",
            EdgeKind::Fixes => "fixes",
            EdgeKind::Regresses => "regresses",
            EdgeKind::Blocks => "blocks",
            EdgeKind::Supersedes => "supersedes",
            EdgeKind::DerivedFrom => "derived-from",
            EdgeKind::AuthoredBy => "authored-by",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "calls" => EdgeKind::Calls,
            "imports" => EdgeKind::Imports,
            "fixes" => EdgeKind::Fixes,
            "regresses" => EdgeKind::Regresses,
            "blocks" => EdgeKind::Blocks,
            "supersedes" => EdgeKind::Supersedes,
            "derived_from" | "derived-from" => EdgeKind::DerivedFrom,
            "authored_by" | "authored-by" => EdgeKind::AuthoredBy,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryNode {
    /// Stable, backend-independent id, e.g. `file:src/auth.rs` or `commit:4f2c1ab`.
    pub id: String,
    pub kind: NodeKind,
    /// Human label shown in the TUI.
    pub label: String,
    /// The text that was embedded, kept so the store can re-embed on model change.
    pub text: String,
    /// Optional embedding. `None` means "graph node only, not semantically searchable".
    pub embedding: Option<Vec<f32>>,
    /// Provenance: file:line, commit sha, thread id — whatever lets a human check it.
    pub provenance: Option<String>,
    /// Unix seconds; retrieval uses it for recency.
    pub created_at: i64,
    /// 0..=1 hand-assigned importance, e.g. a decision outranks a scratch note.
    pub importance: f32,
}

impl MemoryNode {
    pub fn new(id: impl Into<String>, kind: NodeKind, label: impl Into<String>) -> Self {
        MemoryNode {
            id: id.into(),
            kind,
            label: label.into(),
            text: String::new(),
            embedding: None,
            provenance: None,
            created_at: 0,
            importance: 0.5,
        }
    }

    pub fn with_text(mut self, t: impl Into<String>) -> Self {
        self.text = t.into();
        self
    }

    pub fn with_embedding(mut self, e: Vec<f32>) -> Self {
        self.embedding = Some(e);
        self
    }

    pub fn with_provenance(mut self, p: impl Into<String>) -> Self {
        self.provenance = Some(p.into());
        self
    }

    pub fn with_importance(mut self, i: f32) -> Self {
        self.importance = i.clamp(0.0, 1.0);
        self
    }

    pub fn with_created_at(mut self, t: i64) -> Self {
        self.created_at = t;
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryEdge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    /// Optional confidence for inferred edges (0..=1).
    pub weight: f32,
}

impl MemoryEdge {
    pub fn new(from: impl Into<String>, to: impl Into<String>, kind: EdgeKind) -> Self {
        MemoryEdge {
            from: from.into(),
            to: to.into(),
            kind,
            weight: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Exact match on identity or text.
    Lexical,
    /// Cosine similarity over embeddings.
    Semantic,
    /// Reached by traversing the graph from the query focus.
    Graph,
    /// Matched by a declared inference rule/function.
    Inferred,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub node: MemoryNode,
    pub score: f32,
    /// Per-signal breakdown, so a bad retrieval can be debugged instead of guessed at.
    pub signals: Signals,
    /// How the node was reached (empty for a direct hit).
    pub via: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Signals {
    pub cosine: f32,
    pub bm25: f32,
    pub graph: f32,
    pub recency: f32,
    pub importance: f32,
    pub inferred: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MemoryError {
    NotFound(String),
    Backend {
        detail: String,
    },
    /// The backend cannot express the requested operation — e.g. no vector index.
    Unsupported {
        feature: String,
    },
    Invalid(String),
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryError::NotFound(id) => write!(f, "not found: {id}"),
            MemoryError::Backend { detail } => write!(f, "backend error: {detail}"),
            MemoryError::Unsupported { feature } => write!(f, "backend does not support {feature}"),
            MemoryError::Invalid(d) => write!(f, "invalid input: {d}"),
        }
    }
}

impl std::error::Error for MemoryError {}

pub type Result<T> = std::result::Result<T, MemoryError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typeql_type_names_round_trip() {
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
            assert_eq!(
                NodeKind::parse(k.typeql_type()),
                Some(k),
                "typeql name for {k:?}"
            );
            assert_eq!(NodeKind::parse(k.as_str()), Some(k), "short name for {k:?}");
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
            assert_eq!(
                EdgeKind::parse(e.typeql_type()),
                Some(e),
                "typeql name for {e:?}"
            );
        }
    }

    #[test]
    fn typeql_type_names_are_valid_labels() {
        // TypeQL labels are lower-case words joined by hyphens; a space or underscore
        // would produce a schema we cannot define.
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
            let t = k.typeql_type();
            assert!(!t.contains(' ') && !t.contains('_'), "bad label {t}");
            assert!(
                t.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "bad label {t}"
            );
        }
        for e in [EdgeKind::DerivedFrom, EdgeKind::AuthoredBy] {
            let t = e.typeql_type();
            assert!(!t.contains('_'), "TypeQL wants hyphens, got {t}");
        }
    }

    #[test]
    fn importance_is_clamped() {
        let n = MemoryNode::new("x", NodeKind::Doc, "x").with_importance(5.0);
        assert_eq!(n.importance, 1.0);
        let n = MemoryNode::new("x", NodeKind::Doc, "x").with_importance(-2.0);
        assert_eq!(n.importance, 0.0);
    }
}
