//! `zenithal mission` end to end: a real git repo, real worktrees, three Operator, one
//! Strategos — over a real loopback socket.
//!
//! The only thing faked is the model. Everything else is the shipped code: the adapter, the
//! transport, the sandbox, the tool loop, `git worktree`, the apply step, the commit.
//!
//! The scripted server answers requests in order, so these tests pin the *protocol* between
//! Strategos and the Operator: first call plans, the middle calls act, the last one reviews.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use support::{openai_json, openai_tool_call, Stub};
use zenithal_cli::{run, Buffers, Ctx, FAILED, OK, USAGE};

// ── a repository on disk ────────────────────────────────────────────────────────────────

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(tag: &str) -> Repo {
        let root =
            std::env::temp_dir().join(format!("zenithal-mission-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let repo = Repo { root };
        repo.git(&["init", "-q", "-b", "main"]);
        // Deliberately no `user.name` / `user.email`: a fresh machine has none, and the
        // orchestrator must still be able to commit its own work.
        repo.write("README.md", "# fixture\n");
        repo.git(&["add", "-A"]);
        repo.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@zenithal.invalid",
            "commit",
            "-qm",
            "initial",
        ]);
        repo
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap_or_default()
    }

    fn exists(&self, rel: &str) -> bool {
        self.root.join(rel).exists()
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .output()
            .expect("git must be installed");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn log(&self) -> Vec<String> {
        self.git(&["log", "--format=%s"])
            .lines()
            .map(str::to_string)
            .collect()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        if std::env::var("ZENITHAL_TEST_KEEP").is_ok() {
            eprintln!("kept {}", self.root.display());
            return;
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// ── a home with a provider pointing at the stub ─────────────────────────────────────────

struct Rig {
    home: PathBuf,
    ctx: Ctx,
    buffers: Buffers,
}

impl Rig {
    fn new(tag: &str, base_url: &str) -> Rig {
        let home = std::env::temp_dir().join(format!(
            "zenithal-mission-home-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        let dir = home.join(".zenithal");
        std::fs::create_dir_all(&dir).unwrap();
        let config = format!(
            r#"{{
              "memory": {{ "backend": "file" }},
              "providers": {{
                "default": "openai-chat",
                "model": "test-model",
                "base_url": "{base_url}",
                "api_key_env": "ZENITHAL_MISSION_TEST_KEY",
                "max_tokens": 512
              }}
            }}"#
        );
        std::fs::write(dir.join("config.json"), config).unwrap();
        std::env::set_var("ZENITHAL_MISSION_TEST_KEY", "sk-test-not-a-real-key");
        let (ctx, buffers) = Ctx::buffered(home.clone());
        Rig { home, ctx, buffers }
    }

    fn invoke(&mut self, args: &[&str]) -> i32 {
        self.buffers.clear();
        let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        run(&argv, &mut self.ctx)
    }

    fn out(&self) -> String {
        self.buffers.out()
    }

    fn err(&self) -> String {
        self.buffers.err()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

// ── the script spokes ──────────────────────────────────────────────────────────────────

fn plan(codename: &str, task: &str) -> String {
    format!(r#"{{"assignments":[{{"codename":"{codename}","task":"{task}"}}]}}"#)
}

/// Strategos' plan for three operators, as the model would write it in prose around JSON.
fn three_way_plan() -> Vec<u8> {
    let body = format!(
        "Here is the split.\n\n{}\n",
        serde_json::json!({
            "assignments": [
                {"codename": "slot-1", "code": "x1-write", "task": "create alpha.txt containing the word alpha"},
                {"codename": "slot-2", "code": "x2-check", "task": "create beta.txt containing the word beta"},
                {"codename": "slot-3", "code": "x3-ship", "task": "create gamma.txt containing the word gamma"},
            ]
        })
    );
    openai_json(&body)
}

fn review(accept: &[(&str, bool)], reason: &str) -> Vec<u8> {
    let verdicts: Vec<serde_json::Value> = accept
        .iter()
        .map(|(codename, ok)| {
            serde_json::json!({ "codename": codename, "accept": ok, "reason": reason })
        })
        .collect();
    openai_json(&serde_json::json!({ "verdicts": verdicts }).to_string())
}

fn write(path: &str, body: &str) -> Vec<u8> {
    openai_tool_call(
        "write_file",
        &serde_json::json!({ "path": path, "content": body }),
    )
}

fn done(text: &str) -> Vec<u8> {
    openai_json(text)
}

/// The reply a Operator sends when it decides it has nothing to change.
fn nothing() -> Vec<u8> {
    openai_json("Nothing to do here.")
}

// ── tests ────────────────────────────────────────────────────────────────────────────────

#[test]
fn three_operators_land_three_files_and_one_commit() {
    let repo = Repo::new("three-way");
    let stub = Stub::script(
        "three-way",
        vec![
            three_way_plan(),
            // x1-write
            write("alpha.txt", "alpha\n"),
            done("Wrote alpha.txt."),
            // x2-check
            write("beta.txt", "beta\n"),
            done("Wrote beta.txt."),
            // x3-ship
            write("gamma.txt", "gamma\n"),
            done("Wrote gamma.txt."),
            // Strategos reviews
            review(
                &[("x1-write", true), ("x2-check", true), ("x3-ship", true)],
                "as assigned",
            ),
        ],
    );
    let mut rig = Rig::new("three-way", &stub.base_url());

    let code = rig.invoke(&[
        "mission",
        "run",
        "add one file per code",
        "--repo",
        repo.root.to_str().unwrap(),
        "--parallel",
        "1",
    ]);

    assert_eq!(code, OK, "stderr:\n{}", rig.err());
    assert_eq!(repo.read("alpha.txt"), "alpha\n");
    assert_eq!(repo.read("beta.txt"), "beta\n");
    assert_eq!(repo.read("gamma.txt"), "gamma\n");

    // One commit for the whole mission, on top of the fixture's initial commit.
    let log = repo.log();
    assert_eq!(log.len(), 2, "expected one mission commit, got {log:?}");
    assert!(log[0].starts_with("zenithal mission:"), "{}", log[0]);
    assert!(log[0].contains("3 file"), "{}", log[0]);
    assert!(repo.git(&["status", "--porcelain"]).trim().is_empty());

    // The report names every Operator, and the working tree is left clean of worktrees.
    let out = rig.out();
    for codename in ["x1-write", "x2-check", "x3-ship"] {
        assert!(out.contains(codename), "report omitted {codename}:\n{out}");
    }
    assert!(out.contains("Strategos"), "{out}");
    assert!(
        !repo.exists(".git/zenithal-missions"),
        "worktrees were left behind"
    );

    // Every branch that landed is gone; none of the temporary ones linger.
    let branches = repo.git(&["branch", "--list", "zenithal/*"]);
    assert!(
        branches.trim().is_empty(),
        "branches left behind: {branches}"
    );
}

#[test]
fn the_plan_reaches_each_operators_and_the_work_does_not_overlap() {
    let repo = Repo::new("split");
    let stub = Stub::script(
        "split",
        vec![
            three_way_plan(),
            write("alpha.txt", "alpha\n"),
            done("done"),
            write("beta.txt", "beta\n"),
            done("done"),
            write("gamma.txt", "gamma\n"),
            done("done"),
            review(
                &[("x1-write", true), ("x2-check", true), ("x3-ship", true)],
                "ok",
            ),
        ],
    );
    let mut rig = Rig::new("split", &stub.base_url());

    let code = rig.invoke(&[
        "mission",
        "run",
        "add one file per code",
        "--repo",
        repo.root.to_str().unwrap(),
        "--parallel",
        "1",
    ]);
    assert_eq!(code, OK, "stderr:\n{}", rig.err());

    let requests = stub.join();
    assert_eq!(requests.len(), 8, "one plan, six turns, one review");

    // Request 0 is the plan: it must carry the task and the repository layout.
    assert!(
        requests[0].contains("add one file per code"),
        "{}",
        requests[0]
    );
    assert!(
        requests[0].contains("README.md"),
        "the layout must be shown: {}",
        requests[0]
    );

    // Each Operator is told its own assignment, and only its own.
    let bodies: Vec<&String> = requests.iter().collect();
    let f1 = bodies
        .iter()
        .find(|b| b.contains("x1-write") && b.contains("alpha.txt"));
    assert!(f1.is_some(), "x1-write was never told to write alpha.txt");
    let f1 = f1.unwrap();
    assert!(
        !f1.contains("beta.txt"),
        "x1-write was told about another agent's file"
    );

    // The review is the last call and sees all three reports.
    let last = bodies.last().unwrap();
    for codename in ["x1-write", "x2-check", "x3-ship"] {
        assert!(
            last.contains(codename),
            "the review was not shown {codename}"
        );
    }
}

#[test]
fn a_rejected_operators_keeps_its_work_on_its_branch_and_out_of_the_tree() {
    let repo = Repo::new("reject");
    let stub = Stub::script(
        "reject",
        vec![
            three_way_plan(),
            write("alpha.txt", "alpha\n"),
            done("done"),
            write("beta.txt", "beta\n"),
            done("done"),
            write("gamma.txt", "gamma\n"),
            done("done"),
            review(
                &[("x1-write", true), ("x2-check", false), ("x3-ship", true)],
                "x2-check invented a file nobody asked for",
            ),
        ],
    );
    let mut rig = Rig::new("reject", &stub.base_url());

    let code = rig.invoke(&[
        "mission",
        "run",
        "add one file per code",
        "--repo",
        repo.root.to_str().unwrap(),
        "--parallel",
        "1",
    ]);
    assert_eq!(code, OK, "stderr:\n{}", rig.err());

    assert!(repo.exists("alpha.txt"));
    assert!(
        !repo.exists("beta.txt"),
        "a rejected change reached the working tree"
    );
    assert!(repo.exists("gamma.txt"));

    // Rejected work is not lost: it stays on its branch, and only that branch survives —
    // the two that landed were already committed to the working tree.
    let branches = repo.git(&["branch", "--list", "zenithal/*"]);
    let survivors: Vec<&str> = branches
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        survivors,
        vec!["zenithal/x2-check"],
        "only the rejected Operator's branch should survive: {branches}"
    );
    assert!(repo
        .git(&["show", "zenithal/x2-check:beta.txt"])
        .contains("beta"));

    assert!(rig.out().contains("rejected"), "{}", rig.out());
}

#[test]
fn two_operators_on_one_file_is_a_conflict_and_the_first_accepted_writer_wins() {
    let repo = Repo::new("conflict");
    // Both are told to write the same file — a plan that overlaps on purpose.
    let overlapping = openai_json(
        &serde_json::json!({
            "assignments": [
                {"codename": "slot-1", "code": "x1-write", "task": "write shared.txt with one"},
                {"codename": "slot-2", "code": "x2-check", "task": "write shared.txt with two"},
                {"codename": "slot-3", "code": "x3-ship", "task": "write own.txt with three"},
            ]
        })
        .to_string(),
    );
    let stub = Stub::script(
        "conflict",
        vec![
            overlapping,
            write("shared.txt", "one\n"),
            done("done"),
            write("shared.txt", "two\n"),
            done("done"),
            write("own.txt", "three\n"),
            done("done"),
            review(
                &[("x1-write", true), ("x2-check", true), ("x3-ship", true)],
                "ok",
            ),
        ],
    );
    let mut rig = Rig::new("conflict", &stub.base_url());

    let code = rig.invoke(&[
        "mission",
        "run",
        "write a shared file",
        "--repo",
        repo.root.to_str().unwrap(),
        "--parallel",
        "1",
    ]);
    assert_eq!(code, OK, "stderr:\n{}", rig.err());

    // First writer wins, whole file, and the loser is reported rather than silently merged.
    assert_eq!(repo.read("shared.txt"), "one\n");
    assert_eq!(repo.read("own.txt"), "three\n");
    let out = rig.out();
    assert!(out.contains("conflict"), "{out}");
    assert!(out.contains("shared.txt"), "{out}");
    assert!(out.contains("x2-check"), "{out}");
}

#[test]
fn a_mission_where_nothing_lands_exits_with_failure() {
    let repo = Repo::new("empty");
    let stub = Stub::script(
        "empty",
        vec![
            three_way_plan(),
            nothing(),
            nothing(),
            nothing(),
            review(
                &[("x1-write", true), ("x2-check", true), ("x3-ship", true)],
                "ok",
            ),
        ],
    );
    let mut rig = Rig::new("empty", &stub.base_url());

    let code = rig.invoke(&[
        "mission",
        "run",
        "add one file per code",
        "--repo",
        repo.root.to_str().unwrap(),
        "--parallel",
        "1",
    ]);
    assert_eq!(code, FAILED, "out:\n{}\nerr:\n{}", rig.out(), rig.err());
    assert!(
        rig.err()
            .contains("no operator of The Phalanx produced an applicable change"),
        "{}",
        rig.err()
    );
    // Nothing was committed, so the repository is untouched apart from the fixture commit.
    assert_eq!(repo.log().len(), 1);
}

#[test]
fn a_operators_that_fails_does_not_take_the_mission_down() {
    let repo = Repo::new("one-fails");
    let stub = Stub::script(
        "one-fails",
        vec![
            three_way_plan(),
            write("alpha.txt", "alpha\n"),
            done("done"),
            // x2-check asks for a tool that does not exist: a broken turn, not a crash.
            openai_tool_call("rm_rf_everything", &serde_json::json!({})),
            done("I could not do it."),
            write("gamma.txt", "gamma\n"),
            done("done"),
            review(
                &[("x1-write", true), ("x2-check", false), ("x3-ship", true)],
                "partial",
            ),
        ],
    );
    let mut rig = Rig::new("one-fails", &stub.base_url());

    let code = rig.invoke(&[
        "mission",
        "run",
        "add one file per code",
        "--repo",
        repo.root.to_str().unwrap(),
        "--parallel",
        "1",
    ]);
    assert_eq!(code, OK, "out:\n{}\nerr:\n{}", rig.out(), rig.err());
    assert!(repo.exists("alpha.txt"));
    assert!(repo.exists("gamma.txt"));

    let out = rig.out();
    assert!(out.contains("x2-check"), "{out}");
    // The unknown tool is answered, not fatal — the loop reports it back to the model.
    let requests = stub.join();
    assert!(
        requests.iter().any(|r| r.contains("rm_rf_everything")),
        "the failed tool call was never reported back"
    );
}

#[test]
fn mission_plan_shows_the_assignments_and_changes_nothing() {
    let repo = Repo::new("plan-only");
    let stub = Stub::script("plan-only", vec![three_way_plan()]);
    let mut rig = Rig::new("plan-only", &stub.base_url());

    let code = rig.invoke(&[
        "mission",
        "plan",
        "add one file per code",
        "--repo",
        repo.root.to_str().unwrap(),
    ]);
    assert_eq!(code, OK, "stderr:\n{}", rig.err());

    let out = rig.out();
    // The preview names the Phalanx exactly as a run would, role and all.
    assert!(out.contains("[strategos]"), "{out}");
    assert!(out.contains("slot-1"), "the slot it replaced: {out}");
    assert!(out.contains("x1-write"), "{out}");
    assert!(out.contains("the primary logic generator"), "{out}");
    assert!(out.contains("alpha.txt"), "{out}");

    // One call, no worktrees, no commits.
    assert_eq!(stub.join().len(), 1);
    assert_eq!(repo.log().len(), 1);
    assert!(!repo.exists(".git/zenithal-missions"));
}

#[test]
fn mission_plan_json_is_machine_readable() {
    let repo = Repo::new("plan-json");
    let stub = Stub::script("plan-json", vec![three_way_plan()]);
    let mut rig = Rig::new("plan-json", &stub.base_url());

    let code = rig.invoke(&[
        "mission",
        "plan",
        "add one file per code",
        "--repo",
        repo.root.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, OK, "stderr:\n{}", rig.err());

    let value: serde_json::Value = serde_json::from_str(rig.out().trim()).expect("valid JSON");
    assert_eq!(value["source"], "strategos");
    let assignments = value["assignments"].as_array().unwrap();
    assert_eq!(assignments.len(), 3);
    assert_eq!(assignments[0]["code"], "x1-write");
    assert_eq!(value["phalanx"], "The Phalanx");
    assert_eq!(value["commander"], "Strategos");
}

#[test]
fn mission_run_json_reports_every_operators_and_the_commit() {
    let repo = Repo::new("run-json");
    let stub = Stub::script(
        "run-json",
        vec![
            three_way_plan(),
            write("alpha.txt", "alpha\n"),
            done("done"),
            write("beta.txt", "beta\n"),
            done("done"),
            write("gamma.txt", "gamma\n"),
            done("done"),
            review(
                &[("x1-write", true), ("x2-check", true), ("x3-ship", true)],
                "ok",
            ),
        ],
    );
    let mut rig = Rig::new("run-json", &stub.base_url());

    let code = rig.invoke(&[
        "mission",
        "run",
        "add one file per code",
        "--repo",
        repo.root.to_str().unwrap(),
        "--parallel",
        "1",
        "--json",
    ]);
    assert_eq!(code, OK, "stderr:\n{}", rig.err());

    let value: serde_json::Value = serde_json::from_str(rig.out().trim()).expect("valid JSON");
    assert_eq!(value["strategos"], "Strategos");
    assert_eq!(value["applied"], 3);
    assert!(value["commit"].is_string(), "{}", value["commit"]);
    let members = value["operators"].as_array().unwrap();
    assert_eq!(members.len(), 3);
    for member in members {
        assert_eq!(member["state"], "done");
        assert_eq!(member["verdict"], "accepted");
        assert!(member["files"].as_array().unwrap().len() == 1);
    }
    assert_eq!(value["conflicts"].as_array().unwrap().len(), 0);
    // --json keeps stderr quiet, so the JSON is the whole output.
    assert!(rig.err().is_empty(), "{}", rig.err());
}

#[test]
fn an_unreadable_plan_falls_back_to_giving_everyone_the_whole_task() {
    let repo = Repo::new("fallback");
    let stub = Stub::script(
        "fallback",
        vec![
            // No JSON at all: Strategos answered in prose it did not structure.
            openai_json("I think you should all just do the task, honestly."),
            write("alpha.txt", "alpha\n"),
            done("done"),
            write("beta.txt", "beta\n"),
            done("done"),
            write("gamma.txt", "gamma\n"),
            done("done"),
            review(
                &[("x1-write", true), ("x2-check", true), ("x3-ship", true)],
                "ok",
            ),
        ],
    );
    let mut rig = Rig::new("fallback", &stub.base_url());

    let code = rig.invoke(&[
        "mission",
        "run",
        "add one file per code",
        "--repo",
        repo.root.to_str().unwrap(),
        "--parallel",
        "1",
    ]);
    assert_eq!(code, OK, "out:\n{}\nerr:\n{}", rig.out(), rig.err());

    // Degraded, not fatal: the work still landed and the report says the plan was a fallback.
    assert!(repo.exists("alpha.txt") && repo.exists("gamma.txt"));
    let out = rig.out();
    assert!(out.to_lowercase().contains("fallback"), "{out}");
}

#[test]
fn no_review_is_allowed_and_reports_every_clean_result_as_accepted() {
    let repo = Repo::new("no-review");
    let stub = Stub::script(
        "no-review",
        vec![
            three_way_plan(),
            write("alpha.txt", "alpha\n"),
            done("done"),
            write("beta.txt", "beta\n"),
            done("done"),
            write("gamma.txt", "gamma\n"),
            done("done"),
        ],
    );
    let mut rig = Rig::new("no-review", &stub.base_url());

    let code = rig.invoke(&[
        "mission",
        "run",
        "add one file per code",
        "--repo",
        repo.root.to_str().unwrap(),
        "--parallel",
        "1",
        "--no-review",
    ]);
    assert_eq!(code, OK, "stderr:\n{}", rig.err());
    assert!(repo.exists("alpha.txt") && repo.exists("beta.txt") && repo.exists("gamma.txt"));
    assert!(rig.out().contains("review disabled"), "{}", rig.out());
    assert_eq!(stub.join().len(), 7, "no review call should have been made");
}

#[test]
fn a_verify_command_that_fails_rejects_the_work() {
    let repo = Repo::new("verify");
    let stub = Stub::script(
        "verify",
        vec![
            three_way_plan(),
            write("alpha.txt", "alpha\n"),
            done("done"),
            write("beta.txt", "beta\n"),
            done("done"),
            write("gamma.txt", "gamma\n"),
            done("done"),
            // Strategos is willing to accept everything.
            review(
                &[("x1-write", true), ("x2-check", true), ("x3-ship", true)],
                "ok",
            ),
        ],
    );
    let mut rig = Rig::new("verify", &stub.base_url());

    // A command that always fails: `false` exists everywhere a POSIX shell does.
    let code = rig.invoke(&[
        "mission",
        "run",
        "add one file per code",
        "--repo",
        repo.root.to_str().unwrap(),
        "--parallel",
        "1",
        "--verify",
        "false",
    ]);

    // Everything failed verification, so nothing may be applied and the mission fails.
    assert_eq!(code, FAILED, "out:\n{}\nerr:\n{}", rig.out(), rig.err());
    assert!(!repo.exists("alpha.txt"));
    assert!(!repo.exists("gamma.txt"));
    assert_eq!(repo.log().len(), 1);
}

#[test]
fn a_missing_provider_is_a_usage_error_before_anything_is_created() {
    let repo = Repo::new("no-provider");
    // A home with no config at all.
    let home = std::env::temp_dir().join(format!("zenithal-mission-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let (mut ctx, buffers) = Ctx::buffered(home.clone());

    let argv: Vec<String> = ["mission", "run", "x", "--repo", repo.root.to_str().unwrap()]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let code = run(&argv, &mut ctx);

    assert_eq!(code, USAGE, "{}", buffers.err());
    assert!(!repo.exists(".git/zenithal-missions"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn mission_without_a_task_explains_itself() {
    let (mut ctx, buffers) = Ctx::buffered(std::env::temp_dir().join("zenithal-mission-usage"));
    let code = run(&["mission".into(), "run".into()], &mut ctx);
    assert_eq!(code, USAGE);
    assert!(
        buffers.err().contains("usage: zenithal mission run"),
        "{}",
        buffers.err()
    );
}

#[test]
fn the_help_text_describes_the_mission_shape() {
    let (mut ctx, buffers) = Ctx::buffered(std::env::temp_dir().join("zenithal-mission-help"));
    let code = run(&["--help".into()], &mut ctx);
    assert_eq!(code, OK);
    let help = buffers.out();
    assert!(help.contains("Strategos"), "the boss must be named: {help}");
    assert!(
        help.contains("Operator"),
        "the sub-agents must be named: {help}"
    );
    assert!(help.contains("mission run"), "{help}");
}

// Keep the unused-helper lints honest: `plan` builds a one-member plan for ad-hoc tests.
#[allow(dead_code)]
fn one_member_plan(codename: &str, task: &str) -> String {
    plan(codename, task)
}

#[allow(dead_code)]
fn path_of(repo: &Repo, rel: &str) -> PathBuf {
    repo.root.join(rel)
}

#[allow(dead_code)]
fn is_file(p: &Path) -> bool {
    p.is_file()
}
