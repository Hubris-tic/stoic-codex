//! Catalogue discovery stays at the wire-shape layer: request construction and parsing are
//! testable without a key, an HTTP call, or a completion.

use zenithal_providers::{adapter_for, AuthSpec};

#[test]
fn free_tier_provider_presets_build_read_only_model_requests() {
    for (id, expected_path) in [
        ("gemini", "/v1beta/models"),
        ("groq", "/openai/v1/models"),
        ("openrouter", "/api/v1/models"),
    ] {
        let adapter = adapter_for(id).expect("built-in provider");
        let req = adapter.list_models_request().expect("catalogue endpoint");
        assert_eq!(req.method, "GET", "{id}");
        assert!(req.url.ends_with(expected_path), "{}: {}", id, req.url);
        assert!(
            req.body.is_null(),
            "catalogue GET must not carry a prompt: {id}"
        );
    }

    let gemini = adapter_for("gemini")
        .unwrap()
        .list_models_request()
        .unwrap();
    assert_eq!(gemini.auth, AuthSpec::header("x-goog-api-key"));

    for id in ["groq", "openrouter"] {
        let req = adapter_for(id).unwrap().list_models_request().unwrap();
        assert_eq!(req.auth, AuthSpec::Bearer, "{id}");
    }
}

#[test]
fn paid_provider_catalogue_requests_are_defined_but_not_live_tested() {
    // Wire support is independent from whether the user has a paid key. Constructing these
    // requests is offline; no completion or external request is made by this test.
    for id in ["anthropic", "openai"] {
        let adapter = adapter_for(id).expect("built-in provider");
        let req = adapter.list_models_request().expect("catalogue endpoint");
        assert_eq!(req.method, "GET", "{id}");
        assert!(req.url.ends_with("/models"), "{}: {}", id, req.url);
        assert!(
            req.body.is_null(),
            "catalogue GET must not carry a prompt: {id}"
        );
    }
}
