//! Command implementations. Each returns `Ok(())` or a typed error that the caller maps
//! to an exit code, so no command ever calls `process::exit` itself.

pub mod agent;
pub mod daemon;
pub mod memory;
pub mod mission;
pub mod providers;
pub mod run;
pub mod tools;

use zenithal_memory::daemon::{DaemonConfig, LaunchPolicy};
use zenithal_memory::{MemoryStore, OpenOptions, OpenedMemory};

use crate::config::Config;
use crate::paths::Paths;

/// Open the memory store described by the config.
///
/// The returned [`OpenedMemory`] must outlive the work: dropping it stops a daemon that
/// this process started.
pub fn open_store(paths: &Paths) -> Result<OpenedMemory, CmdError> {
    let cfg = Config::load_for(paths).map_err(CmdError::failed)?;
    cfg.validate().map_err(CmdError::failed)?;
    let daemon = DaemonConfig {
        address: cfg.memory.daemon_address.clone(),
        database: cfg.memory.database.clone(),
        data_dir: paths.typedb_data_dir(),
        log_file: paths.typedb_log(),
        policy: if cfg.memory.allow_daemon {
            LaunchPolicy::Auto
        } else {
            LaunchPolicy::Never
        },
        ..Default::default()
    };
    MemoryStore::open(OpenOptions {
        backend: cfg.memory.backend.clone(),
        daemon,
        file: paths.memory_file(),
    })
    .map_err(|e| CmdError::failed(e.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdError {
    /// The user asked for something that does not exist or is malformed.
    Usage(String),
    /// The operation was understood but could not be completed.
    Failed(String),
}

impl std::fmt::Display for CmdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CmdError::Usage(msg) | CmdError::Failed(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for CmdError {}

impl CmdError {
    pub fn usage(msg: impl Into<String>) -> Self {
        CmdError::Usage(msg.into())
    }
    pub fn failed(msg: impl Into<String>) -> Self {
        CmdError::Failed(msg.into())
    }
}

pub type CmdResult = Result<(), CmdError>;
