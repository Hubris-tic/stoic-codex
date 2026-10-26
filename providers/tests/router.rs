//! Router behaviour: cheap models for cheap work, strong models for hard work,
//! and a rationale for every decision.

use zenithal_providers::capabilities::Needs;
use zenithal_providers::{Registry, Requirements, Router, TaskClass};

fn router() -> Router {
    Router::new(Registry::seeded())
}

#[test]
fn classification_prefers_a_cheap_model() {
    let sel = router()
        .select(TaskClass::Classify, &Requirements::default())
        .unwrap();
    assert_eq!(
        sel.provider, "gemini",
        "cheap tier should win for trivial work, got {:?}",
        sel
    );
    assert!(
        !sel.rationale.is_empty(),
        "every decision must explain itself"
    );
}

#[test]
fn planning_prefers_a_strong_model() {
    let sel = router()
        .select(TaskClass::Plan, &Requirements::default())
        .unwrap();
    assert!(
        matches!(sel.tier, zenithal_providers::registry::Tier::Strong),
        "planning needs a strong model, got {:?}",
        sel
    );
}

#[test]
fn tool_requirement_filters_out_non_tool_models() {
    let mut reg = Registry::seeded();
    let mut spec = reg.get("gemini:flash").unwrap().clone();
    spec.id = "notools:model".into();
    spec.provider = "notools".into();
    spec.capabilities.tools = false;
    spec.capabilities.parallel_tools = false;
    reg.insert(spec);

    let r = Router::new(reg);
    let req = Requirements {
        needs: Needs {
            tools: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let sel = r.select(TaskClass::Classify, &req).unwrap();
    assert_ne!(
        sel.model, "notools:model",
        "a model without tools must never be chosen for tool work"
    );
}

#[test]
fn context_requirement_is_hard() {
    let mut reg = Registry::seeded();
    for m in ["gemini:flash", "ollama:qwen-coder"] {
        if let Some(s) = reg.get(m).cloned() {
            let mut s2 = s;
            s2.capabilities.max_context = 8_000;
            reg.insert(s2);
        }
    }
    let r = Router::new(reg);
    let req = Requirements {
        min_context: 150_000,
        ..Default::default()
    };
    let sel = r.select(TaskClass::Patch, &req).unwrap();
    assert!(
        r.registry()
            .get(&sel.model)
            .unwrap()
            .capabilities
            .max_context
            >= 150_000,
        "chose {} with too little context",
        sel.model
    );
}

#[test]
fn prefer_local_picks_the_local_model_when_capable() {
    let req = Requirements {
        prefer_local: true,
        ..Default::default()
    };
    let sel = router().select(TaskClass::Patch, &req).unwrap();
    assert_eq!(
        sel.provider, "ollama",
        "local preference should win: {:?}",
        sel
    );
    assert!(
        sel.rationale.iter().any(|r| r.contains("local")),
        "rationale must mention locality"
    );
}

#[test]
fn tier_ceiling_is_respected() {
    let req = Requirements {
        max_tier: Some(zenithal_providers::registry::Tier::Small),
        ..Default::default()
    };
    let sel = router()
        .select(TaskClass::Patch, &Requirements { ..req })
        .unwrap();
    assert_eq!(sel.tier, zenithal_providers::registry::Tier::Small);
}

#[test]
fn unsatisfiable_requirements_explain_why() {
    let req = Requirements {
        min_context: 10_000_000,
        ..Default::default()
    };
    let err = router().select(TaskClass::Plan, &req).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("no model satisfies"), "{msg}");
    assert!(
        msg.contains("context"),
        "error should name the failing constraint: {msg}"
    );
}

#[test]
fn selection_is_deterministic() {
    let r = router();
    let a = r
        .select(TaskClass::Review, &Requirements::default())
        .unwrap();
    let b = r
        .select(TaskClass::Review, &Requirements::default())
        .unwrap();
    assert_eq!(a, b, "routing must not depend on map iteration order");
}
