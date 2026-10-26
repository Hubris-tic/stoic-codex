//! Cost accounting: integer micro-USD, cache-aware.

use stoic_codex_providers::cost::{Ledger, Pricing};
use stoic_codex_providers::Usage;

#[test]
fn plain_cost_math() {
    let p = Pricing::new(1.0, 2.0); // $1/Mtok in, $2/Mtok out
    let u = Usage {
        input_tokens: 1_000_000,
        output_tokens: 500_000,
        ..Default::default()
    };
    assert_eq!(
        p.cost_micros(&u),
        2_000_000,
        "1M in + 0.5M out at $2/Mtok = $2"
    );
    assert!((p.cost_usd(&u) - 2.0).abs() < 1e-9);
}

#[test]
fn cached_tokens_are_discounted_not_double_counted() {
    let p = Pricing {
        cache_read_multiplier: 0.1,
        ..Pricing::new(10.0, 10.0)
    };
    // 1000 input tokens of which 800 were cache reads.
    let u = Usage {
        input_tokens: 1_000,
        cache_read_tokens: 800,
        ..Default::default()
    };
    let micros = p.cost_micros(&u);
    // 200 uncached @ $10/Mtok = 2000 µ$; 800 cached @ $1/Mtok = 800 µ$; total 2800.
    assert_eq!(micros, 2_800, "got {micros}");
}

#[test]
fn cache_writes_are_premium_priced() {
    let p = Pricing {
        cache_write_multiplier: 1.25,
        ..Pricing::new(8.0, 0.0)
    };
    let u = Usage {
        input_tokens: 0,
        cache_write_tokens: 1_000,
        ..Default::default()
    };
    // 1000 * 8 * 1.25 / 1e6 * 1e6 = 10_000 µ$
    assert_eq!(p.cost_micros(&u), 10_000);
}

#[test]
fn ledger_accumulates_and_reports_hit_ratio() {
    let p = Pricing::new(3.0, 15.0);
    let mut l = Ledger::default();
    l.record(
        &p,
        &Usage {
            input_tokens: 1_000,
            output_tokens: 200,
            ..Default::default()
        },
    );
    l.record(
        &p,
        &Usage {
            input_tokens: 1_000,
            output_tokens: 200,
            cache_read_tokens: 1_000,
            ..Default::default()
        },
    );
    assert_eq!(l.calls, 2);
    assert_eq!(l.input_tokens, 2_000);
    assert_eq!(l.output_tokens, 400);
    assert!(l.usd() > 0.0);
    assert!(l.cache_hit_ratio() > 0.0 && l.cache_hit_ratio() < 1.0);
}

#[test]
fn local_models_are_free() {
    let p = Pricing::default();
    let u = Usage {
        input_tokens: 10_000_000,
        output_tokens: 10_000_000,
        ..Default::default()
    };
    assert_eq!(p.cost_micros(&u), 0);
}
