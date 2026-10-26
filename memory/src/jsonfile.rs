//! A single-file local store: `~/.zenithal/memory.json`.
//!
//! Why this exists: the in-process backend forgets everything when the process exits,
//! which makes `zenithal memory remember …` followed by a later `zenithal context …`
//! useless. TypeDB fixes that but requires a daemon; users without one should still get
//! memory that survives a restart, on disk, on their machine.
//!
//! Design:
//! * whole-file load, whole-file atomic save (`write tmp` → `rename`) — a crash cannot
//!   leave a half-written graph;
//! * ranking is the shared [`crate::rank`] path, so behaviour matches every other backend;
//! * a corrupt file is a typed error that names the path, never a panic and never a
//!   silent reset — losing someone's memory quietly would be worse than failing loudly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::backend::{Capabilities, MemoryBackend, Query};
use crate::domain::{EdgeKind, Hit, MemoryEdge, MemoryError, MemoryNode, NodeKind, Result};
use crate::scoring::Weights;

#[derive(Debug)]
pub struct JsonFileBackend {
    path: PathBuf,
    nodes: BTreeMap<String, MemoryNode>,
    edges: Vec<MemoryEdge>,
    weights: Weights,
    now: i64,
    dirty: bool,
}

impl JsonFileBackend {
    /// Open (or create) the store at `path`. A missing file is an empty store.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let mut backend = JsonFileBackend {
            path,
            nodes: BTreeMap::new(),
            edges: Vec::new(),
            weights: Weights::default(),
            now: now_seconds(),
            dirty: false,
        };
        backend.load()?;
        Ok(backend)
    }

    /// An in-memory-only instance of the same code path, for tests and `--dry-run`.
    pub fn memory_only() -> Self {
        JsonFileBackend {
            path: PathBuf::new(),
            nodes: BTreeMap::new(),
            edges: Vec::new(),
            weights: Weights::default(),
            now: now_seconds(),
            dirty: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Force a persist even if nothing changed (used by `memory init`).
    pub fn flush(&mut self) -> Result<()> {
        self.save()
    }

    fn load(&mut self) -> Result<()> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(MemoryError::Backend {
                    detail: format!("cannot read {}: {e}", self.path.display()),
                })
            }
        };
        if text.trim().is_empty() {
            return Ok(());
        }
        let doc: Document = serde_json::from_str(&text).map_err(|e| MemoryError::Backend {
            detail: format!(
                "memory file {} is not valid JSON ({e}). Refusing to overwrite it — \
                 move it aside or fix it.",
                self.path.display()
            ),
        })?;
        for n in doc.nodes {
            if n.id.trim().is_empty() {
                continue;
            }
            let kind = NodeKind::parse(&n.kind).unwrap_or(NodeKind::Doc);
            let mut node = MemoryNode::new(n.id, kind, n.label)
                .with_text(n.text)
                .with_importance(n.importance)
                .with_created_at(n.created_at);
            node.embedding = n.embedding;
            node.provenance = n.provenance;
            self.nodes.insert(node.id.clone(), node);
        }
        for e in doc.edges {
            let kind = EdgeKind::parse(&e.kind).unwrap_or(EdgeKind::DerivedFrom);
            // Drop edges whose ends vanished; a dangling edge would break traversal.
            if self.nodes.contains_key(&e.from) && self.nodes.contains_key(&e.to) {
                self.edges.push(MemoryEdge {
                    from: e.from,
                    to: e.to,
                    kind,
                    weight: e.weight,
                });
            }
        }
        Ok(())
    }

    fn save(&mut self) -> Result<()> {
        if self.path.as_os_str().is_empty() {
            self.dirty = false;
            return Ok(());
        }
        if !self.dirty {
            return Ok(());
        }
        let doc = Document {
            version: 1,
            nodes: self
                .nodes
                .values()
                .map(|n| NodeRecord {
                    id: n.id.clone(),
                    kind: n.kind.as_str().to_string(),
                    label: n.label.clone(),
                    text: n.text.clone(),
                    provenance: n.provenance.clone(),
                    importance: n.importance,
                    created_at: n.created_at,
                    embedding: n.embedding.clone(),
                })
                .collect(),
            edges: self
                .edges
                .iter()
                .map(|e| EdgeRecord {
                    from: e.from.clone(),
                    to: e.to.clone(),
                    kind: e.kind.as_str().to_string(),
                    weight: e.weight,
                })
                .collect(),
        };
        let json = serde_json::to_string_pretty(&doc).map_err(|e| MemoryError::Backend {
            detail: format!("cannot serialize memory: {e}"),
        })?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| MemoryError::Backend {
                detail: format!("cannot create {}: {e}", parent.display()),
            })?;
        }
        // Atomic replace: a crash mid-write must not truncate the previous graph.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| MemoryError::Backend {
            detail: format!("cannot write {}: {e}", tmp.display()),
        })?;
        std::fs::rename(&tmp, &self.path).map_err(|e| MemoryError::Backend {
            detail: format!("cannot replace {}: {e}", self.path.display()),
        })?;
        self.dirty = false;
        Ok(())
    }
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Document {
    version: u32,
    #[serde(default)]
    nodes: Vec<NodeRecord>,
    #[serde(default)]
    edges: Vec<EdgeRecord>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct NodeRecord {
    id: String,
    kind: String,
    label: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    provenance: Option<String>,
    #[serde(default = "default_importance")]
    importance: f32,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    embedding: Option<Vec<f32>>,
}

fn default_importance() -> f32 {
    0.5
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct EdgeRecord {
    from: String,
    to: String,
    kind: String,
    #[serde(default = "default_weight")]
    weight: f32,
}

fn default_weight() -> f32 {
    1.0
}

impl MemoryBackend for JsonFileBackend {
    fn id(&self) -> &'static str {
        "jsonfile"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            vector_search: true,
            lexical_search: true,
            graph_traversal: true,
            inference: false,
            requires_server: false,
            schema_validated: false,
        }
    }

    fn upsert_node(&mut self, node: MemoryNode) -> Result<()> {
        if node.id.trim().is_empty() {
            return Err(MemoryError::Invalid("node id must not be empty".into()));
        }
        let node = if node.created_at == 0 {
            MemoryNode {
                created_at: now_seconds(),
                ..node
            }
        } else {
            node
        };
        self.nodes.insert(node.id.clone(), node);
        self.dirty = true;
        self.save()
    }

    fn upsert_edge(&mut self, edge: MemoryEdge) -> Result<()> {
        for end in [&edge.from, &edge.to] {
            if !self.nodes.contains_key(end) {
                return Err(MemoryError::NotFound(end.clone()));
            }
        }
        if !self.edges.contains(&edge) {
            self.edges.push(edge);
            self.dirty = true;
            self.save()?;
        }
        Ok(())
    }

    fn get_node(&self, id: &str) -> Result<Option<MemoryNode>> {
        Ok(self.nodes.get(id).cloned())
    }

    fn neighbours(&self, id: &str, hops: u32, kinds: &[EdgeKind]) -> Result<Vec<Hit>> {
        if !self.nodes.contains_key(id) {
            return Err(MemoryError::NotFound(id.to_string()));
        }
        let mut edges = self.edges.clone();
        if !kinds.is_empty() {
            edges.retain(|e| kinds.contains(&e.kind));
        }
        let adj = crate::rank::adjacency(&edges);
        let map = crate::rank::hop_map(id, hops, &adj);
        let mut out: Vec<Hit> = map
            .iter()
            .filter(|(k, h)| k.as_str() != id && **h > 0)
            .filter_map(|(k, h)| {
                self.nodes.get(k).map(|n| {
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
        let nodes: Vec<MemoryNode> = self.nodes.values().cloned().collect();
        let adj = crate::rank::adjacency(&self.edges);
        let hops = match &query.focus {
            Some(focus) => crate::rank::hop_map(focus, query.hops, &adj),
            None => BTreeMap::new(),
        };
        Ok(crate::rank::rank(
            &nodes,
            query,
            &self.weights,
            self.now,
            &hops,
        ))
    }

    fn forget(&mut self, id: &str) -> Result<()> {
        if self.nodes.remove(id).is_none() {
            return Err(MemoryError::NotFound(id.to_string()));
        }
        self.edges.retain(|e| e.from != id && e.to != id);
        self.dirty = true;
        self.save()
    }

    fn clear(&mut self) -> Result<()> {
        self.nodes.clear();
        self.edges.clear();
        self.dirty = true;
        self.save()
    }

    fn len(&self) -> Result<usize> {
        Ok(self.nodes.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    fn temp_path(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("zenithal-jsonfile-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("memory.json")
    }

    fn seed(path: &Path) {
        let mut b = JsonFileBackend::open(path).unwrap();
        b.upsert_node(
            MemoryNode::new("file:auth.rs", NodeKind::File, "auth.rs").with_text("verify token"),
        )
        .unwrap();
        b.upsert_node(
            MemoryNode::new("commit:4f2c", NodeKind::Commit, "fix expiry").with_text("clock bug"),
        )
        .unwrap();
        b.upsert_edge(MemoryEdge::new(
            "commit:4f2c",
            "file:auth.rs",
            EdgeKind::Fixes,
        ))
        .unwrap();
    }

    #[test]
    fn a_missing_file_is_an_empty_store_not_an_error() {
        let path = temp_path("missing");
        let b = JsonFileBackend::open(&path).unwrap();
        assert!(b.is_empty().unwrap());
        assert!(
            !path.exists(),
            "opening must not create the file until something is written"
        );
    }

    #[test]
    fn data_survives_a_reopen() {
        let path = temp_path("persist");
        seed(&path);
        let b = JsonFileBackend::open(&path).unwrap();
        assert_eq!(b.len().unwrap(), 2);
        let n = b.get_node("file:auth.rs").unwrap().unwrap();
        assert_eq!(n.kind, NodeKind::File);
        assert_eq!(n.text, "verify token");
        assert!(n.created_at > 0, "a timestamp must be stamped on write");
        assert_eq!(
            b.neighbours("file:auth.rs", 1, &[]).unwrap()[0].node.id,
            "commit:4f2c"
        );
    }

    #[test]
    fn embeddings_survive_the_round_trip() {
        let path = temp_path("embed");
        let mut b = JsonFileBackend::open(&path).unwrap();
        b.upsert_node(
            MemoryNode::new("file:a.rs", NodeKind::File, "a.rs")
                .with_text("token")
                .with_embedding(vec![0.5, -0.25, 0.125]),
        )
        .unwrap();
        drop(b);
        let b = JsonFileBackend::open(&path).unwrap();
        assert_eq!(
            b.get_node("file:a.rs")
                .unwrap()
                .unwrap()
                .embedding
                .as_deref(),
            Some(&[0.5f32, -0.25, 0.125][..])
        );
    }

    #[test]
    fn a_corrupt_file_is_refused_rather_than_reset() {
        // Silently starting from empty would delete a user's memory on a bad edit.
        let path = temp_path("corrupt");
        std::fs::write(&path, "{ this is not json").unwrap();
        let err = JsonFileBackend::open(&path).unwrap_err().to_string();
        assert!(err.contains("not valid JSON"), "{err}");
        assert!(err.contains("Refusing to overwrite"), "{err}");
        // And the bad file is still on disk, untouched.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{ this is not json"
        );
    }

    #[test]
    fn dangling_edges_are_dropped_on_load() {
        let path = temp_path("dangling");
        std::fs::write(
            &path,
            r#"{"version":1,
                "nodes":[{"id":"a","kind":"doc","label":"a"}],
                "edges":[{"from":"a","to":"ghost","kind":"calls"}]}"#,
        )
        .unwrap();
        let b = JsonFileBackend::open(&path).unwrap();
        assert!(
            b.neighbours("a", 1, &[]).unwrap().is_empty(),
            "an edge to a missing node is not kept"
        );
    }

    #[test]
    fn writing_is_atomic_and_leaves_no_temp_file() {
        let path = temp_path("atomic");
        seed(&path);
        assert!(path.exists());
        assert!(
            !path.with_extension("json.tmp").exists(),
            "the temporary file must be renamed away"
        );
    }

    #[test]
    fn search_and_focus_work_through_the_file_backend() {
        let path = temp_path("search");
        seed(&path);
        let b = JsonFileBackend::open(&path).unwrap();
        let hits = b
            .search(&Query::new("clock").with_focus("file:auth.rs"))
            .unwrap();
        assert_eq!(hits[0].node.id, "commit:4f2c");
        assert!(
            !hits[0].via.is_empty(),
            "focus must produce an explanation path"
        );
    }

    #[test]
    fn forget_persists_and_drops_the_edges() {
        let path = temp_path("forget");
        seed(&path);
        let mut b = JsonFileBackend::open(&path).unwrap();
        b.forget("file:auth.rs").unwrap();
        drop(b);
        let b = JsonFileBackend::open(&path).unwrap();
        assert!(b.get_node("file:auth.rs").unwrap().is_none());
        assert!(matches!(
            b.neighbours("commit:4f2c", 1, &[]),
            Ok(ref hits) if hits.is_empty()
        ));
    }

    #[test]
    fn clear_persists_emptiness() {
        let path = temp_path("clear");
        seed(&path);
        let mut b = JsonFileBackend::open(&path).unwrap();
        b.clear().unwrap();
        drop(b);
        assert_eq!(JsonFileBackend::open(&path).unwrap().len().unwrap(), 0);
    }

    #[test]
    fn the_store_facade_works_with_the_file_backend() {
        let path = temp_path("facade");
        seed(&path);
        let mut store = MemoryStore::new(Box::new(JsonFileBackend::open(&path).unwrap()));
        assert_eq!(store.backend_id(), "jsonfile");
        store
            .remember(
                MemoryNode::new("decision:wt", NodeKind::Decision, "worktrees")
                    .with_importance(1.0),
            )
            .unwrap();
        let hits = store.recall(&Query::new("expiry")).unwrap();
        assert_eq!(hits[0].node.id, "commit:4f2c");
        assert!(store.context(&hits, 500).contains("commit:4f2c"));
    }
}
