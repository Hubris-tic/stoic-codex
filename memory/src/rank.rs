//! Shared ranking. Every backend ranks with this, so "why is this result first?" has one
//! answer regardless of whether the nodes came from memory, SQLite, or TypeDB.
//!
//! Backends differ in how they *load* candidates and how they *find* graph proximity;
//! they must not differ in how they *score* them.

use std::collections::{BTreeMap, VecDeque};

use crate::backend::Query;
use crate::domain::{EdgeKind, Hit, MemoryEdge, MemoryNode, Signals};
use crate::scoring::{self, Weights};

/// Undirected adjacency, both directions, since "what affects this" and "what does this
/// affect" are both legitimate questions about code.
pub fn adjacency(edges: &[MemoryEdge]) -> BTreeMap<String, Vec<(String, EdgeKind)>> {
    let mut adj: BTreeMap<String, Vec<(String, EdgeKind)>> = BTreeMap::new();
    for e in edges {
        adj.entry(e.from.clone())
            .or_default()
            .push((e.to.clone(), e.kind));
        adj.entry(e.to.clone())
            .or_default()
            .push((e.from.clone(), e.kind));
    }
    adj
}

/// Hop distance from `start` to every node within `max_hops`. Stops on cycles.
pub fn hop_map(
    start: &str,
    max_hops: u32,
    adj: &BTreeMap<String, Vec<(String, EdgeKind)>>,
) -> BTreeMap<String, u32> {
    let mut hops = BTreeMap::new();
    hops.insert(start.to_string(), 0);
    let mut queue = VecDeque::from([(start.to_string(), 0u32)]);
    while let Some((cur, d)) = queue.pop_front() {
        if d >= max_hops {
            continue;
        }
        for (next, _) in adj.get(&cur).into_iter().flatten() {
            if !hops.contains_key(next) {
                hops.insert(next.clone(), d + 1);
                queue.push_back((next.clone(), d + 1));
            }
        }
    }
    hops
}

/// Score and order candidates. `hits` are truncated to `query.limit`.
pub fn rank(
    nodes: &[MemoryNode],
    query: &Query,
    weights: &Weights,
    now: i64,
    hops_from_focus: &BTreeMap<String, u32>,
) -> Vec<Hit> {
    let q_terms = scoring::terms(&query.text);
    let total_docs = nodes.len();
    let mut df: BTreeMap<String, usize> = BTreeMap::new();
    let mut docs: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut total_len = 0usize;
    for n in nodes {
        let terms = scoring::terms(&format!("{} {}", n.label, n.text));
        // Document frequency counts *documents*, not occurrences: a doc that says
        // "token" five times still adds one. Counting occurrences let df exceed the
        // corpus size, drove IDF negative, and the floor silently zeroed every score —
        // which surfaced as "search returns nothing" for perfectly good queries.
        let distinct: std::collections::BTreeSet<&str> = terms.iter().map(String::as_str).collect();
        for t in distinct {
            *df.entry(t.to_string()).or_insert(0) += 1;
        }
        total_len += terms.len();
        docs.insert(n.id.as_str(), terms);
    }
    let avg_len = if total_docs == 0 {
        1.0
    } else {
        total_len as f32 / total_docs as f32
    };
    let df_fn = |t: &str| df.get(t).copied().unwrap_or(0);

    let mut out = Vec::new();
    for node in nodes {
        if !query.kinds.is_empty() && !query.kinds.contains(&node.kind) {
            continue;
        }
        let mut s = Signals {
            recency: scoring::recency(node.created_at, now, 30.0),
            importance: node.importance,
            ..Default::default()
        };
        if let (Some(qe), Some(ne)) = (&query.embedding, &node.embedding) {
            s.cosine = scoring::cosine(qe, ne).max(0.0);
        }
        if let Some(t) = docs.get(node.id.as_str()) {
            s.bm25 = scoring::bm25(&q_terms, t, avg_len, total_docs, &df_fn);
        }
        if let Some(h) = hops_from_focus.get(&node.id) {
            s.graph = scoring::graph_proximity(*h);
        }
        // Importance and recency only modulate relevance; a node that matched nothing
        // at all must not surface just for being recent or important. Without this, a
        // query for "zebra" returns the whole store at near-zero scores and the prompt
        // budget fills with noise.
        let relevant = s.cosine > 0.0 || s.bm25 > 0.0 || s.graph > 0.0 || s.inferred > 0.0;
        if !relevant {
            continue;
        }
        let score = scoring::combine(&s, weights);
        if score <= 0.0 {
            continue;
        }
        let via = match (hops_from_focus.get(&node.id), query.focus.as_ref()) {
            (Some(h), Some(f)) if *h > 0 => vec![format!("{h} hop(s) from {f}")],
            _ => Vec::new(),
        };
        out.push(Hit {
            node: node.clone(),
            score,
            signals: s,
            via,
        });
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.node.id.cmp(&b.node.id))
    });
    out.truncate(query.limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::NodeKind;

    fn n(id: &str, label: &str, text: &str) -> MemoryNode {
        MemoryNode::new(id, NodeKind::Doc, label).with_text(text)
    }

    #[test]
    fn adjacency_is_undirected() {
        let adj = adjacency(&[MemoryEdge::new("a", "b", EdgeKind::Calls)]);
        assert_eq!(adj.get("a").unwrap().len(), 1);
        assert_eq!(adj.get("b").unwrap().len(), 1);
    }

    #[test]
    fn hop_map_terminates_on_a_cycle() {
        let adj = adjacency(&[
            MemoryEdge::new("a", "b", EdgeKind::Calls),
            MemoryEdge::new("b", "a", EdgeKind::Imports),
        ]);
        let hops = hop_map("a", 10, &adj);
        assert_eq!(hops.len(), 2);
        assert_eq!(hops["b"], 1);
    }

    #[test]
    fn hop_map_respects_the_limit() {
        let adj = adjacency(&[
            MemoryEdge::new("a", "b", EdgeKind::Calls),
            MemoryEdge::new("b", "c", EdgeKind::Calls),
        ]);
        assert!(hop_map("a", 1, &adj).get("c").is_none());
        assert_eq!(hop_map("a", 2, &adj)["c"], 2);
    }

    #[test]
    fn rank_orders_by_score_then_id() {
        let nodes = vec![
            n("x", "alpha", "unique token"),
            n("y", "beta", "unrelated words"),
        ];
        let hits = rank(
            &nodes,
            &Query::new("token"),
            &Weights::default(),
            0,
            &BTreeMap::new(),
        );
        assert_eq!(hits[0].node.id, "x");
    }

    #[test]
    fn rank_drops_zero_signal_nodes() {
        let nodes = vec![n("x", "alpha", "nothing relevant")];
        let q = Query {
            ..Query::new("zebra")
        };
        assert!(rank(&nodes, &q, &Weights::default(), 0, &BTreeMap::new()).is_empty());
    }

    #[test]
    fn rank_truncates_to_the_requested_limit() {
        let nodes: Vec<MemoryNode> = (0..10)
            .map(|i| n(&format!("n{i}"), "token", "token body"))
            .collect();
        let hits = rank(
            &nodes,
            &Query {
                limit: 3,
                ..Query::new("token")
            },
            &Weights::default(),
            0,
            &BTreeMap::new(),
        );
        assert_eq!(hits.len(), 3);
    }
}
