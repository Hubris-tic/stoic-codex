//! Read-only git tools.
//!
//! The agent may *look* at git state; it may not rewrite it. Every mutating git operation
//! belongs to the orchestrator (`worktree.rs`), which runs on the user's behalf rather than
//! on the model's. An agent that can `git reset --hard` can destroy another agent's work
//! in a shared repository, and no prompt is a reliable guard against that.

use std::process::Command;
use std::sync::Arc;

use serde_json::Value;

use crate::sandbox::Sandbox;
use crate::tool::{optional_str, schema_object, Tool, ToolOutcome};

const MAX_OUTPUT: usize = 24 * 1024;

fn git(root: &std::path::Path, args: &[&str]) -> ToolOutcome {
    let output = match Command::new("git").arg("-C").arg(root).args(args).output() {
        Ok(o) => o,
        Err(e) => return ToolOutcome::err(format!("cannot run git: {e}")),
    };
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !stderr.trim().is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    if text.len() > MAX_OUTPUT {
        text.truncate(MAX_OUTPUT);
        text.push_str("\n… git output truncated");
    }
    if output.status.success() {
        ToolOutcome::ok(if text.trim().is_empty() {
            "(no output)".to_string()
        } else {
            text
        })
    } else {
        ToolOutcome::err(text)
    }
}

pub struct GitStatus {
    sandbox: Arc<Sandbox>,
}

impl GitStatus {
    pub fn new(sandbox: Arc<Sandbox>) -> Self {
        GitStatus { sandbox }
    }
}

impl Tool for GitStatus {
    fn name(&self) -> &'static str {
        "git_status"
    }

    fn description(&self) -> &'static str {
        "Show which files in this working tree are modified, added or deleted. Use it to \
         check your own progress before finishing."
    }

    fn schema(&self) -> Value {
        schema_object(&[], &[])
    }

    fn invoke(&self, _args: &Value) -> ToolOutcome {
        git(self.sandbox.root(), &["status", "--short", "--branch"])
    }
}

pub struct GitDiff {
    sandbox: Arc<Sandbox>,
}

impl GitDiff {
    pub fn new(sandbox: Arc<Sandbox>) -> Self {
        GitDiff { sandbox }
    }
}

impl Tool for GitDiff {
    fn name(&self) -> &'static str {
        "git_diff"
    }

    fn description(&self) -> &'static str {
        "Show the current working-tree changes as a unified diff. Use `path` to narrow it to \
         one file."
    }

    fn schema(&self) -> Value {
        schema_object(&[("path", "string", "Optional single file to diff")], &[])
    }

    fn invoke(&self, args: &Value) -> ToolOutcome {
        let mut argv: Vec<&str> = vec!["diff", "--no-color", "HEAD"];
        let path = optional_str(args, "path");
        if let Some(p) = path {
            argv.push("--");
            argv.push(p);
        }
        let out = git(self.sandbox.root(), &argv);
        if out.content.trim() == "(no output)" || out.content.trim().is_empty() {
            return ToolOutcome::ok("no changes in the working tree");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Repo(std::path::PathBuf);

    impl Repo {
        fn new(tag: &str) -> Repo {
            let dir = std::env::temp_dir().join(format!(
                "stoic-codex-git-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let repo = Repo(dir);
            repo.run(&["init", "-q", "-b", "main"]);
            repo.run(&["config", "user.email", "test@example.com"]);
            repo.run(&["config", "user.name", "Test"]);
            std::fs::write(repo.0.join("a.txt"), "one\n").unwrap();
            repo.run(&["add", "-A"]);
            repo.run(&["commit", "-q", "-m", "init"]);
            repo
        }
        fn run(&self, args: &[&str]) {
            let status = Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        }
        fn sandbox(&self) -> Arc<Sandbox> {
            Arc::new(Sandbox::new(&self.0).unwrap())
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn status_reports_a_modified_file() {
        let repo = Repo::new("status");
        std::fs::write(repo.0.join("a.txt"), "two\n").unwrap();
        let out = GitStatus::new(repo.sandbox()).invoke(&serde_json::json!({}));
        assert!(out.content.contains("a.txt"), "{}", out.content);
        assert!(
            out.content.contains("main"),
            "branch is reported: {}",
            out.content
        );
    }

    #[test]
    fn diff_shows_the_change_and_says_so_when_there_is_none() {
        let repo = Repo::new("diff");
        let clean = GitDiff::new(repo.sandbox()).invoke(&serde_json::json!({}));
        assert!(clean.content.contains("no changes"), "{}", clean.content);

        std::fs::write(repo.0.join("a.txt"), "two\n").unwrap();
        let dirty = GitDiff::new(repo.sandbox()).invoke(&serde_json::json!({}));
        assert!(dirty.content.contains("-one"), "{}", dirty.content);
        assert!(dirty.content.contains("+two"), "{}", dirty.content);
    }
}
