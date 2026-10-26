//! Hybrid retrieval scoring.
//!
//! One formula, used by every backend, so "why is this result here?" has one answer:
//!
//! ```text
//! score = w_cos·cosine + w_bm25·bm25 + w_graph·graph + w_rec·recency
//!       + w_imp·importance + w_inf·inferred
//! ```
//!
//! Weights live here rather than in a backend because tuning retrieval is a product
//! decision, not a storage decision — and a score you cannot reproduce cannot be debugged.

use crate::domain::Signals;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weights {
    pub cosine: f32,
    pub bm25: f32,
    pub graph: f32,
    pub recency: f32,
    pub importance: f32,
    pub inferred: f32,
}

impl Default for Weights {
    fn default() -> Self {
        // Semantic first, but graph proximity matters a lot for code: "near the file I
        // am editing" beats "semantically similar somewhere else in the repo".
        Weights {
            cosine: 0.40,
            bm25: 0.20,
            graph: 0.20,
            recency: 0.08,
            importance: 0.07,
            inferred: 0.05,
        }
    }
}

impl Weights {
    /// Retrieval tuned for "what do I need to know before editing this file".
    pub fn for_editing() -> Self {
        Weights {
            cosine: 0.28,
            bm25: 0.17,
            graph: 0.35,
            recency: 0.08,
            importance: 0.07,
            inferred: 0.05,
        }
    }

    pub fn sum(&self) -> f32 {
        self.cosine + self.bm25 + self.graph + self.recency + self.importance + self.inferred
    }
}

/// Combine signals into a single score. Weights are normalized so a caller cannot
/// accidentally produce a 0..5 score that breaks threshold checks downstream.
pub fn combine(s: &Signals, w: &Weights) -> f32 {
    let total = w.sum();
    if total <= 0.0 {
        return 0.0;
    }
    let raw = s.cosine * w.cosine
        + s.bm25 * w.bm25
        + s.graph * w.graph
        + s.recency * w.recency
        + s.importance * w.importance
        + s.inferred * w.inferred;
    (raw / total).clamp(0.0, 1.0)
}

/// Cosine similarity, treating a zero vector as "no signal" rather than NaN.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let (x, y) = (*x as f64, *y as f64);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    ((dot / (na.sqrt() * nb.sqrt())) as f32).clamp(-1.0, 1.0)
}

/// Recency as a 0..1 decay. `half_life_days` keeps the meaning obvious: a memory from
/// one half-life ago scores 0.5.
pub fn recency(created_at: i64, now: i64, half_life_days: f32) -> f32 {
    if created_at <= 0 || half_life_days <= 0.0 {
        return 0.0;
    }
    let age_days = ((now - created_at).max(0) as f32) / 86_400.0;
    (0.5f32).powf(age_days / half_life_days)
}

/// Graph proximity: 1.0 at the focus, decaying per hop. Not 1/hop because a two-hop
/// dependency is still highly relevant when you are editing the file that imports it.
pub fn graph_proximity(hops: u32) -> f32 {
    match hops {
        0 => 1.0,
        1 => 0.7,
        2 => 0.45,
        3 => 0.25,
        // Keep decaying past three hops instead of flat-lining: a long-but-real
        // dependency path should still be distinguishable from an unconnected node.
        h => 0.1 / (h as f32 - 2.0),
    }
}

/// Simple BM25 over a small candidate set. Good enough for a code memory store:
/// the heavy lifting is the vector + graph signals.
pub fn bm25(
    query_terms: &[String],
    doc_terms: &[String],
    avg_doc_len: f32,
    total_docs: usize,
    df: &dyn Fn(&str) -> usize,
) -> f32 {
    if query_terms.is_empty() || doc_terms.is_empty() || total_docs == 0 {
        return 0.0;
    }
    const K1: f32 = 1.2;
    const B: f32 = 0.75;
    let doc_len = doc_terms.len() as f32;
    let mut score = 0.0f32;
    for term in query_terms {
        let tf = doc_terms.iter().filter(|t| *t == term).count() as f32;
        if tf == 0.0 {
            continue;
        }
        let n = df(term) as f32;
        // Probabilistic IDF, floored so a term present in every doc contributes ~0
        // instead of going negative.
        let idf = ((total_docs as f32 - n + 0.5) / (n + 0.5) + 1.0)
            .ln()
            .max(0.0);
        let denom = tf + K1 * (1.0 - B + B * (doc_len / avg_doc_len.max(1.0)));
        score += idf * (tf * (K1 + 1.0)) / denom;
    }
    // Squash to 0..1; BM25 is unbounded and the combiner expects normalized inputs.
    score / (score + 1.0)
}

