//! In-memory backend: the reference implementation, the test double, and the
//! zero-dependency fallback when neither SQLite nor TypeDB is configured.
//!
//! Every other backend is tested *against* this one for behavioural parity, so the
//! retrieval semantics are defined once, in code, and are not allowed to drift.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::backend::{Capabilities, MemoryBackend, Query};
use crate::domain::{EdgeKind, Hit, MemoryEdge, MemoryError, MemoryNode, Origin, Result, Signals};
use crate::scoring::{self, Weights};

#[derive(Debug, Default)]
pub struct InMemoryBackend {
    nodes: BTreeMap<String, MemoryNode>,
    /// Adjacency kept in both directions; traversal is undirected because "what does
    /// this affect" and "what affects this" are both useful questions.
    edges: Vec<MemoryEdge>,
    now: i64,
    weights: Weights,
}

impl InMemoryBackend {
    pub fn new() -> Self {
        InMemoryBackend {
            nodes: BTreeMap::new(),
            edges: Vec::new(),
            now: 0,
            weights: Weights::default(),
        }
    }

    /// Fix "now" so recency scoring is deterministic in tests.
    pub fn with_now(mut self, now: i64) -> Self {
        self.now = now;
        self
    }

    pub fn with_weights(mut self, w: Weights) -> Self {
        self.weights = w;
        self
    }

    fn adjacency(&self) -> BTreeMap<&str, Vec<(&str, EdgeKind)>> {
        let mut adj: BTreeMap<&str, Vec<(&str, EdgeKind)>> = BTreeMap::new();
        for e in &self.edges {
            adj.entry(e.from.as_str())
                .or_default()
                .push((e.to.as_str(), e.kind));
            adj.entry(e.to.as_str())
                .or_default()
                .push((e.from.as_str(), e.kind));
        }
        adj
    }
}

impl MemoryBackend for InMemoryBackend {
    fn id(&self) -> &'static str {
        "memory"
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
        self.nodes.insert(node.id.clone(), node);
        Ok(())
    }

    fn upsert_edge(&mut self, edge: MemoryEdge) -> Result<()> {
        for end in [&edge.from, &edge.to] {
            if !self.nodes.contains_key(end) {
                return Err(MemoryError::NotFound(end.clone()));
            }
        }
        if !self.edges.contains(&edge) {
            self.edges.push(edge);
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
        let adj = self.adjacency();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        seen.insert(id.to_string());
        let mut queue: VecDeque<(String, u32, Vec<String>)> = VecDeque::new();
        queue.push_back((id.to_string(), 0, Vec::new()));
        let mut out = Vec::new();
        while let Some((cur, depth, path)) = queue.pop_front() {
            if depth >= hops {
                continue;
            }
            let Some(nbrs) = adj.get(cur.as_str()) else {
                continue;
            };
            for (next, kind) in nbrs {
                if !kinds.is_empty() && !kinds.contains(kind) {
                    continue;
                }
                if !seen.insert(next.to_string()) {
                    continue;
                }
                let mut p = path.clone();
                p.push(format!("{kind:?}"));
                if let Some(n) = self.nodes.get(*next) {
                    let proximity = scoring::graph_proximity(depth + 1);
                    out.push(Hit {
                        node: n.clone(),
                        score: proximity,
                        signals: Signals {
                            graph: proximity,
                            ..Default::default()
                        },
                        via: p.clone(),
                    });
                }
                queue.push_back((next.to_string(), depth + 1, p));
            }
        }
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.node.id.cmp(&b.node.id))
        });
        Ok(out)
    }

    fn search(&self, query: &Query) -> Result<Vec<Hit>> {
        // Ranking is shared with every other backend (`crate::rank`), so a result set
        // does not depend on which store produced the candidates.
        let adj = crate::rank::adjacency(&self.edges);
        let hops = match &query.focus {
            Some(focus) => crate::rank::hop_map(focus, query.hops, &adj),
            None => BTreeMap::new(),
        };
        let nodes: Vec<MemoryNode> = self.nodes.values().cloned().collect();
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
        // Edges to a deleted node would dangle, and a dangling edge breaks traversal.
        self.edges.retain(|e| e.from != id && e.to != id);
        Ok(())
    }

    fn clear(&mut self) -> Result<()> {
        self.nodes.clear();
        self.edges.clear();
        Ok(())
    }

    fn len(&self) -> Result<usize> {
        Ok(self.nodes.len())
    }
}

