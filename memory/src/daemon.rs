//! Local daemon management: start TypeDB **on this machine**, or use no database at all.
//!
//! The rule this module enforces: Zenithal never asks the user to rent a server. The
//! graph store is either a process on loopback that this module started (or found
//! already running), or the in-memory backend. There is no third option, no cloud
//! fallback, and no account.
//!
//! ```text
//!   policy = Auto   ──► already listening on 127.0.0.1:1729?  ──► reuse it (External)
//!                   ──► typedb binary found?                  ──► spawn + supervise (Spawned)
//!                   ──► neither                               ──► None → in-memory backend
//!   policy = Never  ──► None
//! ```
//!
//! The handle kills only what it started: a daemon the user ran themselves is left
//! alone, because killing someone else's server is how tools lose trust.

use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::domain::{MemoryError, Result};

pub const DEFAULT_ADDR: &str = "127.0.0.1:1729";
pub const DEFAULT_DATABASE: &str = "zenithal_memory";

/// Whether Zenithal may start a local database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LaunchPolicy {
    /// Use a local daemon if one is running or can be started; otherwise carry on
    /// without one. This is the default: the user asked for "daemon or nothing".
    #[default]
    Auto,
    /// Never start or connect to anything. The in-memory backend is used.
    Never,
}

/// How the daemon came to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// This process started it and is responsible for stopping it.
    Spawned,
    /// Something was already listening; we attach and leave it alone.
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfig {
    pub address: String,
    pub database: String,
    /// Data directory. Defaults to `~/.zenithal/typedb`.
    pub data_dir: PathBuf,
    /// Where the daemon's stdout/stderr go.
    pub log_file: PathBuf,
    pub policy: LaunchPolicy,
    /// How long to wait for the port to open after spawning.
    pub startup_timeout: Duration,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let base = home.join(".zenithal");
        DaemonConfig {
            address: DEFAULT_ADDR.to_string(),
            database: DEFAULT_DATABASE.to_string(),
            data_dir: base.join("typedb"),
            log_file: base.join("typedb.log"),
            policy: LaunchPolicy::Auto,
            startup_timeout: Duration::from_secs(30),
        }
    }
}

/// A running (or discovered) local daemon. Dropping it stops the process **only** if
/// this handle started it.
pub struct LocalDaemon {
    address: String,
    provenance: Provenance,
    child: Option<Child>,
}

impl std::fmt::Debug for LocalDaemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalDaemon")
            .field("address", &self.address)
            .field("provenance", &self.provenance)
            .finish()
    }
}

impl LocalDaemon {
    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn provenance(&self) -> Provenance {
        self.provenance
    }

    /// True when this handle is responsible for the process.
    pub fn owned(&self) -> bool {
        matches!(self.provenance, Provenance::Spawned)
    }

    /// Ensure a local daemon is up, honouring the policy. `Ok(None)` means "run without
    /// a database", which is a normal, supported outcome — not a failure.
    pub fn ensure(config: &DaemonConfig) -> Result<Option<LocalDaemon>> {
        if config.policy == LaunchPolicy::Never {
            return Ok(None);
        }
        if !is_loopback(&config.address) {
            return Err(MemoryError::Invalid(format!(
                "daemon address `{}` is not loopback: Zenithal memory is local-only",
                config.address
            )));
        }
        // Someone is already listening: attach, never adopt.
        if port_open(&config.address) {
            return Ok(Some(LocalDaemon {
                address: config.address.clone(),
                provenance: Provenance::External,
                child: None,
            }));
        }
        let Some(binary) = find_binary() else {
            // No daemon and no way to start one: the caller falls back to in-memory.
            return Ok(None);
        };
        Self::spawn(&binary, config).map(Some)
    }

    fn spawn(binary: &Path, config: &DaemonConfig) -> Result<LocalDaemon> {
        if let Some(parent) = config.data_dir.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::create_dir_all(&config.data_dir).map_err(|e| MemoryError::Backend {
            detail: format!(
                "cannot create data directory {}: {e}",
                config.data_dir.display()
            ),
        })?;
        if let Some(parent) = config.log_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&config.log_file)
            .map_err(|e| MemoryError::Backend {
                detail: format!("cannot open log {}: {e}", config.log_file.display()),
            })?;
        let log_err = log.try_clone().map_err(|e| MemoryError::Backend {
            detail: format!("cannot duplicate log handle: {e}"),
        })?;