/// Split text into lowercase alphanumeric terms. Deliberately crude: a code memory
/// store should not depend on a language tokenizer that changes between releases.
pub fn terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_sum_to_one_by_default() {
        assert!((Weights::default().sum() - 1.0).abs() < 1e-6);
        assert!((Weights::for_editing().sum() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn combine_is_bounded_and_normalized() {
        let max = Signals {
            cosine: 1.0,
            bm25: 1.0,
            graph: 1.0,
            recency: 1.0,
            importance: 1.0,
            inferred: 1.0,
        };
        assert!((combine(&max, &Weights::default()) - 1.0).abs() < 1e-6);
        assert_eq!(combine(&Signals::default(), &Weights::default()), 0.0);
        // Unnormalized weights must not push the score above 1.
        let big = Weights {
            cosine: 10.0,
            bm25: 10.0,
            graph: 10.0,
            recency: 10.0,
            importance: 10.0,
            inferred: 10.0,
        };
        assert!(combine(&max, &big) <= 1.0);
    }

    #[test]
    fn cosine_handles_orthogonal_identical_and_degenerate_inputs() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(
            cosine(&[0.0, 0.0], &[1.0, 1.0]),
            0.0,
            "zero vector is no signal, not NaN"
        );
        assert_eq!(
            cosine(&[1.0, 2.0], &[1.0]),
            0.0,
            "length mismatch must not panic"
        );
    }

    #[test]
    fn recency_halves_at_one_half_life() {
        let day = 86_400;
        let now = 1_000 * day;
        assert!(
            (recency(now, now, 30.0) - 1.0).abs() < 1e-5,
            "fresh memory scores 1"
        );
        assert!((recency(now - 30 * day, now, 30.0) - 0.5).abs() < 1e-4);
        assert!((recency(now - 60 * day, now, 30.0) - 0.25).abs() < 1e-4);
        assert_eq!(recency(0, now, 30.0), 0.0, "unknown timestamp is no signal");
        assert_eq!(
            recency(now + 100 * day, now, 30.0),
            1.0,
            "future timestamps clamp to 1"
        );
    }

    #[test]
    fn graph_proximity_decays_monotonically() {
        let s: Vec<f32> = (0..6).map(graph_proximity).collect();
        assert_eq!(s[0], 1.0);
        for w in s.windows(2) {
            assert!(w[0] > w[1], "must strictly decrease: {s:?}");
        }
    }

    #[test]
    fn bm25_ranks_a_term_matching_doc_above_one_that_does_not() {
        let q = terms("retry backoff");
        let df = |t: &str| usize::from(t == "retry");
        let hit = bm25(&q, &terms("wire retry backoff into client"), 5.0, 10, &df);
        let miss = bm25(&q, &terms("render the status line"), 5.0, 10, &df);
        assert!(hit > 0.0);
        assert_eq!(miss, 0.0);
    }

    #[test]
    fn bm25_is_bounded() {
        let q = terms("a b c");
        let doc = terms("a b c a b c a b c a b c");
        let s = bm25(&q, &doc, 3.0, 5, &|_| 1);
        assert!(
            (0.0..=1.0).contains(&s),
            "bm25 must be squashed to 0..1, got {s}"
        );
    }

    #[test]
    fn editing_weights_favour_graph_over_semantics() {
        let d = Weights::default();
        let e = Weights::for_editing();
        assert!(e.graph > d.graph);
        assert!(e.cosine < d.cosine);
    }

    #[test]
    fn terms_are_lowercase_and_split_on_punctuation() {
        assert_eq!(
            terms("Auth.rs: verify_token()"),
            vec!["auth", "rs", "verify_token"]
        );
    }
}