/// Origin is not stored on the node; useful for rendering "why" in the TUI.
pub fn origin_of(hit: &Hit) -> Origin {
    if !hit.via.is_empty() {
        Origin::Graph
    } else if hit.signals.cosine > 0.0 {
        Origin::Semantic
    } else if hit.signals.bm25 > 0.0 {
        Origin::Lexical
    } else {
        Origin::Inferred
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::NodeKind;

    fn seed() -> InMemoryBackend {
        let mut b = InMemoryBackend::new().with_now(1_000_000);
        b.upsert_node(
            MemoryNode::new("file:auth.rs", NodeKind::File, "auth.rs")
                .with_text("token verification session login")
                .with_embedding(vec![0.9, 0.1, 0.0]),
        )
        .unwrap();
        b.upsert_node(
            MemoryNode::new("sym:verify", NodeKind::Symbol, "verify_token")
                .with_text("verify token signature jwt")
                .with_embedding(vec![0.8, 0.2, 0.0]),
        )
        .unwrap();
        b.upsert_node(
            MemoryNode::new("commit:4f2c", NodeKind::Commit, "reject expired tokens")
                .with_text("fix token expiry check login")
                .with_embedding(vec![0.7, 0.1, 0.3]),
        )
        .unwrap();
        b.upsert_node(
            MemoryNode::new("file:render.rs", NodeKind::File, "render.rs")
                .with_text("draw the status line buffer"),
        )
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
        b
    }

    #[test]
    fn empty_backend_is_empty() {
        let b = InMemoryBackend::new();
        assert!(b.is_empty().unwrap());
        assert_eq!(b.len().unwrap(), 0);
    }

    #[test]
    fn upsert_is_idempotent_by_id() {
        let mut b = InMemoryBackend::new();
        b.upsert_node(MemoryNode::new("a", NodeKind::Doc, "one"))
            .unwrap();
        b.upsert_node(MemoryNode::new("a", NodeKind::Doc, "two"))
            .unwrap();
        assert_eq!(b.len().unwrap(), 1);
        assert_eq!(b.get_node("a").unwrap().unwrap().label, "two");
    }

    #[test]
    fn empty_node_id_is_rejected() {
        let mut b = InMemoryBackend::new();
        assert!(matches!(
            b.upsert_node(MemoryNode::new("  ", NodeKind::Doc, "x")),
            Err(MemoryError::Invalid(_))
        ));
    }

    #[test]
    fn edge_to_unknown_node_is_rejected() {
        // Dangling edges are the classic way a graph store rots; TypeDB refuses them at
        // the schema level, so the trait must refuse them too.
        let mut b = InMemoryBackend::new();
        b.upsert_node(MemoryNode::new("a", NodeKind::Doc, "a"))
            .unwrap();
        let err = b.upsert_edge(MemoryEdge::new("a", "ghost", EdgeKind::Calls));
        assert!(matches!(err, Err(MemoryError::NotFound(_))));
    }

    #[test]
    fn edges_are_not_duplicated() {
        let mut b = seed();
        let before = b.neighbours("file:auth.rs", 1, &[]).unwrap().len();
        b.upsert_edge(MemoryEdge::new(
            "sym:verify",
            "file:auth.rs",
            EdgeKind::Calls,
        ))
        .unwrap();
        assert_eq!(b.neighbours("file:auth.rs", 1, &[]).unwrap().len(), before);
    }

    #[test]
    fn neighbours_are_undirected_and_filtered_by_kind() {
        let b = seed();
        let all = b.neighbours("file:auth.rs", 1, &[]).unwrap();
        assert_eq!(all.len(), 2);
        let only_fixes = b.neighbours("file:auth.rs", 1, &[EdgeKind::Fixes]).unwrap();
        assert_eq!(only_fixes.len(), 1);
        assert_eq!(only_fixes[0].node.id, "commit:4f2c");
    }

    #[test]
    fn neighbours_respect_hop_limit() {
        let mut b = seed();
        b.upsert_node(MemoryNode::new("doc:far", NodeKind::Doc, "far away"))
            .unwrap();
        b.upsert_edge(MemoryEdge::new(
            "sym:verify",
            "doc:far",
            EdgeKind::DerivedFrom,
        ))
        .unwrap();
        let one = b.neighbours("file:auth.rs", 1, &[]).unwrap();
        let two = b.neighbours("file:auth.rs", 2, &[]).unwrap();
        assert!(!one.iter().any(|h| h.node.id == "doc:far"));
        assert!(two.iter().any(|h| h.node.id == "doc:far"));
    }

    #[test]
    fn unknown_focus_is_an_error_not_a_panic() {
        let b = seed();
        assert!(matches!(
            b.neighbours("nope", 2, &[]),
            Err(MemoryError::NotFound(_))
        ));
    }

    #[test]
    fn traversal_terminates_on_cycles() {
        let mut b = seed();
        b.upsert_edge(MemoryEdge::new(
            "file:auth.rs",
            "sym:verify",
            EdgeKind::Imports,
        ))
        .unwrap();
        let all = b.neighbours("file:auth.rs", 5, &[]).unwrap();
        // 2 neighbours + 1 far-free graph; must not loop forever or duplicate.
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn semantic_search_prefers_the_closest_embedding() {
        let b = seed();
        let q = Query::new("token").with_embedding(vec![0.95, 0.05, 0.0]);
        let hits = b.search(&q).unwrap();
        assert_eq!(hits[0].node.id, "file:auth.rs");
        assert!(hits[0].signals.cosine > hits[1].signals.cosine);
    }

    #[test]
    fn lexical_search_finds_unembedded_nodes() {
        // render.rs has no embedding; only BM25 can reach it.
        let b = seed();
        let hits = b.search(&Query::new("status line")).unwrap();
        assert_eq!(hits[0].node.id, "file:render.rs");
        assert_eq!(hits[0].signals.cosine, 0.0);
    }

    #[test]
    fn focus_biases_toward_graph_neighbours() {
        let b = seed();
        let plain = b
            .search(&Query::new("token").with_embedding(vec![0.95, 0.05, 0.0]))
            .unwrap();
        let focused = b
            .search(
                &Query::new("token")
                    .with_embedding(vec![0.95, 0.05, 0.0])
                    .with_focus("sym:verify"),
            )
            .unwrap();
        let score_of = |hs: &[Hit], id: &str| {
            hs.iter()
                .find(|h| h.node.id == id)
                .map(|h| h.score)
                .unwrap()
        };
        // Focus must add a graph signal to structural neighbours ...
        assert!(
            score_of(&focused, "file:auth.rs") > score_of(&plain, "file:auth.rs"),
            "focus should boost a neighbour: plain={:?}",
            plain
                .iter()
                .map(|h| (h.node.id.as_str(), h.score))
                .collect::<Vec<_>>()
        );
        // ... and the unconnected, semantically-unrelated `file:render.rs` must not be
        // in either result set at all (a graph neighbourhood is not a reason to surface
        // an unrelated node — that is what the relevance gate in `rank` is for).
        assert!(plain.iter().all(|h| h.node.id != "file:render.rs"));
        assert!(focused.iter().all(|h| h.node.id != "file:render.rs"));
        assert!(
            focused.iter().any(|h| !h.via.is_empty()),
            "focus must produce explanation paths"
        );
    }

    #[test]
    fn kind_filter_excludes_other_kinds() {
        let b = seed();
        let hits = b
            .search(&Query {
                kinds: vec![NodeKind::Commit],
                ..Query::new("token")
            })
            .unwrap();
        assert!(hits.iter().all(|h| h.node.kind == NodeKind::Commit));
        assert!(!hits.is_empty());
    }

    #[test]
    fn limit_is_respected() {
        let b = seed();
        let hits = b
            .search(&Query {
                limit: 1,
                ..Query::new("token login")
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn search_on_empty_store_returns_nothing() {
        let b = InMemoryBackend::new();
        assert!(b.search(&Query::new("anything")).unwrap().is_empty());
    }

    #[test]
    fn results_are_deterministic_across_runs() {
        let ids = |b: &InMemoryBackend| -> Vec<String> {
            b.search(&Query::new("token login status"))
                .unwrap()
                .into_iter()
                .map(|h| h.node.id)
                .collect()
        };
        let b = seed();
        assert_eq!(ids(&b), ids(&b));
    }

    #[test]
    fn scores_stay_in_unit_interval() {
        let b = seed();
        for h in b
            .search(&Query::new("token login fix render").with_embedding(vec![1.0, 1.0, 1.0]))
            .unwrap()
        {
            assert!(
                (0.0..=1.0).contains(&h.score),
                "score out of range for {}: {}",
                h.node.id,
                h.score
            );
        }
    }

    #[test]
    fn origin_reports_how_a_hit_was_reached() {
        let b = seed();
        let graph = b.neighbours("file:auth.rs", 1, &[]).unwrap();
        assert_eq!(origin_of(&graph[0]), Origin::Graph);
        let sem = b
            .search(&Query::new("").with_embedding(vec![0.9, 0.1, 0.0]))
            .unwrap();
        assert_eq!(origin_of(&sem[0]), Origin::Semantic);
    }

    #[test]
    fn forget_removes_the_node_and_its_edges() {
        let mut b = seed();
        b.forget("file:auth.rs").unwrap();
        assert!(b.get_node("file:auth.rs").unwrap().is_none());
        assert!(b
            .edges
            .iter()
            .all(|e| e.from != "file:auth.rs" && e.to != "file:auth.rs"));
        assert_eq!(b.len().unwrap(), 3);
    }

    #[test]
    fn forgetting_an_unknown_id_is_an_error() {
        let mut b = seed();
        assert!(matches!(b.forget("nope"), Err(MemoryError::NotFound(_))));
    }

    #[test]
    fn clear_removes_everything() {
        let mut b = seed();
        b.clear().unwrap();
        assert_eq!(b.len().unwrap(), 0);
        assert!(b.neighbours("file:auth.rs", 1, &[]).is_err());
    }
}