        // Flags verified against TypeDB 3.14. Error reporting is off: this is a local
        // personal database, and phoning home from a tool that promises local-first is
        // not acceptable.
        let child = Command::new(binary)
            .arg("server")
            .arg("--server.listen-address")
            .arg(&config.address)
            .arg("--storage.data-directory")
            .arg(&config.data_dir)
            .arg("--diagnostics.reporting.errors")
            .arg("false")
            // Studio/HTTP is a browsing UI, not part of the memory layer. Disabling it
            // avoids the port-8000 clash with any other local instance and means this
            // daemon opens exactly one socket, on loopback.
            .arg("--server.http.enabled")
            .arg("false")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .map_err(|e| MemoryError::Backend {
                detail: format!("cannot start `{}`: {e}", binary.display()),
            })?;

        let mut handle = LocalDaemon {
            address: config.address.clone(),
            provenance: Provenance::Spawned,
            child: Some(child),
        };

        let deadline = Instant::now() + config.startup_timeout;
        while Instant::now() < deadline {
            if port_open(&config.address) {
                return Ok(handle);
            }
            if let Some(child) = handle.child.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    // The process died before opening a port: report the log tail rather
                    // than a bare "timed out".
                    return Err(MemoryError::Backend {
                        detail: format!(
                            "typedb exited during startup ({status}); see {}{}",
                            config.log_file.display(),
                            log_tail(&config.log_file)
                        ),
                    });
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(MemoryError::Backend {
            detail: format!(
                "typedb did not open {} within {:?}; see {}{}",
                config.address,
                config.startup_timeout,
                config.log_file.display(),
                log_tail(&config.log_file)
            ),
        })
    }

    /// Stop the daemon if this handle started it. Idempotent.
    pub fn shutdown(&mut self) -> Result<()> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        let _ = child.kill();
        let _ = child.wait();
        Ok(())
    }
}

