//! Git worktree management — the isolation that makes concurrent agents safe.
//!
//! Why worktrees rather than branches or clones:
//!
//! * **branches alone are not isolation.** Two agents on two branches but one working
//!   directory overwrite each other's files on every save. The isolation must include the
//!   *files*.
//! * **clones are too expensive.** A worktree shares the object database and costs a
//!   checkout; a clone copies everything, and a squad of five clones a repo five times.
//! * **worktrees are exactly the right unit**: one directory, one branch, one index, all
//!   backed by the same `.git`. `git` itself refuses to check out the same branch twice,
//!   which is a free guard against two agents sharing a branch by accident.
//!
//! Everything here shells out to `git`. That is deliberate: the alternative is a git
//! library, and the user's repository is not the place to be clever.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    NotARepository { path: String },
    Command { args: String, detail: String },
    Dirty { path: String },
    NoCommits { path: String },
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitError::NotARepository { path } => {
                write!(f, "{path} is not inside a git repository")
            }
            GitError::Command { args, detail } => write!(f, "git {args} failed: {detail}"),
            GitError::Dirty { path } => write!(
                f,
                "{path} has uncommitted changes; commit or stash them before starting a squad \
                 (the integrator applies results to this working tree)"
            ),
            GitError::NoCommits { path } => write!(
                f,
                "{path} has no commits yet; make an initial commit so worktrees have a base"
            ),
        }
    }
}

impl std::error::Error for GitError {}

/// A repository root, discovered once and then used for every git call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    root: PathBuf,
}

impl Repo {
    pub fn discover(start: impl AsRef<Path>) -> Result<Repo, GitError> {
        let start = start.as_ref();
        let output = Command::new("git")
            .arg("-C")
            .arg(start)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .map_err(|e| GitError::Command {
                args: "rev-parse".into(),
                detail: e.to_string(),
            })?;
        if !output.status.success() {
            return Err(GitError::NotARepository {
                path: start.display().to_string(),
            });
        }
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() {
            return Err(GitError::NotARepository {
                path: start.display().to_string(),
            });
        }
        Ok(Repo {
            root: PathBuf::from(text),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn git(&self, args: &[&str]) -> Result<String, GitError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .map_err(|e| GitError::Command {
                args: args.join(" "),
                detail: e.to_string(),
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(GitError::Command {
                args: args.join(" "),
                detail: if stderr.trim().is_empty() {
                    stdout
                } else {
                    stderr
                },
            });
        }
        Ok(stdout)
    }

    /// Stage everything and commit it. `None` when there was nothing to commit.
    ///
    /// Used for the mission's own commit, on top of which the Operator's files were copied.
    pub fn commit_all(&self, message: &str) -> Result<Option<String>, GitError> {
        let dirty = !self.git(&["status", "--porcelain"])?.trim().is_empty();
        if !dirty {
            return Ok(None);
        }
        self.git(&["add", "-A"])?;
        let mut args = identity_flags(&self.root);
        args.extend(
            ["commit", "-q", "-m", message]
                .iter()
                .map(|s| s.to_string()),
        );
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        self.git(&borrowed)?;
        let sha = self.git(&["rev-parse", "HEAD"])?.trim().to_string();
        Ok(Some(sha))
    }

    /// Current branch name, or a short HEAD sha when detached.
    pub fn head(&self) -> Result<String, GitError> {
        if let Ok(name) = self.git(&["symbolic-ref", "--short", "-q", "HEAD"]) {
            let name = name.trim().to_string();
            if !name.is_empty() {
                return Ok(name);
            }
        }
        Ok(self
            .git(&["rev-parse", "--short", "HEAD"])?
            .trim()
            .to_string())
    }

    pub fn has_commits(&self) -> bool {
        self.git(&["rev-parse", "--verify", "HEAD"]).is_ok()
    }

    pub fn is_clean(&self) -> Result<bool, GitError> {
        let status = self.git(&["status", "--porcelain"])?;
        Ok(status.trim().is_empty())
    }

    /// Create an isolated branch + working directory for one agent.
    ///
    /// `dir` is where the checkout goes; the caller owns it (a squad puts them under
    /// `.stoic-codex/worktrees/`). The branch is `stoic-codex/<name>`, namespaced so a user can see
    /// at a glance which branches the tool made and delete them all with one glob.
    pub fn create_worktree(
        &self,
        name: &str,
        dir: &Path,
        base: Option<&str>,
    ) -> Result<Worktree, GitError> {
        if !self.has_commits() {
            return Err(GitError::NoCommits {
                path: self.root.display().to_string(),
            });
        }
        let slug = slugify(name);
        let branch = unique_branch(self, &format!("stoic-codex/{slug}"));
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| GitError::Command {
                args: "create worktree dir".into(),
                detail: e.to_string(),
            })?;
        }
        let dir_str = dir.to_string_lossy().to_string();
        let branch_str = branch.clone();
        let base_ref = base.unwrap_or("HEAD").to_string();
        self.git(&["worktree", "add", "-b", &branch_str, &dir_str, &base_ref])?;
        Ok(Worktree {
            path: dir.to_path_buf(),
            branch,
            repo_root: self.root.clone(),
        })
    }

    /// Every worktree git knows about, including the main one.
    pub fn worktrees(&self) -> Result<Vec<PathBuf>, GitError> {
        let text = self.git(&["worktree", "list", "--porcelain"])?;
        Ok(text
            .lines()
            .filter_map(|l| l.strip_prefix("worktree "))
            .map(PathBuf::from)
            .collect())
    }

    /// Remove a branch once its content is merged, so a squad run does not leave a pile of
    /// dead branches behind.
    pub fn delete_branch(&self, branch: &str) -> Result<(), GitError> {
        self.git(&["branch", "-D", branch])?;
        Ok(())
    }
}

fn unique_branch(repo: &Repo, wanted: &str) -> String {
    if repo
        .git(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{wanted}"),
        ])
        .is_err()
    {
        return wanted.to_string();
    }
    for n in 2..1000 {
        let candidate = format!("{wanted}-{n}");
        if repo
            .git(&[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{candidate}"),
            ])
            .is_err()
        {
            return candidate;
        }
    }
    format!("{wanted}-{}", std::process::id())
}

fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "agent".to_string()
    } else {
        trimmed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    path: PathBuf,
    branch: String,
    repo_root: PathBuf,
}

impl Worktree {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn branch(&self) -> &str {
        &self.branch
    }

    pub fn git(&self, args: &[&str]) -> Result<String, GitError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(args)
            .output()
            .map_err(|e| GitError::Command {
                args: args.join(" "),
                detail: e.to_string(),
            })?;
        if !output.status.success() {
            return Err(GitError::Command {
                args: args.join(" "),
                detail: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Stage everything and return the names of the files that changed, with their status.
    ///
    /// Staging is the only way to get untracked files into a diff, and doing it inside an
    /// isolated worktree costs nothing: nobody else shares this index.
    pub fn stage_all(&self) -> Result<Vec<FileChange>, GitError> {
        self.git(&["add", "-A"])?;
        let text = self.git(&["diff", "--cached", "--name-status"])?;
        Ok(parse_name_status(&text))
    }

    /// Unified diff of everything this agent produced, staged or not.
    pub fn patch(&self) -> Result<String, GitError> {
        self.stage_all()?;
        self.git(&["diff", "--cached", "--no-color", "--patch"])
    }

    pub fn changed_files(&self) -> Result<Vec<FileChange>, GitError> {
        self.stage_all()
    }

    /// Commit the agent's work on its own branch. Returns `None` when it changed nothing —
    /// an agent that only read files should not produce an empty commit.
    pub fn commit_all(&self, message: &str) -> Result<Option<String>, GitError> {
        let changes = self.stage_all()?;
        if changes.is_empty() {
            return Ok(None);
        }
        let mut args = identity_flags(&self.path);
        args.extend(
            ["commit", "-q", "-m", message]
                .iter()
                .map(|s| s.to_string()),
        );
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        self.git(&borrowed)?;
        let sha = self.git(&["rev-parse", "HEAD"])?.trim().to_string();
        Ok(Some(sha))
    }

    /// Hand a file's content from this worktree to another checkout.
    ///
    /// Used by the integrator instead of `git merge`: a file-level application is
    /// deterministic, cannot leave conflict markers in the user's tree, and — crucially —
    /// makes "the winner's version of this file" an explicit decision rather than an
    /// emergent property of merge order.
    pub fn copy_file_to(&self, rel: &str, destination: &Path) -> Result<(), GitError> {
        let source = self.path.join(rel);
        let target = destination.join(rel);
        if !source.exists() {
            // Deletion: the agent removed the file, so the application removes it too.
            if target.exists() {
                std::fs::remove_file(&target).map_err(|e| GitError::Command {
                    args: format!("rm {rel}"),
                    detail: e.to_string(),
                })?;
            }
            return Ok(());
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| GitError::Command {
                args: format!("mkdir {rel}"),
                detail: e.to_string(),
            })?;
        }
        std::fs::copy(&source, &target).map_err(|e| GitError::Command {
            args: format!("copy {rel}"),
            detail: e.to_string(),
        })?;
        Ok(())
    }

    /// Remove the worktree directory and forget it. The branch stays until the caller
    /// decides what to do with it.
    pub fn remove(self) -> Result<(), GitError> {
        let repo = Repo {
            root: self.repo_root,
        };
        let path = self.path.to_string_lossy().to_string();
        repo.git(&["worktree", "remove", "--force", &path])?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// `A`, `M`, `D`, `R100`, … as git reports it.
    pub status: String,
    pub path: String,
}

impl FileChange {
    pub fn is_delete(&self) -> bool {
        self.status.starts_with('D')
    }
}

fn parse_name_status(text: &str) -> Vec<FileChange> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.split('\t');
        let Some(status) = parts.next() else { continue };
        // Renames report `R100\told\tnew`; the new path is the one that matters.
        let path = parts.last().unwrap_or("").trim().to_string();
        if path.is_empty() {
            continue;
        }
        out.push(FileChange {
            status: status.trim().to_string(),
            path,
        });
    }
    out
}

/// `git -c user.name=… -c user.email=…` flags, or nothing when an identity is configured.
///
/// A fresh clone on a fresh machine has no `user.name`/`user.email` and git refuses to
/// commit. Losing a whole mission at its last step because of that would be absurd, so the
/// orchestrator commits as `Stoic Codex <stoic-codex@localhost>` — but only when nothing is
/// configured, so a user who *has* an identity never sees our name in their history.
fn identity_flags(dir: &Path) -> Vec<String> {
    let configured = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["config", "user.email"])
        .output()
        .map(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false);
    if configured {
        Vec::new()
    } else {
        [
            "-c",
            "user.name=Stoic Codex",
            "-c",
            "user.email=stoic-codex@localhost",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) struct TestRepo {
        pub dir: PathBuf,
    }

    impl TestRepo {
        pub fn new(tag: &str) -> TestRepo {
            let dir = std::env::temp_dir().join(format!(
                "stoic-codex-wt-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let repo = TestRepo { dir };
            repo.run(&["init", "-q", "-b", "main"]);
            repo.run(&["config", "user.email", "test@example.com"]);
            repo.run(&["config", "user.name", "Test"]);
            repo.run(&["config", "commit.gpgsign", "false"]);
            std::fs::write(repo.dir.join("README.md"), "hello\n").unwrap();
            repo.run(&["add", "-A"]);
            repo.run(&["commit", "-q", "-m", "init"]);
            repo
        }

        pub fn run(&self, args: &[&str]) {
            let status = Command::new("git")
                .arg("-C")
                .arg(&self.dir)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        }

        pub fn repo(&self) -> Repo {
            Repo::discover(&self.dir).unwrap()
        }
    }

    impl Drop for TestRepo {
        fn drop(&mut self) {
            // Remove worktrees first, or their directories linger as read-only junk.
            let _ = Command::new("git")
                .arg("-C")
                .arg(&self.dir)
                .args(["worktree", "prune"])
                .status();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn discovering_a_repository_finds_the_toplevel() {
        let t = TestRepo::new("discover");
        let repo = Repo::discover(&t.dir).unwrap();
        assert_eq!(repo.root(), t.dir.canonicalize().unwrap().as_path());
        assert_eq!(repo.head().unwrap(), "main");
        assert!(repo.is_clean().unwrap());
    }

    #[test]
    fn a_directory_outside_a_repository_is_named_plainly() {
        let dir = std::env::temp_dir().join(format!("stoic-codex-norepo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = Repo::discover(&dir).unwrap_err();
        assert!(
            err.to_string().contains("not inside a git repository"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_agents_get_two_directories_and_two_branches() {
        let t = TestRepo::new("two");
        let repo = t.repo();
        let a = repo
            .create_worktree("alpha", &t.dir.join(".git/wt/a"), None)
            .unwrap();
        let b = repo
            .create_worktree("beta", &t.dir.join(".git/wt/b"), None)
            .unwrap();

        assert_ne!(a.path(), b.path());
        assert_eq!(a.branch(), "stoic-codex/alpha");
        assert_eq!(b.branch(), "stoic-codex/beta");

        // The isolation property that matters: a write in one is invisible in the other.
        std::fs::write(a.path().join("only-a.txt"), "a").unwrap();
        assert!(a.path().join("only-a.txt").exists());
        assert!(
            !b.path().join("only-a.txt").exists(),
            "worktrees share the filesystem"
        );

        a.remove().unwrap();
        b.remove().unwrap();
    }

    #[test]
    fn the_main_working_tree_is_untouched_until_the_integrator_applies_a_result() {
        let t = TestRepo::new("isolated");
        let repo = t.repo();
        let wt = repo
            .create_worktree("solo", &t.dir.join(".git/wt/solo"), None)
            .unwrap();
        std::fs::write(wt.path().join("README.md"), "changed\n").unwrap();
        wt.commit_all("agent work").unwrap().expect("a commit");

        assert_eq!(
            std::fs::read_to_string(t.dir.join("README.md")).unwrap(),
            "hello\n"
        );
        assert!(
            repo.is_clean().unwrap(),
            "a worktree created under .git/ must not make the working tree dirty: {}",
            repo.git(&["status", "--porcelain"]).unwrap()
        );

        wt.remove().unwrap();
    }

    #[test]
    fn committing_reports_the_changed_files_and_skips_empty_work() {
        let t = TestRepo::new("commit");
        let repo = t.repo();
        let wt = repo
            .create_worktree("files", &t.dir.join(".git/wt/f"), None)
            .unwrap();

        assert_eq!(
            wt.commit_all("nothing"),
            Ok(None),
            "no changes means no commit"
        );

        std::fs::write(wt.path().join("new.txt"), "x").unwrap();
        let files = wt.changed_files().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "new.txt");
        assert!(files[0].status.starts_with('A'));

        let sha = wt.commit_all("added new.txt").unwrap().expect("a commit");
        assert_eq!(sha.len(), 40);
        wt.remove().unwrap();
    }

    #[test]
    fn the_patch_contains_the_change_including_untracked_files() {
        let t = TestRepo::new("patch");
        let repo = t.repo();
        let wt = repo
            .create_worktree("patch", &t.dir.join(".git/wt/p"), None)
            .unwrap();
        std::fs::write(wt.path().join("added.rs"), "fn added() {}\n").unwrap();
        let patch = wt.patch().unwrap();
        assert!(patch.contains("added.rs"), "{patch}");
        assert!(patch.contains("+fn added() {}"), "{patch}");
        wt.remove().unwrap();
    }

    #[test]
    fn a_deleted_file_is_removed_by_an_application() {
        let t = TestRepo::new("delete");
        let repo = t.repo();
        let wt = repo
            .create_worktree("del", &t.dir.join(".git/wt/d"), None)
            .unwrap();
        std::fs::remove_file(wt.path().join("README.md")).unwrap();
        let files = wt.changed_files().unwrap();
        assert!(files.iter().any(|f| f.is_delete()), "{files:?}");

        wt.copy_file_to("README.md", &t.dir).unwrap();
        assert!(
            !t.dir.join("README.md").exists(),
            "the deletion did not propagate"
        );
        wt.remove().unwrap();
    }

    #[test]
    fn branch_names_are_unique_so_two_runs_cannot_collide() {
        let t = TestRepo::new("unique");
        let repo = t.repo();
        let a = repo
            .create_worktree("same", &t.dir.join(".git/wt/1"), None)
            .unwrap();
        let b = repo
            .create_worktree("same", &t.dir.join(".git/wt/2"), None)
            .unwrap();
        assert_eq!(a.branch(), "stoic-codex/same");
        assert_eq!(b.branch(), "stoic-codex/same-2");
        a.remove().unwrap();
        b.remove().unwrap();
    }

    #[test]
    fn a_repository_with_no_commits_says_so_instead_of_failing_obscurely() {
        let dir = std::env::temp_dir().join(format!("stoic-codex-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let status = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(status.success());
        let repo = Repo::discover(&dir).unwrap();
        let err = repo
            .create_worktree("x", &dir.join("wt"), None)
            .unwrap_err();
        assert!(err.to_string().contains("no commits"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_identity_fallback_only_fires_when_nothing_is_configured() {
        let t = TestRepo::new("identity-config");
        assert!(
            identity_flags(&t.dir).is_empty(),
            "a repository with a configured identity must keep it"
        );
    }

    #[test]
    fn a_repository_with_no_configured_identity_can_still_be_committed_to() {
        let t = TestRepo::new("identity-none");
        for key in ["user.email", "user.name"] {
            let _ = Command::new("git")
                .arg("-C")
                .arg(&t.dir)
                .args(["config", "--unset", key])
                .status();
        }
        let configured = Command::new("git")
            .arg("-C")
            .arg(&t.dir)
            .args(["config", "user.email"])
            .output()
            .unwrap();
        if !String::from_utf8_lossy(&configured.stdout)
            .trim()
            .is_empty()
        {
            // This machine has a global identity, so the fallback is correctly not needed.
            return;
        }

        assert!(
            !identity_flags(&t.dir).is_empty(),
            "no identity anywhere: the fallback must apply"
        );
        std::fs::write(t.dir.join("mission.txt"), "landed\n").unwrap();
        let repo = t.repo();
        let sha = repo
            .commit_all("mission: 1 file")
            .unwrap()
            .expect("a commit");
        assert_eq!(sha.len(), 40);
        let author = repo.git(&["log", "-1", "--format=%an <%ae>"]).unwrap();
        assert!(author.contains("Stoic Codex"), "committed as {author}");
        assert!(
            repo.is_clean().unwrap(),
            "staging everything must leave the tree clean"
        );
    }

    #[test]
    fn committing_nothing_is_not_an_error() {
        let t = TestRepo::new("identity-clean");
        assert_eq!(t.repo().commit_all("nothing to do"), Ok(None));
    }

    #[test]
    fn slugs_are_readable_and_safe() {
        assert_eq!(slugify("Implementer 1: auth"), "implementer-1-auth");
        assert_eq!(slugify("  "), "agent");
        assert_eq!(slugify("a/b\\c"), "a-b-c");
    }
}
