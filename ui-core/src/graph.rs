//! Memory-graph layout: turns the hybrid vector+graph store into a deterministic
//! terminal layout.
//!
//! Terminals cannot draw a force-directed cloud legibly. What works is a **layered**
//! layout: pick a focus node (the thing the agent is working on), rank everything by
//! graph distance, and place rank `r` on row `r`. That reads like a call tree, is stable
//! across redraws, and needs no physics — which also means it costs microseconds and
//! never makes memory usage unpredictable.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

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
    pub fn glyph(self) -> char {
        match self {
            NodeKind::File => '▪',
            NodeKind::Symbol => 'ƒ',
            NodeKind::Commit => '●',
            NodeKind::Task => '▸',
            NodeKind::Decision => '◆',
            NodeKind::Error => '✗',
            NodeKind::Agent => '☰',
            NodeKind::Doc => '≡',
        }
    }

    pub fn label(self) -> &'static str {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
    pub fn label(self) -> &'static str {
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

    /// Two glyphs to draw the link in a narrow column.
    pub fn rule(self) -> &'static str {
        match self {
            EdgeKind::Calls => "->",
            EdgeKind::Imports => "=>",
            EdgeKind::Fixes => "++",
            EdgeKind::Regresses => "!!",
            EdgeKind::Blocks => "x-",
            EdgeKind::Supersedes => "~~",
            EdgeKind::DerivedFrom => "..",
            EdgeKind::AuthoredBy => "@@",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub label: String,
    /// Relevance score from hybrid retrieval (cosine + bm25 + graph + recency).
    /// Higher sorts first within a rank.
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Graph {
    pub fn add_node(&mut self, n: Node) {
        self.nodes.push(n);
    }

    pub fn add_edge(&mut self, from: impl Into<String>, to: impl Into<String>, kind: EdgeKind) {
        self.edges.push(Edge { from: from.into(), to: to.into(), kind });
    }

    fn index(&self) -> BTreeMap<&str, &Node> {
        self.nodes.iter().map(|n| (n.id.as_str(), n)).collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub node: Node,
    /// Graph distance from the focus; 0 is the focus itself.
    pub rank: u32,
    /// Column within the rank, 0 = leftmost.
    pub column: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub from: Placed,
    pub to: Placed,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub placed: Vec<Placed>,
    pub links: Vec<Link>,
    /// Nodes that were dropped because of `max_nodes`.
    pub omitted: usize,
    pub max_rank: u32,
    pub width: u16,
}

/// Lay out the neighbourhood of `focus` up to `depth` hops, keeping at most
/// `max_nodes` (highest scoring first). Deterministic for a given input.
pub fn layout(graph: &Graph, focus: &str, depth: u32, max_nodes: usize) -> Layout {
    let idx = graph.index();
    if !idx.contains_key(focus) {
        return Layout::default();
    }

    // Undirected adjacency: the graph is a knowledge graph, so a decision pointing at
    // a file is just as relevant as a file pointing at a decision.
    let mut adj: BTreeMap<&str, Vec<(&str, EdgeKind)>> = BTreeMap::new();
    for e in &graph.edges {
        if idx.contains_key(e.from.as_str()) && idx.contains_key(e.to.as_str()) {
            adj.entry(e.from.as_str()).or_default().push((e.to.as_str(), e.kind));
            adj.entry(e.to.as_str()).or_default().push((e.from.as_str(), e.kind));
        }
    }

    // BFS for distance.
    let mut dist: BTreeMap<&str, u32> = BTreeMap::new();
    dist.insert(focus, 0);
    let mut q = VecDeque::new();
    q.push_back(focus);
    while let Some(cur) = q.pop_front() {
        let d = dist[cur];
        if d >= depth {
            continue;
        }
        if let Some(neigh) = adj.get(cur) {
            for (n, _) in neigh {
                if !dist.contains_key(n) {
                    dist.insert(n, d + 1);
                    q.push_back(n);
                }
            }
        }
    }

    // Rank + sort.
    let mut by_rank: BTreeMap<u32, Vec<&Node>> = BTreeMap::new();
    for n in &graph.nodes {
        if let Some(d) = dist.get(n.id.as_str()) {
            by_rank.entry(*d).or_default().push(n);
        }
    }
    for (_, v) in by_rank.iter_mut() {
        v.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.kind.cmp(&b.kind))
                .then(a.id.cmp(&b.id))
        });
    }

    // Truncate globally, keeping the focus and nearest ranks first.
    let mut budget = max_nodes.max(1);
    let mut kept: Vec<Placed> = Vec::new();
    let mut omitted = 0usize;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut max_rank = 0u32;
    for (rank, nodes) in by_rank {
        let mut column = 0u16;
        for n in nodes {
            if budget == 0 {
                omitted += 1;
                continue;
            }
            budget -= 1;
            seen.insert(n.id.clone());
            max_rank = max_rank.max(rank);
            kept.push(Placed { node: n.clone(), rank, column });
            column += 1;
        }
    }

    let placed_by_id: BTreeMap<&str, &Placed> = kept.iter().map(|p| (p.node.id.as_str(), p)).collect();
    let mut links = Vec::new();
    for e in &graph.edges {
        if let (Some(a), Some(b)) = (placed_by_id.get(e.from.as_str()), placed_by_id.get(e.to.as_str())) {
            // Draw the link from the nearer node to the further one so the arrow reads
            // as "outward from the focus".
            let (a, b) = if a.rank <= b.rank { (a, b) } else { (b, a) };
            links.push(Link { from: (*a).clone(), to: (*b).clone(), kind: e.kind });
        }
    }
    links.sort_by(|x, y| {
        (x.from.rank, x.from.column)
            .cmp(&(y.from.rank, y.from.column))
            .then((x.to.rank, x.to.column).cmp(&(y.to.rank, y.to.column)))
    });

    let width = kept
        .iter()
        .map(|p| p.column)
        .max()
        .map(|c| c + 1)
        .unwrap_or(0);

    Layout { placed: kept, links, omitted, max_rank, width }
}

