//! Live tests against the real Gemini API.
//!
//! These are `#[ignore]`d, because they cost money and need a network. They exist because
//! two of the bugs they cover were **only** visible against the real service:
//!
//!   1. Gemini 3.x rejects a replayed function call that is missing its `thoughtSignature`
//!      (HTTP 400, "Function call is missing a thought_signature"). Nothing in the docs we
//!      had said so, and every scripted cassette passed without it.
//!   2. Gemini's function-declaration schema is a restricted OpenAPI subset and it does not
//!      ignore unknown keywords: our tool schemas carry `additionalProperties`, so *every*
//!      tool call failed with `INVALID_ARGUMENT: Unknown name "additionalProperties"`.
//!
//! Run them:
//!
//! ```bash
//! export GEMINI_API_KEY=...        # never in a file that gets committed
//! cargo test --test live_gemini -- --ignored --test-threads=1
//! ```
//!
//! They also read `ZENITHAL_LIVE_MODEL` (default `gemini-3.1-flash-lite`) so a different
//! model can be exercised without editing the test.

use std::process::Command;

use zenithal_cli::{run, Ctx, OK};

fn key() -> Option<String> {
    std::env::var("GEMINI_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
}

fn model() -> String {
    std::env::var("ZENITHAL_LIVE_MODEL").unwrap_or_else(|_| "gemini-3.1-flash-lite".into())
}

/// A throwaway `$HOME` with a Gemini provider configured to read the key from the env.
fn home(tag: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("zenithal-live-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".zenithal")).unwrap();
    std::fs::write(
        home.join(".zenithal/config.json"),
        r#"{ "memory": { "backend": "file" },
             "providers": { "default": "gemini", "api_key_env": "GEMINI_API_KEY",
                            "max_tokens": 512, "timeout_secs": 120 } }"#,
    )
    .unwrap();
    home
}

fn invoke(home: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let (mut ctx, buffers) = Ctx::buffered(home.to_path_buf());
    let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let code = run(&argv, &mut ctx);
    (code, buffers.out(), buffers.err())
}

#[test]
#[ignore = "live: needs GEMINI_API_KEY and network"]
fn a_real_turn_reports_real_tokens_and_a_rate_card_estimate() {
    if key().is_none() {
        eprintln!("skipped: GEMINI_API_KEY is not set");
        return;
    }
    let home = home("turn");
    let (code, out, err) = invoke(
        &home,
        &[
            "run",
            "Reply with exactly: live",
            "--provider",
            "gemini",
            "--model",
            &model(),
        ],
    );
    let _ = std::fs::remove_dir_all(&home);

    assert_eq!(code, OK, "stderr:\n{err}");
    assert!(out.to_lowercase().contains("live"), "out:\n{out}");
    // The report line is the thing that makes BYOK honest: it must carry a price, not
    // "cost unknown", because the model is now in the registry.
    assert!(err.contains(" tok · "), "no usage line: {err}");
    assert!(
        err.contains('$'),
        "no rate-card estimate on the usage line: {err}"
    );
    assert!(
        err.contains("rate-card estimate"),
        "estimate must be labelled: {err}"
    );
    assert!(
        err.contains("not billing data"),
        "estimate must not imply a charge: {err}"
    );
    assert!(
        !err.contains("cost unknown"),
        "the registry lost the model: {err}"
    );
}

#[test]
#[ignore = "live: needs GEMINI_API_KEY, network and git"]
fn a_real_tool_loop_survives_the_second_turn() {
    if key().is_none() {
        eprintln!("skipped: GEMINI_API_KEY is not set");
        return;
    }
    let repo = std::env::temp_dir().join(format!("zenithal-live-repo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(&repo)
            .status()
            .expect("git");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(
        repo.join("src/auth.rs"),
        "pub fn verify(token: &str) -> bool {\n    // TODO: check the signature\n    true\n}\n",
    )
    .unwrap();
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.name=Live",
        "-c",
        "user.email=live@zenithal.invalid",
        "commit",
        "-qm",
        "init",
    ]);

    let home = home("tools");
    let (code, out, err) = invoke(
        &home,
        &[
            "agent",
            "run",
            "Read src/auth.rs and say in one short sentence what verify() does. Do not edit anything.",
            "--repo",
            repo.to_str().unwrap(),
            "--provider",
            "gemini",
            "--model",
            &model(),
        ],
    );
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&repo);

    assert_eq!(code, OK, "code={code}\nstderr:\n{err}");
    // Progress goes to stderr and the answer to stdout, so look at both.
    let transcript = format!("{err}{out}");
    // The tool call happening at all is the proof that the schema translation works, and
    // the fact that the *next* turn returned proves the signature round-tripped: without it
    // the API answers 400 and the loop dies here.
    assert!(
        transcript.contains("read_file"),
        "no tool call in the transcript — schema translation broken?\nstderr:\n{err}\nstdout:\n{out}"
    );
    assert!(!err.contains("400"), "the API rejected a turn:\n{err}");
    assert!(
        !out.trim().is_empty(),
        "the loop produced no answer:\n{err}"
    );
}
