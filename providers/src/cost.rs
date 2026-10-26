//! Cost accounting.
//!
//! A BYOK agent that cannot tell you what a session costs is not production-ready.
//! Prices are data, not code: they live in the registry JSON so they can be updated
//! from config without a rebuild. All money math is integer micro-USD to avoid the
//! float drift that makes invoices not add up.

use serde::{Deserialize, Serialize};

use crate::ir::Usage;

/// USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    /// Multiplier on the input price, e.g. 0.1 = cache reads are 10% of input.
    #[serde(default = "one")]
    pub cache_read_multiplier: f64,
    /// Multiplier on the input price for writing to cache, e.g. 1.25.
    #[serde(default = "one")]
    pub cache_write_multiplier: f64,
}

fn one() -> f64 {
    1.0
}

impl Default for Pricing {
    fn default() -> Self {
        Pricing {
            input_per_mtok: 0.0,
            output_per_mtok: 0.0,
            cache_read_multiplier: 1.0,
            cache_write_multiplier: 1.0,
        }
    }
}

impl Pricing {
    pub fn new(input_per_mtok: f64, output_per_mtok: f64) -> Self {
        Pricing {
            input_per_mtok,
            output_per_mtok,
            ..Default::default()
        }
    }

    /// Billable micro-USD for one call. Uncached input is `input - cache_read - cache_write`
    /// clamped at zero, because providers report those buckets separately.
    pub fn cost_micros(&self, u: &Usage) -> u64 {
        let cached_read =
            u.cache_read_tokens as f64 * self.input_per_mtok * self.cache_read_multiplier;
        let cached_write =
            u.cache_write_tokens as f64 * self.input_per_mtok * self.cache_write_multiplier;
        let uncached =
            (u.input_tokens
                .saturating_sub(u.cache_read_tokens + u.cache_write_tokens)) as f64
                * self.input_per_mtok;
        let out = u.output_tokens as f64 * self.output_per_mtok;
        ((cached_read + cached_write + uncached + out) / 1_000_000.0 * 1_000_000.0).round() as u64
    }

    pub fn cost_usd(&self, u: &Usage) -> f64 {
        self.cost_micros(u) as f64 / 1_000_000.0
    }
}

/// Running tally for a session or a squad member.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub micros: u64,
}

impl Ledger {
    pub fn record(&mut self, pricing: &Pricing, u: &Usage) {
        self.calls += 1;
        self.input_tokens += u.input_tokens;
        self.output_tokens += u.output_tokens;
        self.cache_read_tokens += u.cache_read_tokens;
        self.cache_write_tokens += u.cache_write_tokens;
        self.micros += pricing.cost_micros(u);
    }

    pub fn usd(&self) -> f64 {
        self.micros as f64 / 1_000_000.0
    }

    /// Share of input tokens served from cache — the single most useful number for
    /// tuning prompt layout, since it is where most of the savings live.
    pub fn cache_hit_ratio(&self) -> f64 {
        let total = self.input_tokens + self.cache_read_tokens;
        if total == 0 {
            0.0
        } else {
            self.cache_read_tokens as f64 / total as f64
        }
    }
}