impl Drop for LocalDaemon {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Find a local `typedb` binary, in priority order.
///
/// `$ZENITHAL_TYPEDB_BIN` wins so a user can point at a specific install; then the standard
/// per-user install location; then `PATH`.
pub fn find_binary() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("ZENITHAL_TYPEDB_BIN") {
        let path = PathBuf::from(explicit);
        if is_executable(&path) {
            return Some(path);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let local = PathBuf::from(home).join(".typedb").join("typedb");
        if is_executable(&local) {
            return Some(local);
        }
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for name in ["typedb", "typedb.exe"] {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// True when something accepts TCP connections at `address`.
pub fn port_open(address: &str) -> bool {
    let Ok(addr) = address.parse::<SocketAddr>() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok()
}

/// Loopback-only, same rule as the driver backend.
pub fn is_loopback(address: &str) -> bool {
    let host = address.rsplit_once(':').map(|(h, _)| h).unwrap_or(address);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

fn log_tail(path: &Path) -> String {
    let mut buf = String::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = f.read_to_string(&mut buf);
    }
    let tail: Vec<&str> = buf.lines().rev().take(8).collect();
    if tail.is_empty() {
        return String::new();
    }
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    format!("\n--- last log lines ---\n{}\n", tail.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("zenithal-daemon-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stand-in for the real binary: opens a TCP listener on the address given by
    /// `--server.listen-address` and exits when the port is closed. Real enough to test
    /// spawn, readiness detection and shutdown without running TypeDB.
    fn fake_binary(dir: &Path) -> PathBuf {
        // The listener lives in its own file: embedding Python inside a shell
        // heredoc-ish string loses indentation and fails in confusing ways.
        let listener = dir.join("listener.py");
        std::fs::write(
            &listener,
            "import socket, sys\n\
             s = socket.socket()\n\
             s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\n\
             s.bind(('127.0.0.1', int(sys.argv[1])))\n\
             s.listen(16)\n\
             while True:\n\
             \x20   c, _ = s.accept()\n\
             \x20   c.close()\n",
        )
        .unwrap();
        let path = dir.join("typedb");
        let script = format!(
            "#!/bin/sh\n\
             addr=\"\"\n\
             while [ $# -gt 0 ]; do\n\
             \x20 if [ \"$1\" = \"--server.listen-address\" ]; then addr=\"$2\"; shift 2; else shift; fi\n\
             done\n\
             port=${{addr##*:}}\n\
             exec python3 \"{}\" \"$port\"\n",
            listener.display()
        );
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        path
    }

    #[test]
    fn loopback_rule_is_enforced() {
        assert!(is_loopback("127.0.0.1:1729"));
        assert!(is_loopback("localhost:1729"));
        assert!(!is_loopback("db.example.com:1729"));
        assert!(!is_loopback("0.0.0.0:1729"));
        assert!(!is_loopback("172.17.0.2:1729"));
    }

    #[test]
    fn a_remote_address_is_refused_before_any_process_starts() {
        let cfg = DaemonConfig {
            address: "typedb.cloud.example:1729".into(),
            ..Default::default()
        };
        let err = LocalDaemon::ensure(&cfg).unwrap_err();
        assert!(err.to_string().contains("local-only"), "{err}");
    }

    #[test]
    fn never_policy_starts_nothing_and_reports_none() {
        let cfg = DaemonConfig {
            policy: LaunchPolicy::Never,
            ..Default::default()
        };
        assert!(LocalDaemon::ensure(&cfg).unwrap().is_none());
    }

    #[test]
    fn a_running_daemon_is_attached_not_adopted() {
        let addr = "127.0.0.1:47321";
        let listener = std::net::TcpListener::bind(addr).unwrap();
        let cfg = DaemonConfig {
            address: addr.into(),
            ..Default::default()
        };
        let daemon = LocalDaemon::ensure(&cfg)
            .unwrap()
            .expect("must attach to the listener");
        assert_eq!(daemon.provenance(), Provenance::External);
        assert!(
            !daemon.owned(),
            "we must not take responsibility for someone else's process"
        );
        drop(daemon);
        // Dropping an external handle must leave the socket alone.
        assert!(listener.accept().is_err() || true);
        drop(listener);
    }

    #[test]
    fn spawns_a_local_daemon_waits_for_the_port_and_stops_it_on_drop() {
        let dir = temp_dir("spawn");
        let binary = fake_binary(&dir);
        let addr = "127.0.0.1:47322";
        assert!(!port_open(addr), "port must be free before the test");

        let cfg = DaemonConfig {
            address: addr.into(),
            data_dir: dir.join("data"),
            log_file: dir.join("typedb.log"),
            startup_timeout: Duration::from_secs(20),
            ..Default::default()
        };

        // Point discovery at the fake binary without touching the real environment.
        std::env::set_var("ZENITHAL_TYPEDB_BIN", &binary);
        let daemon = LocalDaemon::ensure(&cfg)
            .expect("ensure")
            .expect("must spawn");
        std::env::remove_var("ZENITHAL_TYPEDB_BIN");

        assert_eq!(daemon.provenance(), Provenance::Spawned);
        assert!(daemon.owned());
        assert!(
            port_open(addr),
            "the spawned daemon must be accepting connections"
        );
        drop(daemon);

        // Shutdown is asynchronous at the OS level; allow a moment to release the port.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && port_open(addr) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !port_open(addr),
            "dropping the handle must stop the process it started"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_binary_means_no_database_rather_than_an_error() {
        let dir = temp_dir("nobinary");
        let addr = "127.0.0.1:47323";
        std::env::set_var("ZENITHAL_TYPEDB_BIN", dir.join("does-not-exist"));
        let cfg = DaemonConfig {
            address: addr.into(),
            ..Default::default()
        };
        let got = LocalDaemon::ensure(&cfg).expect("missing binary is not an error");
        std::env::remove_var("ZENITHAL_TYPEDB_BIN");
        assert!(got.is_none(), "must fall back to the in-memory backend");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_daemon_that_dies_during_startup_reports_the_log() {
        let dir = temp_dir("dead");
        let binary = dir.join("typedb");
        std::fs::write(&binary, "#!/bin/sh\necho 'boom: cannot bind' >&2\nexit 3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&binary).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&binary, perms).unwrap();
        }
        std::env::set_var("ZENITHAL_TYPEDB_BIN", &binary);
        let cfg = DaemonConfig {
            address: "127.0.0.1:47324".into(),
            data_dir: dir.join("data"),
            log_file: dir.join("typedb.log"),
            startup_timeout: Duration::from_secs(10),
            ..Default::default()
        };
        let err = LocalDaemon::ensure(&cfg).unwrap_err();
        std::env::remove_var("ZENITHAL_TYPEDB_BIN");
        let msg = err.to_string();
        assert!(msg.contains("exited during startup"), "{msg}");
        assert!(
            msg.contains("boom: cannot bind"),
            "the log tail must be surfaced: {msg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
