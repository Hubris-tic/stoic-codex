//! Path confinement for tools.
//!
//! Every tool that touches the filesystem goes through a [`Sandbox`]. The rule is simple
//! and is enforced in one place: **a path is either inside the root, or it does not
//! exist.** An agent that can write `../../.bashrc` is not an agent, it is a remote shell.
//!
//! Three escapes are rejected, in this order:
//! 1. absolute paths — they cannot be relative to the root at all;
//! 2. any `..` component — rejected lexically, before touching the disk, so
//!    `a/../../b` cannot be smuggled past a canonicalisation step;
//! 3. symlinks that leave the root — caught by canonicalising the deepest existing
//!    ancestor and checking the prefix.
//!
//! `.git` is refused for writes: a worktree's git metadata is the orchestrator's, and an
//! agent that edits it can forge another agent's result.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// An absolute path was given where a relative one was required.
    Absolute {
        path: String,
    },
    /// A `..` component was present.
    Parent {
        path: String,
    },
    /// The path resolves outside the root (symlink, or a root that moved).
    Escapes {
        path: String,
        root: String,
    },
    /// Writing into `.git`.
    GitInternals {
        path: String,
    },
    NotFound {
        path: String,
    },
    Io {
        path: String,
        detail: String,
    },
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxError::Absolute { path } => {
                write!(
                    f,
                    "`{path}` is absolute; use a path relative to the repository"
                )
            }
            SandboxError::Parent { path } => {
                write!(
                    f,
                    "`{path}` contains `..`; paths must stay inside the repository"
                )
            }
            SandboxError::Escapes { path, root } => {
                write!(f, "`{path}` resolves outside {root}")
            }
            SandboxError::GitInternals { path } => {
                write!(f, "`{path}` is inside .git, which the agent may not modify")
            }
            SandboxError::NotFound { path } => write!(f, "`{path}` does not exist"),
            SandboxError::Io { path, detail } => write!(f, "{path}: {detail}"),
        }
    }
}

impl std::error::Error for SandboxError {}

#[derive(Debug, Clone)]
pub struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// Canonicalise the root once, so later prefix checks compare like with like.
    pub fn new(root: impl AsRef<Path>) -> Result<Sandbox, SandboxError> {
        let root = root.as_ref();
        let canonical = root.canonicalize().map_err(|e| SandboxError::Io {
            path: root.display().to_string(),
            detail: e.to_string(),
        })?;
        Ok(Sandbox { root: canonical })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a path that may not exist yet (a write target).
    pub fn resolve(&self, raw: &str) -> Result<PathBuf, SandboxError> {
        self.check_lexically(raw)?;
        let joined = self.root.join(raw.trim_start_matches("./"));
        if joined.exists() {
            return self.confine(joined, raw);
        }
        // Check the deepest existing ancestor, so a symlinked directory cannot be used as
        // a foothold for a path that does not exist yet.
        let mut ancestor = joined.clone();
        while !ancestor.exists() {
            match ancestor.parent() {
                Some(p) => ancestor = p.to_path_buf(),
                None => break,
            }
        }
        self.confine(ancestor, raw)?;
        Ok(joined)
    }

    /// Resolve a path that must already exist (a read).
    pub fn resolve_existing(&self, raw: &str) -> Result<PathBuf, SandboxError> {
        let path = self.resolve(raw)?;
        if !path.exists() {
            return Err(SandboxError::NotFound {
                path: raw.to_string(),
            });
        }
        Ok(path)
    }

    /// Relative form, for messages: never leak an absolute path the user cannot see.
    pub fn display(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.display().to_string())
    }

    /// True when a relative path is `.git` or below it.
    pub fn is_git_internal(&self, raw: &str) -> bool {
        Path::new(raw).components().any(|c| c.as_os_str() == ".git")
    }

    fn check_lexically(&self, raw: &str) -> Result<(), SandboxError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(SandboxError::NotFound {
                path: "(empty path)".into(),
            });
        }
        let path = Path::new(trimmed);
        if path.is_absolute() {
            return Err(SandboxError::Absolute {
                path: trimmed.to_string(),
            });
        }
        for component in path.components() {
            match component {
                Component::ParentDir => {
                    return Err(SandboxError::Parent {
                        path: trimmed.to_string(),
                    })
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(SandboxError::Absolute {
                        path: trimmed.to_string(),
                    })
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn confine(&self, candidate: PathBuf, raw: &str) -> Result<PathBuf, SandboxError> {
        let canonical = candidate.canonicalize().unwrap_or(candidate);
        if !canonical.starts_with(&self.root) {
            return Err(SandboxError::Escapes {
                path: raw.to_string(),
                root: self.root.display().to_string(),
            });
        }
        Ok(canonical)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> (Sandbox, tempdir::TempDir) {
        let dir = tempdir::TempDir::new();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        (Sandbox::new(dir.path()).unwrap(), dir)
    }

    /// A tiny temp-dir helper: one dependency fewer than `tempfile`, and this crate needs
    /// nothing else from it.
    mod tempdir {
        use std::path::{Path, PathBuf};

        pub struct TempDir(PathBuf);
        impl TempDir {
            pub fn new() -> TempDir {
                let mut path = std::env::temp_dir();
                let unique = format!(
                    "stoic-codex-agent-{}-{:?}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                );
                path.push(unique);
                std::fs::create_dir_all(&path).unwrap();
                TempDir(path)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn a_relative_path_stays_inside() {
        let (sandbox, _dir) = sandbox();
        let path = sandbox.resolve_existing("src/main.rs").unwrap();
        assert_eq!(sandbox.display(&path), "src/main.rs");
    }

    #[test]
    fn an_absolute_path_is_refused() {
        let (sandbox, _dir) = sandbox();
        assert!(matches!(
            sandbox.resolve("/etc/passwd"),
            Err(SandboxError::Absolute { .. })
        ));
    }

    #[test]
    fn any_parent_component_is_refused_even_if_it_would_come_back() {
        let (sandbox, _dir) = sandbox();
        for attempt in ["../escape", "src/../../escape", "a/../b"] {
            assert!(
                matches!(sandbox.resolve(attempt), Err(SandboxError::Parent { .. })),
                "{attempt} was not refused"
            );
        }
    }

    #[test]
    fn a_symlink_out_of_the_root_is_refused() {
        let (sandbox, dir) = sandbox();
        let outside = dir.path().parent().unwrap().join("stoic-codex-outside-target");
        std::fs::write(&outside, "secret").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, dir.path().join("link")).unwrap();
            assert!(matches!(
                sandbox.resolve("link"),
                Err(SandboxError::Escapes { .. })
            ));
        }
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn a_write_target_inside_a_missing_directory_is_allowed() {
        let (sandbox, _dir) = sandbox();
        let path = sandbox.resolve("src/deep/new/file.txt").unwrap();
        assert!(path.starts_with(sandbox.root()));
    }

    #[test]
    fn git_internals_are_recognised() {
        let (sandbox, _dir) = sandbox();
        assert!(sandbox.is_git_internal(".git/config"));
        assert!(sandbox.is_git_internal("sub/.git/HEAD"));
        assert!(!sandbox.is_git_internal("src/gitfile.rs"));
    }

    #[test]
    fn empty_paths_are_rejected_with_a_readable_message() {
        let (sandbox, _dir) = sandbox();
        let err = sandbox.resolve("   ").unwrap_err().to_string();
        assert!(err.contains("empty"), "{err}");
    }
}
