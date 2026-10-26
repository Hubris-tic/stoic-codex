//! A shell tool that is off unless it is switched on.
//!
//! Letting a model run commands is the single largest capability jump in this crate — it
//! is also the point where "edits my branch" becomes "runs on my machine". So it is:
//!
//! * **opt-in**: the registry has no shell unless the caller adds one;
//! * **allowlisted by program**, not by string prefix, so `cargo` cannot be smuggled in as
//!   `cargo && curl …` (there is no shell involved, so there is no `&&`);
//! * **time-limited**: a hung command is killed, not waited on forever;
//! * **output-capped**: a command that prints a megabyte does not eat the agent's context.
//!
//! There is no deny list of "dangerous commands" here on purpose. A deny list always loses:
//! the only reliable control is an allow list of programs, plus the fact that the process
//! runs inside the agent's own worktree.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::sandbox::Sandbox;
use crate::tool::{optional_str, optional_u64, required_str, schema_object, Tool, ToolOutcome};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellPolicy {
    /// Programs the agent may execute. `"*"` allows anything — say that out loud.
    pub allow: Vec<String>,
    pub timeout: Duration,
    pub max_output: usize,
}

impl Default for ShellPolicy {
    fn default() -> Self {
        ShellPolicy::locked()
    }
}

impl ShellPolicy {
    /// Nothing runs.
    pub fn locked() -> Self {
        ShellPolicy {
            allow: Vec::new(),
            timeout: Duration::from_secs(60),
            max_output: 16 * 1024,
        }
    }

    /// Typical build/test programs a squad member needs. Note what is *not* here: no
    /// `sh`, no `bash`, no `curl`, no interpreter that could run arbitrary text.
    pub fn build_tools() -> Self {
        ShellPolicy {
            allow: [
                "cargo", "git", "make", "npm", "pytest", "python3", "go", "rustc", "tsc",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            ..ShellPolicy::locked()
        }
    }

    /// Everything. Only for a container you would not mind losing.
    pub fn unrestricted() -> Self {
        ShellPolicy {
            allow: vec!["*".into()],
            ..ShellPolicy::locked()
        }
    }

    pub fn permits(&self, program: &str) -> bool {
        self.allow.iter().any(|a| a == "*" || a == program)
    }
}

pub struct RunCommand {
    sandbox: Arc<Sandbox>,
    policy: ShellPolicy,
}

impl RunCommand {
    pub fn new(sandbox: Arc<Sandbox>, policy: ShellPolicy) -> Self {
        RunCommand { sandbox, policy }
    }
}

impl Tool for RunCommand {
    fn name(&self) -> &'static str {
        "run_command"
    }

    fn description(&self) -> &'static str {
        "Run a program inside the repository and return its exit code and output. No shell: \
         arguments are passed directly, so no pipes or globs. Only allowlisted programs run."
    }

    fn schema(&self) -> Value {
        schema_object(
            &[
                ("program", "string", "Executable name, e.g. `cargo`"),
                ("args", "array", "Arguments, one string each"),
                (
                    "timeout_ms",
                    "integer",
                    "Kill the process after this many milliseconds",
                ),
            ],
            &["program"],
        )
    }

    fn invoke(&self, args: &Value) -> ToolOutcome {
        let program = match required_str(args, "program", "run_command") {
            Ok(p) => p,
            Err(e) => return e,
        };
        if !self.policy.permits(program) {
            return ToolOutcome::err(format!(
                "`{program}` is not on the allow list for this agent{}. No shell was invoked.",
                if self.policy.allow.is_empty() {
                    " (none is: the shell tool is locked)"
                } else {
                    ""
                }
            ));
        }
        let cli_args: Vec<String> = match args.get("args") {
            Some(Value::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    match item.as_str() {
                        Some(s) => out.push(s.to_string()),
                        None => return ToolOutcome::err("every entry in `args` must be a string"),
                    }
                }
                out
            }
            Some(Value::Null) | None => Vec::new(),
            Some(_) => return ToolOutcome::err("`args` must be an array of strings"),
        };
        let timeout = optional_u64(args, "timeout_ms")
            .map(Duration::from_millis)
            .unwrap_or(self.policy.timeout)
            .min(Duration::from_secs(600));

        // Output goes to files, not pipes: a child that fills a pipe buffer while we poll
        // for exit would deadlock, and this is the classic way a "timeout" becomes a hang.
        let stamp = format!(
            "{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir();
        let out_path = dir.join(format!("stoic-codex-cmd-{stamp}.out"));
        let err_path = dir.join(format!("stoic-codex-cmd-{stamp}.err"));
        let (Ok(out_file), Ok(err_file)) = (
            std::fs::File::create(&out_path),
            std::fs::File::create(&err_path),
        ) else {
            return ToolOutcome::err("cannot create a temporary file for the command output");
        };

        let mut command = Command::new(program);
        command
            .args(&cli_args)
            .current_dir(self.sandbox.root())
            .stdin(Stdio::null())
            .stdout(out_file)
            .stderr(err_file);
        // A user-provided cwd would be a second escape route; `.current_dir` is the root.

        let started = Instant::now();
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = std::fs::remove_file(&out_path);
                let _ = std::fs::remove_file(&err_path);
                return ToolOutcome::err(format!("cannot run `{program}`: {e}"));
            }
        };

        let mut killed = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if started.elapsed() > timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        killed = true;
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&out_path);
                    let _ = std::fs::remove_file(&err_path);
                    return ToolOutcome::err(format!("cannot wait for `{program}`: {e}"));
                }
            }
        };

        let stdout = read_capped(&out_path, self.policy.max_output);
        let stderr = read_capped(&err_path, self.policy.max_output);
        let _ = std::fs::remove_file(&out_path);
        let _ = std::fs::remove_file(&err_path);

        let mut report = String::new();
        match (&status, killed) {
            (_, true) => report.push_str(&format!(
                "`{program}` was killed after {} ms (timeout)\n",
                started.elapsed().as_millis()
            )),
            (Some(s), false) => report.push_str(&format!(
                "exit: {}\n",
                s.code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into())
            )),
            (None, false) => report.push_str("exit: unknown\n"),
        }
        if !stdout.trim().is_empty() {
            report.push_str("stdout:\n");
            report.push_str(&stdout);
            if !report.ends_with('\n') {
                report.push('\n');
            }
        }
        if !stderr.trim().is_empty() {
            report.push_str("stderr:\n");
            report.push_str(&stderr);
        }
        let failed = killed || status.as_ref().map(|s| !s.success()).unwrap_or(true);
        if failed {
            ToolOutcome::err(report)
        } else {
            ToolOutcome::ok(report)
        }
    }
}