impl Layout {
    /// Rows as the renderer draws them: one row per rank, focus first.
    pub fn rows(&self) -> Vec<(u32, Vec<&Placed>)> {
        let mut by_rank: BTreeMap<u32, Vec<&Placed>> = BTreeMap::new();
        for p in &self.placed {
            by_rank.entry(p.rank).or_default().push(p);
        }
        for v in by_rank.values_mut() {
            v.sort_by_key(|p| p.column);
        }
        by_rank.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> Graph {
        let mut g = Graph::default();
        g.add_node(Node { id: "f".into(), kind: NodeKind::File, label: "src/auth.rs".into(), score: 0.9 });
        g.add_node(Node { id: "s".into(), kind: NodeKind::Symbol, label: "fn verify".into(), score: 0.8 });
        g.add_node(Node { id: "d".into(), kind: NodeKind::Decision, label: "use argon2".into(), score: 0.7 });
        g.add_node(Node { id: "e".into(), kind: NodeKind::Error, label: "timing leak".into(), score: 0.6 });
        g.add_node(Node { id: "far".into(), kind: NodeKind::Doc, label: "docs/x.md".into(), score: 0.1 });
        g.add_edge("f", "s", EdgeKind::Imports);
        g.add_edge("d", "f", EdgeKind::DerivedFrom);
        g.add_edge("e", "f", EdgeKind::Regresses);
        g.add_node(Node { id: "island".into(), kind: NodeKind::Task, label: "unrelated".into(), score: 0.5 });
        g
    }

    #[test]
    fn focus_is_rank_zero_and_neighbours_are_rank_one() {
        let l = layout(&demo(), "f", 2, 100);
        let focus = l.placed.iter().find(|p| p.node.id == "f").unwrap();
        assert_eq!(focus.rank, 0);
        for id in ["s", "d", "e"] {
            let p = l.placed.iter().find(|x| x.node.id == id).unwrap();
            assert_eq!(p.rank, 1, "{id} should be one hop from focus");
        }
    }

    #[test]
    fn depth_limits_reach() {
        let l = layout(&demo(), "f", 1, 100);
        assert!(l.placed.iter().all(|p| p.rank <= 1));
        assert_eq!(l.max_rank, 1);
    }

    #[test]
    fn disconnected_nodes_are_not_placed() {
        let l = layout(&demo(), "f", 3, 100);
        assert!(
            !l.placed.iter().any(|p| p.node.id == "island"),
            "unreachable nodes must not appear in the neighbourhood view"
        );
    }

    #[test]
    fn unknown_focus_yields_empty_layout_not_panic() {
        let l = layout(&demo(), "nope", 2, 50);
        assert!(l.placed.is_empty());
        assert_eq!(l.omitted, 0);
    }

    #[test]
    fn deterministic_for_identical_input() {
        let g = demo();
        let a = layout(&g, "f", 3, 100);
        let b = layout(&g, "f", 3, 100);
        assert_eq!(a.placed, b.placed);
        assert_eq!(a.links.len(), b.links.len());
        let ids = |l: &Layout| -> Vec<String> { l.placed.iter().map(|p| p.node.id.clone()).collect() };
        assert_eq!(ids(&a), ids(&b));
    }

    #[test]
    fn budget_truncates_by_rank_then_score() {
        let l = layout(&demo(), "f", 3, 2);
        assert_eq!(l.placed.len(), 2);
        assert!(l.omitted > 0);
        // Focus is always kept even under a tight budget.
        assert!(l.placed.iter().any(|p| p.node.id == "f"));
    }

    #[test]
    fn columns_are_dense_within_a_rank() {
        let l = layout(&demo(), "f", 3, 100);
        for (_rank, row) in l.rows() {
            let cols: Vec<u16> = row.iter().map(|p| p.column).collect();
            let expected: Vec<u16> = (0..cols.len() as u16).collect();
            assert_eq!(cols, expected, "columns must be 0..n with no gaps");
        }
    }

    #[test]
    fn higher_scoring_nodes_come_first_in_a_rank() {
        let l = layout(&demo(), "f", 2, 100);
        let rank1: Vec<&str> = l
            .rows()
            .into_iter()
            .find(|(r, _)| *r == 1)
            .map(|(_, v)| v.iter().map(|p| p.node.id.as_str()).collect())
            .unwrap();
        assert_eq!(rank1[0], "s", "0.8 score before 0.7 and 0.6: {rank1:?}");
    }

    #[test]
    fn links_only_reference_placed_nodes() {
        let l = layout(&demo(), "f", 1, 2); // forces truncation
        let ids: BTreeSet<&str> = l.placed.iter().map(|p| p.node.id.as_str()).collect();
        for link in &l.links {
            assert!(ids.contains(link.from.node.id.as_str()));
            assert!(ids.contains(link.to.node.id.as_str()));
        }
    }

    #[test]
    fn edge_rules_are_distinct_two_char_glyphs() {
        let all = [
            EdgeKind::Calls, EdgeKind::Imports, EdgeKind::Fixes, EdgeKind::Regresses,
            EdgeKind::Blocks, EdgeKind::Supersedes, EdgeKind::DerivedFrom, EdgeKind::AuthoredBy,
        ];
        let mut rules: Vec<&str> = all.iter().map(|e| e.rule()).collect();
        assert!(rules.iter().all(|r| r.chars().count() == 2), "rules must fit a 2-col gutter");
        rules.sort_unstable();
        rules.dedup();
        assert_eq!(rules.len(), all.len(), "edge rules must be distinguishable");
    }
}
