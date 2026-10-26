//! Mission tests: a real repository, real git worktrees, real parallel threads.
//!
//! Strategos and the Operator are scripted — no network, no keys — but everything else is
//! the shipping code: `git worktree` is really invoked, the Operator really run at the same
//! time in separate directories, and the accepted files really land in the base repository.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use serde_json::json;
use zenithal_agent::{
    provisional_name, run_mission, MissionConfig, MissionEvent, Model, Operator, OperatorState,
    Repo, Verdict,
};
use zenithal_providers::{CanonicalRequest, CanonicalResponse, Content, StopReason, Usage};

// ─── fixtures ─────────────────────────────────────────────────────────────────

struct TestRepo {
    dir: PathBuf,
}

impl TestRepo {
    fn new(tag: &str) -> TestRepo {
        let dir = std::env::temp_dir().join(format!(
            "zenithal-mission-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let repo = TestRepo { dir };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "test@example.com"]);
        repo.git(&["config", "user.name", "Test"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        std::fs::create_dir_all(repo.dir.join("src")).unwrap();
        std::fs::write(repo.dir.join("README.md"), "# base\n").unwrap();
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-q", "-m", "init"]);
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn repo(&self) -> Repo {
        Repo::discover(&self.dir).unwrap()
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join(rel)).ok()
    }

    fn exists(&self, rel: &str) -> bool {
        self.dir.join(rel).exists()
    }

    fn status(&self) -> String {
        self.git(&["status", "--porcelain"]).trim().to_string()
    }

    fn branches(&self) -> Vec<String> {
        self.git(&["branch", "--list", "zenithal/*"])
            .lines()
            .map(|l| l.trim_start_matches('*').trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }

    fn log_subject(&self) -> String {
        self.git(&["log", "-1", "--pretty=%s"]).trim().to_string()
    }
}

impl Drop for TestRepo {
    fn drop(&mut self) {
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(["worktree", "prune"])
            .output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ─── models ───────────────────────────────────────────────────────────────────

/// Strategos: replays fixed replies (plan, then review) and records the prompts.
struct Boss {
    replies: Mutex<Vec<String>>,
    prompts: Mutex<Vec<CanonicalRequest>>,
}

impl Boss {
    fn new(replies: Vec<String>) -> Box<dyn Model> {
        Box::new(Boss {
            replies: Mutex::new(replies),
            prompts: Mutex::new(Vec::new()),
        })
    }
    fn plan_and_review(plan: &str, review: &str) -> Box<dyn Model> {
        Boss::new(vec![plan.to_string(), review.to_string()])
    }
}

impl Model for Boss {
    fn complete(
        &self,
        request: &CanonicalRequest,
    ) -> Result<CanonicalResponse, zenithal_agent::ModelError> {
        self.prompts.lock().unwrap().push(request.clone());
        let mut replies = self.replies.lock().unwrap();
        if replies.is_empty() {
            return Err(zenithal_agent::ModelError::Refused {
                status: 500,
                detail: "no more replies".into(),
            });
        }
        Ok(CanonicalResponse {
            model: "strategos-model".into(),
            content: vec![Content::text(replies.remove(0))],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 500,
                output_tokens: 100,
                ..Usage::default()
            },
        })
    }
}

/// A Operator that writes files and then reports back.
struct Worker {
    writes: Vec<(String, String)>,
    calls: AtomicUsize,
}

impl Model for Worker {
    fn complete(
        &self,
        _request: &CanonicalRequest,
    ) -> Result<CanonicalResponse, zenithal_agent::ModelError> {
        let i = self.calls.fetch_add(1, Ordering::SeqCst);
        if i < self.writes.len() {
            let (path, content) = &self.writes[i];
            return Ok(CanonicalResponse {
                model: "worker-model".into(),
                content: vec![Content::tool_use(
                    format!("call-{i}"),
                    "write_file",
                    json!({ "path": path, "content": content }),
                )],
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 200,
                    output_tokens: 40,
                    ..Usage::default()
                },
            });
        }
        Ok(CanonicalResponse {
            model: "worker-model".into(),
            content: vec![Content::text(format!(
                "done: {} file(s)",
                self.writes.len()
            ))],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 100,
                output_tokens: 20,
                ..Usage::default()
            },
        })
    }
}

fn worker(writes: &[(&str, &str)]) -> Box<dyn Model> {
    Box::new(Worker {
        writes: writes
            .iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect(),
        calls: AtomicUsize::new(0),
    })
}

/// A Operator whose model refuses; it should fail without taking the mission down.
fn broken() -> Box<dyn Model> {
    struct Broken;
    impl Model for Broken {
        fn complete(
            &self,
            _: &CanonicalRequest,
        ) -> Result<CanonicalResponse, zenithal_agent::ModelError> {
            Err(zenithal_agent::ModelError::Refused {
                status: 500,
                detail: "provider fell over".into(),
            })
        }
    }
    Box::new(Broken)
}

fn three(workers: Vec<Box<dyn Model>>) -> Vec<Operator> {
    workers
        .into_iter()
        .enumerate()
        .map(|(i, model)| Operator::new(provisional_name(i + 1), model))
        .collect()
}

fn config() -> MissionConfig {
    let mut cfg = MissionConfig::default();
    cfg.agent.model = "worker-model".into();
    cfg
}

/// The code Strategos hands to each slot in these fixtures. Fixed rather than classified so
/// a test asserts the mission, not the keyword table.
fn code_for_slot(slot: &str) -> &'static str {
    match slot {
        "slot-1" => "x1-write",
        "slot-2" => "x2-check",
        "slot-3" => "x3-ship",
        _ => "x0-core",
    }
}

/// A plan as Strategos writes it: slots, the code it names each one, and the work.
fn plan(assignments: &[(&str, &str)]) -> String {
    let items: Vec<String> = assignments
        .iter()
        .map(|(c, t)| {
            format!(
                "{{\"codename\":\"{c}\",\"code\":\"{}\",\"task\":\"{t}\"}}",
                code_for_slot(c)
            )
        })
        .collect();
    format!("{{\"assignments\":[{}]}}", items.join(","))
}

fn review(verdicts: &[(&str, bool, &str)]) -> String {
    let items: Vec<String> = verdicts
        .iter()
        .map(|(c, accept, why)| {
            format!("{{\"codename\":\"{c}\",\"accept\":{accept},\"reason\":\"{why}\"}}")
        })
        .collect();
    format!("{{\"verdicts\":[{}]}}", items.join(","))
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[test]
fn strategos_divides_the_work_and_every_operators_lands_its_own_files() {
    let t = TestRepo::new("divide");
    let boss = Boss::plan_and_review(
        &plan(&[
            ("slot-1", "add the parser in src/parser.rs"),
            ("slot-2", "add the tests in src/parser_test.rs"),
            ("slot-3", "write the docs in docs/parser.md"),
        ]),
        &review(&[
            ("x1-write", true, "parser is correct"),
            ("x2-check", true, "tests cover it"),
            ("x3-ship", true, "docs match"),
        ]),
    );
    let operators = three(vec![
        worker(&[("src/parser.rs", "pub fn parse() {}\n")]),
        worker(&[("src/parser_test.rs", "#[test] fn parses() {}\n")]),
        worker(&[("docs/parser.md", "# parser\n")]),
    ]);

    let report = run_mission(
        &t.repo(),
        boss.as_ref(),
        operators,
        "add a parser",
        &config(),
        &|_| {},
    )
    .expect("mission should run");

    // The plan came from Strategos and gave each agent different work.
    assert_eq!(report.plan.source, zenithal_agent::PlanSource::Strategos);
    let tasks: Vec<&str> = report
        .plan
        .assignments
        .iter()
        .map(|a| a.task.as_str())
        .collect();
    assert!(tasks.iter().any(|t| t.contains("parser.rs")));
    assert!(tasks.iter().any(|t| t.contains("parser_test.rs")));
    assert!(tasks.iter().any(|t| t.contains("docs/parser.md")));

    // Each Operator did its own piece.
    assert_eq!(report.operators.len(), 3);
    for f in &report.operators {
        assert_eq!(f.state, OperatorState::Succeeded, "{f:?}");
        assert_eq!(f.files.len(), 1, "{} changed {:?}", f.codename, f.files);
        assert_eq!(f.verdict, Verdict::Accepted);
    }

    // All three files are in the base tree, in one commit, on a clean working tree.
    assert_eq!(
        t.read("src/parser.rs").as_deref(),
        Some("pub fn parse() {}\n")
    );
    assert_eq!(
        t.read("src/parser_test.rs").as_deref(),
        Some("#[test] fn parses() {}\n")
    );
    assert_eq!(t.read("docs/parser.md").as_deref(), Some("# parser\n"));
    assert_eq!(report.applied_count(), 3);
    assert!(report.commit.is_some());
    assert!(
        t.log_subject().contains("zenithal mission"),
        "{}",
        t.log_subject()
    );
    assert_eq!(t.status(), "", "the working tree must be clean afterwards");

    // Isolation held: each Operator had exactly one job and no conflicts occurred.
    assert!(report.conflicts().is_empty());
    assert_eq!(
        t.branches(),
        Vec::<String>::new(),
        "landed branches should be cleaned up"
    );
    assert!(
        !t.dir.join(".git/zenithal-missions").exists(),
        "worktrees left behind"
    );
}

#[test]
fn a_rejected_operators_changes_nothing_and_keeps_its_branch() {
    let t = TestRepo::new("reject");
    let boss = Boss::plan_and_review(
        &plan(&[
            ("slot-1", "add the feature"),
            ("slot-2", "rewrite the README"),
        ]),
        &review(&[
            ("x1-write", true, "good"),
            ("x2-check", false, "the README rewrite loses information"),
        ]),
    );
    let operators = three(vec![
        worker(&[("src/feature.rs", "pub fn feature() {}\n")]),
        worker(&[("README.md", "gone\n")]),
    ]);

    let report = run_mission(
        &t.repo(),
        boss.as_ref(),
        operators,
        "add a feature",
        &config(),
        &|_| {},
    )
    .expect("mission should run");

    assert_eq!(report.operators[0].verdict, Verdict::Accepted);
    assert_eq!(report.operators[1].verdict, Verdict::Rejected);
    assert_eq!(
        t.read("src/feature.rs").as_deref(),
        Some("pub fn feature() {}\n")
    );
    assert_eq!(
        t.read("README.md").as_deref(),
        Some("# base\n"),
        "the rejection was not honoured"
    );
    assert_eq!(report.applied_count(), 1);
    // The rejected work is not deleted: its branch holds the only copy, named after the code.
    assert!(
        t.branches().iter().any(|b| b.contains("x2-check")),
        "{:?}",
        t.branches()
    );
}

#[test]
fn two_operators_on_the_same_file_is_a_reported_conflict_not_a_silent_overwrite() {
    let t = TestRepo::new("conflict");
    let boss = Boss::plan_and_review(
        &plan(&[("slot-1", "edit config"), ("slot-2", "edit config too")]),
        &review(&[("x1-write", true, "ok"), ("x2-check", true, "also ok")]),
    );
    let operators = three(vec![
        worker(&[("config.toml", "first\n")]),
        worker(&[("config.toml", "second\n")]),
    ]);

    let report = run_mission(
        &t.repo(),
        boss.as_ref(),
        operators,
        "edit config",
        &config(),
        &|_| {},
    )
    .expect("mission should run");

    let conflicts = report.conflicts();
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert_eq!(conflicts[0].0, "config.toml");
    // First accepted writer wins, and the file is whole — not a merge of two halves.
    let content = t.read("config.toml").unwrap();
    assert!(content == "first\n" || content == "second\n", "{content:?}");
    assert_eq!(
        report.applied_count(),
        1,
        "only one Operator may own a file"
    );
}

#[test]
fn an_unreadable_plan_falls_back_and_the_mission_still_runs() {
    let t = TestRepo::new("fallback");
    let boss = Boss::plan_and_review(
        "I am not going to answer in JSON.",
        &review(&[("x1-write", true, "fine")]),
    );
    let operators = three(vec![
        worker(&[("a.txt", "a\n")]),
        worker(&[("b.txt", "b\n")]),
    ]);

    let report = run_mission(
        &t.repo(),
        boss.as_ref(),
        operators,
        "do both things",
        &config(),
        &|_| {},
    )
    .expect("a bad plan must not sink the mission");

    assert_eq!(report.plan.source, zenithal_agent::PlanSource::Fallback);
    assert_eq!(report.plan.assignments.len(), 2);
    assert!(report
        .plan
        .assignments
        .iter()
        .all(|a| a.task == "do both things"));
    assert_eq!(report.applied_count(), 2);
}

#[test]
fn a_failed_review_keeps_the_work_instead_of_throwing_it_away() {
    let t = TestRepo::new("badreview");
    let boss = Boss::plan_and_review(
        &plan(&[("slot-1", "write a file")]),
        "no JSON at all, sorry",
    );
    let operators = three(vec![worker(&[("out.txt", "kept\n")])]);

    let report = run_mission(
        &t.repo(),
        boss.as_ref(),
        operators,
        "write a file",
        &config(),
        &|_| {},
    )
    .expect("mission should run");

    assert!(
        report.review_note.contains("could not review"),
        "{}",
        report.review_note
    );
    assert_eq!(report.operators[0].verdict, Verdict::Accepted);
    assert!(report.operators[0].reason.contains("review step failed"));
    assert_eq!(t.read("out.txt").as_deref(), Some("kept\n"));
}

#[test]
fn review_can_be_switched_off_and_then_every_clean_result_lands() {
    let t = TestRepo::new("noreview");
    // Only one reply is needed: the plan. A review call would exhaust the script.
    let boss = Boss::new(vec![plan(&[
        ("slot-1", "write"),
        ("slot-2", "write other"),
    ])]);
    let operators = three(vec![
        worker(&[("one.txt", "1\n")]),
        worker(&[("two.txt", "2\n")]),
    ]);
    let cfg = MissionConfig {
        review: false,
        ..config()
    };

    let report = run_mission(
        &t.repo(),
        boss.as_ref(),
        operators,
        "write two files",
        &cfg,
        &|_| {},
    )
    .expect("mission should run");
    assert_eq!(report.applied_count(), 2);
    assert!(report.review_note.contains("review disabled"));
}

#[test]
fn a_broken_operators_does_not_stop_the_others() {
    let t = TestRepo::new("partial");
    let boss = Boss::plan_and_review(
        &plan(&[("slot-1", "will fail"), ("slot-2", "will work")]),
        &review(&[("x1-write", false, "no output"), ("x2-check", true, "good")]),
    );
    let operators = three(vec![broken(), worker(&[("ok.txt", "fine\n")])]);

    let report = run_mission(
        &t.repo(),
        boss.as_ref(),
        operators,
        "split work",
        &config(),
        &|_| {},
    )
    .expect("mission should run");

    let failed = report
        .operators
        .iter()
        .find(|f| f.codename == "x1-write")
        .unwrap();
    assert_eq!(failed.state, OperatorState::Failed);
    assert!(failed
        .error
        .as_deref()
        .unwrap_or("")
        .contains("provider fell over"));
    assert_eq!(t.read("ok.txt").as_deref(), Some("fine\n"));
}

#[test]
fn a_dirty_repository_is_refused_before_any_operators_is_sent() {
    let t = TestRepo::new("dirty");
    std::fs::write(t.dir.join("wip.txt"), "in progress").unwrap();
    let boss = Boss::new(vec![plan(&[("slot-1", "go")])]);
    let operators = three(vec![worker(&[("a.txt", "a")])]);

    let err = run_mission(
        &t.repo(),
        boss.as_ref(),
        operators,
        "go",
        &config(),
        &|_| {},
    )
    .unwrap_err();
    assert!(err.to_string().contains("uncommitted changes"), "{err}");
    assert!(!t.exists("a.txt"));
}

#[test]
fn strategos_is_told_the_repository_layout_and_the_task() {
    let t = TestRepo::new("prompt");
    let boss = Boss::plan_and_review(
        &plan(&[("x1-write", "write")]),
        &review(&[("x1-write", true, "ok")]),
    );
    let operators = three(vec![worker(&[("x.txt", "x")])]);
    run_mission(
        &t.repo(),
        boss.as_ref(),
        operators,
        "the actual task",
        &config(),
        &|_| {},
    )
    .unwrap();

    // The Boss keeps its own record only through the model trait, so re-run one call here
    // to assert on what the plan prompt contained.
    let request = zenithal_agent::strategos::strategos_plan_prompt(
        "the actual task",
        "src/ (1 entries)\nREADME.md\n",
        &[provisional_name(1), provisional_name(2)],
    );
    let system = request.system.unwrap();
    assert!(system.contains("Strategos"), "{system}");
    assert!(system.contains("slot-1"), "{system}");
    // The commander is told what the codes mean, because naming is its job.
    assert!(
        system.contains("x1-write = the primary logic generator"),
        "{system}"
    );
    assert!(
        system.contains("worktree"),
        "the model must know it is isolated: {system}"
    );
    let user = request.messages[0].content[0]
        .as_text()
        .unwrap()
        .to_string();
    assert!(user.contains("the actual task"), "{user}");
    assert!(
        user.contains("README.md"),
        "the layout must be shown: {user}"
    );
}

#[test]
fn events_describe_the_whole_mission_for_a_ui() {
    let t = TestRepo::new("events");
    let boss = Boss::plan_and_review(
        &plan(&[("slot-1", "a"), ("slot-2", "b")]),
        &review(&[("x1-write", true, "ok"), ("x2-check", true, "ok")]),
    );
    let operators = three(vec![worker(&[("a.txt", "a")]), worker(&[("b.txt", "b")])]);

    let planned = AtomicUsize::new(0);
    let deployed = AtomicUsize::new(0);
    let reviewed = AtomicUsize::new(0);
    let tool_calls = AtomicUsize::new(0);
    let applying = AtomicUsize::new(0);

    run_mission(
        &t.repo(),
        boss.as_ref(),
        operators,
        "two files",
        &config(),
        &|event| match event {
            MissionEvent::Planned { .. } => {
                planned.fetch_add(1, Ordering::SeqCst);
            }
            MissionEvent::Deployed { .. } => {
                deployed.fetch_add(1, Ordering::SeqCst);
            }
            MissionEvent::Reviewed { .. } => {
                reviewed.fetch_add(1, Ordering::SeqCst);
            }
            MissionEvent::Applying { .. } => {
                applying.fetch_add(1, Ordering::SeqCst);
            }
            MissionEvent::Agent {
                event: zenithal_agent::Event::ToolCall { .. },
                ..
            } => {
                tool_calls.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        },
    )
    .unwrap();

    assert_eq!(planned.load(Ordering::SeqCst), 1);
    assert_eq!(deployed.load(Ordering::SeqCst), 2);
    assert_eq!(reviewed.load(Ordering::SeqCst), 1);
    assert_eq!(tool_calls.load(Ordering::SeqCst), 2);
    assert_eq!(applying.load(Ordering::SeqCst), 2);
}

#[test]
fn the_report_table_names_every_operators_with_its_verdict() {
    let t = TestRepo::new("table");
    let boss = Boss::plan_and_review(
        &plan(&[("slot-1", "keep me"), ("slot-2", "drop me")]),
        &review(&[("x1-write", true, "good"), ("x2-check", false, "bad")]),
    );
    let operators = three(vec![
        worker(&[("keep.txt", "k")]),
        worker(&[("drop.txt", "d")]),
    ]);
    let report = run_mission(&t.repo(), boss.as_ref(), operators, "t", &config(), &|_| {}).unwrap();
    let table = report.table();
    assert!(table.contains("CODE"), "{table}");
    assert!(
        table.contains("x1-write") && table.contains("x2-check"),
        "{table}"
    );
    assert!(
        table.contains("accepted") && table.contains("rejected"),
        "{table}"
    );
}