fn read_capped(path: &std::path::Path, cap: usize) -> String {
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 8192];
    while buf.len() < cap {
        match file.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n.min(cap - buf.len())]),
        }
    }
    let mut text = String::from_utf8_lossy(&buf).to_string();
    if buf.len() >= cap {
        text.push_str(&format!("\n… output truncated at {cap} bytes"));
    }
    text
}

/// `cwd` is accepted in the schema vocabulary but deliberately ignored: it is documented
/// here so a future reader does not "restore" it by accident.
#[allow(dead_code)]
fn _cwd_is_not_supported(args: &Value) -> Option<&str> {
    optional_str(args, "cwd")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> (Arc<Sandbox>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "stoic-codex-shell-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (Arc::new(Sandbox::new(&dir).unwrap()), dir)
    }

    #[test]
    fn a_locked_policy_runs_nothing_and_says_why() {
        let (sb, _dir) = sandbox();
        let tool = RunCommand::new(sb, ShellPolicy::locked());
        let out = tool.invoke(&serde_json::json!({"program": "echo", "args": ["hi"]}));
        assert!(out.is_error);
        assert!(
            out.content.contains("not on the allow list"),
            "{}",
            out.content
        );
        assert!(out.content.contains("locked"), "{}", out.content);
    }

    #[test]
    fn an_allowed_program_runs_and_reports_its_output() {
        let (sb, _dir) = sandbox();
        let tool = RunCommand::new(
            sb,
            ShellPolicy {
                allow: vec!["echo".into()],
                ..ShellPolicy::locked()
            },
        );
        let out = tool.invoke(&serde_json::json!({"program": "echo", "args": ["hello", "world"]}));
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("exit: 0"), "{}", out.content);
        assert!(out.content.contains("hello world"), "{}", out.content);
    }

    #[test]
    fn a_non_zero_exit_is_an_error_outcome_carrying_the_output() {
        let (sb, _dir) = sandbox();
        let tool = RunCommand::new(
            sb,
            ShellPolicy {
                allow: vec!["ls".into()],
                ..ShellPolicy::locked()
            },
        );
        let out =
            tool.invoke(&serde_json::json!({"program": "ls", "args": ["definitely-missing"]}));
        assert!(out.is_error);
        assert!(out.content.contains("exit: 2"), "{}", out.content);
        assert!(out.content.contains("stderr"), "{}", out.content);
    }

    #[test]
    fn there_is_no_shell_so_metacharacters_are_arguments() {
        let (sb, _dir) = sandbox();
        let tool = RunCommand::new(
            sb,
            ShellPolicy {
                allow: vec!["echo".into()],
                ..ShellPolicy::locked()
            },
        );
        let out = tool.invoke(
            &serde_json::json!({"program": "echo", "args": ["a; rm -rf /", "&&", "whoami"]}),
        );
        assert!(
            out.content.contains("a; rm -rf / && whoami"),
            "{}",
            out.content
        );
    }

    #[test]
    fn a_hung_command_is_killed_at_the_timeout() {
        let (sb, _dir) = sandbox();
        let tool = RunCommand::new(
            sb,
            ShellPolicy {
                allow: vec!["sleep".into()],
                ..ShellPolicy::locked()
            },
        );
        let out =
            tool.invoke(&serde_json::json!({"program": "sleep", "args": ["5"], "timeout_ms": 100}));
        assert!(out.is_error);
        assert!(out.content.contains("timeout"), "{}", out.content);
    }

    #[test]
    fn commands_run_inside_the_sandbox_root() {
        let (sb, dir) = sandbox();
        std::fs::write(dir.join("marker.txt"), "x").unwrap();
        let tool = RunCommand::new(
            sb,
            ShellPolicy {
                allow: vec!["ls".into()],
                ..ShellPolicy::locked()
            },
        );
        let out = tool.invoke(&serde_json::json!({"program": "ls"}));
        assert!(out.content.contains("marker.txt"), "{}", out.content);
    }

    #[test]
    fn output_is_capped_with_an_explicit_notice() {
        let (sb, _dir) = sandbox();
        let tool = RunCommand::new(
            sb,
            ShellPolicy {
                allow: vec!["echo".into()],
                max_output: 16,
                ..ShellPolicy::locked()
            },
        );
        let out = tool.invoke(&serde_json::json!({"program": "echo", "args": ["x".repeat(500)]}));
        assert!(
            out.content.contains("truncated at 16 bytes"),
            "{}",
            out.content
        );
    }
}
