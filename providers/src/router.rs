//! Capability-aware model router.
//!
//! The point of BYOK is not "let me type a different API key". It is that the
//! harness can choose the right model per sub-task: a cheap model writes the
//! retrieval query, a strong model plans the refactor, a local model handles
//! secret-adjacent files. Doing that badly is worse than not doing it, so selection
//! is deterministic and always explains itself — every choice returns a `rationale`
//! that shows up in logs and the UI.

use serde::{Deserialize, Serialize};

use crate::capabilities::Needs;
use crate::error::{Error, Result};
use crate::registry::{ModelSpec, Registry, Tier};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskClass {
    /// Pick a file, write a search query, label a diff.
    Classify,
    /// Turn a question into structured retrieval.
    Retrieve,
    /// Architecture, multi-file plan, decomposition.
    Plan,
    /// Write or edit code.
    Patch,
    /// Critique a diff, find defects.
    Review,
    /// Compress history into memory.
    Summarize,
    /// Run tests, interpret failures, verify claims.
    Verify,
}

impl TaskClass {
    /// How much a stronger model is worth for this class, 0..1.
    fn strength_weight(self) -> f64 {
        match self {
            TaskClass::Classify => 0.1,
            TaskClass::Retrieve => 0.2,
            TaskClass::Summarize => 0.3,
            TaskClass::Verify => 0.5,
            TaskClass::Patch => 0.8,
            TaskClass::Review => 0.85,
            TaskClass::Plan => 1.0,
        }
    }
}

fn tier_score(t: Tier) -> f64 {
    match t {
        Tier::Small => 0.35,
        Tier::Mid => 0.7,
        Tier::Strong => 1.0,
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Requirements {
    pub needs: Needs,
    pub min_context: u32,
    /// Hard per-call ceiling in micro-USD; 0 = unlimited.
    pub max_cost_micros: u64,
    /// Prefer local models when they are good enough (privacy / offline / free).
    pub prefer_local: bool,
    /// Cap the tier, e.g. a "cheap mode" that refuses Strong models.
    pub max_tier: Option<Tier>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Selection {
    pub model: String,
    pub provider: String,
    pub tier: Tier,
    pub score: f64,
    pub rationale: Vec<String>,
}

pub struct Router {
    registry: Registry,
    /// Weight of cost relative to quality. 0 = quality only, 1 = cost dominates.
    pub cost_weight: f64,
}

impl Router {
    pub fn new(registry: Registry) -> Self {
        Router {
            registry,
            cost_weight: 0.35,
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn select(&self, task: TaskClass, req: &Requirements) -> Result<Selection> {
        let mut rejected: Vec<String> = Vec::new();
        let mut candidates: Vec<(f64, &ModelSpec, Vec<String>)> = Vec::new();
        let max_price = self
            .registry
            .iter()
            .map(|m| m.pricing.input_per_mtok + m.pricing.output_per_mtok)
            .fold(0.0_f64, f64::max)
            .max(1.0);

        for m in self.registry.iter() {
            let mut why = Vec::new();
            if let Some(max) = req.max_tier {
                if m.tier > max {
                    rejected.push(format!("{}: tier above ceiling", m.id));
                    continue;
                }
            }
            if m.capabilities.max_context < req.min_context {
                rejected.push(format!(
                    "{}: context {} < {}",
                    m.id, m.capabilities.max_context, req.min_context
                ));
                continue;
            }
            let missing = m.capabilities.missing(&req.needs);
            if !missing.is_empty() {
                rejected.push(format!("{}: missing {}", m.id, missing.join(",")));
                continue;
            }
            if !m.capabilities.streaming {
                rejected.push(format!("{}: no streaming", m.id));
                continue;
            }

            // Quality: how well this tier matches how much strength the task needs.
            let need = task.strength_weight();
            let have = tier_score(m.tier);
            // Overqualified is mildly wasteful, underqualified is costly. Penalize
            // shortfall twice as hard as waste.
            let quality = if have >= need {
                1.0 - (have - need) * 0.15
            } else {
                1.0 - (need - have) * 0.9
            };

            let price_norm =
                (m.pricing.input_per_mtok + m.pricing.output_per_mtok) / (2.0 * max_price);
            let mut score = quality - self.cost_weight * price_norm;

            if m.local {
                if req.prefer_local {
                    score += 0.25;
                    why.push("local model, no data leaves the machine (preference bonus)".into());
                } else {
                    score -= 0.05;
                }
            }
            if req.max_cost_micros > 0 {
                // Approximate one-call cost at 2k in / 1k out.
                let est = m.pricing.cost_micros(&crate::ir::Usage {
                    input_tokens: 2_000,
                    output_tokens: 1_000,
                    ..Default::default()
                });
                if est > req.max_cost_micros {
                    rejected.push(format!(
                        "{}: est {} µ$ > ceiling {}",
                        m.id, est, req.max_cost_micros
                    ));
                    continue;
                }
                why.push(format!("est {est} µ$/call under ceiling"));
            }
            why.push(format!(
                "quality {:.2} for {:?} (tier {:?})",
                quality, task, m.tier
            ));
            why.push(format!("price index {:.2}", price_norm));
            candidates.push((score, m, why));
        }

        if candidates.is_empty() {
            return Err(Error::Config {
                detail: format!("no model satisfies {:?}: {}", task, rejected.join("; ")),
            });
        }

        // Deterministic: score desc, then tier desc, then id asc.
        candidates.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.1.tier.cmp(&a.1.tier))
                .then(a.1.id.cmp(&b.1.id))
        });
        let (score, model, mut why) = candidates.remove(0);
        why.insert(0, format!("selected for {:?}", task));
        Ok(Selection {
            model: model.id.clone(),
            provider: model.provider.clone(),
            tier: model.tier,
            score,
            rationale: why,
        })
    }
}
