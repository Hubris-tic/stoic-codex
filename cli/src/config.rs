//! `~/.stoic-codex/config.json` — small, hand-editable, and never required.
//!
//! Every field has a default, so a missing or partial file is normal. Unknown keys are
//! rejected: a typo like `"contxt_budget"` should not be silently ignored, because the
//! user's next hour would be spent wondering why the setting has no effect.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::paths::Paths;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub memory: MemorySettings,
    #[serde(default)]
    pub providers: ProviderSettings,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            memory: MemorySettings::default(),
            providers: ProviderSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemorySettings {
    /// `auto` (TypeDB if available, else the file store), `typedb`, `file`, `memory`.
    #[serde(default = "default_backend")]
    pub backend: String,
    /// Local daemon address. Loopback only; a remote address is refused at startup.
    #[serde(default = "default_address")]
    pub daemon_address: String,
    #[serde(default = "default_database")]
    pub database: String,
    /// Character budget for the memory block injected into a prompt.
    #[serde(default = "default_budget")]
    pub context_budget: usize,
    /// Token budget for that block, enforced with the calibrated local estimator.
    #[serde(default = "default_token_budget")]
    pub context_token_budget: usize,
    /// Set false to guarantee that nothing is ever started.
    #[serde(default = "default_true")]
    pub allow_daemon: bool,
}

fn default_backend() -> String {
    "auto".into()
}
fn default_address() -> String {
    "127.0.0.1:1729".into()
}
fn default_database() -> String {
    // Keep the historical TypeDB database identifier so this rebrand does not orphan data.
    "zenithal_memory".into()
}
fn default_budget() -> usize {
    1200
}
fn default_token_budget() -> usize {
    400
}
fn default_true() -> bool {
    true
}

impl Default for MemorySettings {
    fn default() -> Self {
        MemorySettings {
            backend: default_backend(),
            daemon_address: default_address(),
            database: default_database(),
            context_budget: default_budget(),
            context_token_budget: default_token_budget(),
            allow_daemon: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderSettings {
    /// Provider id used when a command needs one and none was given.
    #[serde(default)]
    pub default: Option<String>,
    /// Model used when a command needs one and none was given.
    #[serde(default)]
    pub model: Option<String>,
    /// Environment variable holding the credential. Defaults to `<PROVIDER>_API_KEY`;
    /// set this when your provider uses a different name.
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Stream tokens as they arrive. Off means one request, one JSON reply.
    #[serde(default = "default_true")]
    pub stream: bool,
    /// Per-connection socket timeout, in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Total attempts for a request that failed in a retryable way (408/429/5xx, or a
    /// connection that never opened). 1 disables retrying; the default is 4.
    #[serde(default = "default_attempts")]
    pub max_attempts: Option<u32>,
    /// Route every request through this base URL, keeping the provider's own path. For a
    /// local proxy, a gateway, or a test server — the provider definition is untouched.
    #[serde(default)]
    pub base_url: Option<String>,
}

fn default_timeout() -> u64 {
    120
}

fn default_attempts() -> Option<u32> {
    Some(4)
}

impl Default for ProviderSettings {
    fn default() -> Self {
        ProviderSettings {
            default: None,
            model: None,
            api_key_env: None,
            max_tokens: None,
            temperature: None,
            stream: true,
            timeout_secs: default_timeout(),
            max_attempts: default_attempts(),
            base_url: None,
        }
    }
}

impl Config {
    /// Load from `path`. A missing file yields defaults; a broken file is an error that
    /// names the path — never a silent fallback, or the user's edits appear to do nothing.
    pub fn load(path: &Path) -> Result<Config, String> {
        match std::fs::read_to_string(path) {
            Ok(text) if text.trim().is_empty() => Ok(Config::default()),
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| format!("{} is not a valid config file: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    pub fn load_for(paths: &Paths) -> Result<Config, String> {
        Config::load(&paths.config_file())
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| format!("cannot serialize config: {e}"))?;
        std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// Validate values that a JSON schema cannot express.
    pub fn validate(&self) -> Result<(), String> {
        match self.memory.backend.as_str() {
            "auto" | "typedb" | "file" | "memory" => {}
            other => {
                return Err(format!(
                    "memory.backend `{other}` is not one of: auto, typedb, file, memory"
                ))
            }
        }
        if !stoic_codex_memory::daemon::is_loopback(&self.memory.daemon_address) {
            return Err(format!(
                "memory.daemon_address `{}` is not loopback — Stoic Codex memory is local-only",
                self.memory.daemon_address
            ));
        }
        if self.memory.context_budget < 100 {
            return Err("memory.context_budget must be at least 100 characters".into());
        }
        if self.memory.context_token_budget == 0 {
            return Err("memory.context_token_budget must be at least 1 token".into());
        }
        if let Some(attempts) = self.providers.max_attempts {
            if !(1..=10).contains(&attempts) {
                return Err("providers.max_attempts must be between 1 and 10".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stoic-codex-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.json")
    }

    #[test]
    fn a_missing_file_is_defaults() {
        let path = temp("missing");
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.memory.backend, "auto");
        assert_eq!(cfg.memory.database, "zenithal_memory");
        assert!(cfg.memory.allow_daemon);
        assert_eq!(cfg.memory.context_budget, 1200);
        assert_eq!(cfg.memory.context_token_budget, 400);
    }

    #[test]
    fn a_partial_file_keeps_defaults_for_the_rest() {
        let path = temp("partial");
        std::fs::write(
            &path,
            r#"{"memory":{"context_budget":400,"context_token_budget":128}}"#,
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.memory.context_budget, 400);
        assert_eq!(cfg.memory.context_token_budget, 128);
        assert_eq!(cfg.memory.daemon_address, "127.0.0.1:1729");
    }

    #[test]
    fn max_attempts_must_be_between_one_and_ten() {
        for attempts in [0, 11] {
            let mut cfg = Config::default();
            cfg.providers.max_attempts = Some(attempts);
            let error = cfg.validate().unwrap_err();
            assert!(error.contains("providers.max_attempts"), "{error}");
        }
    }

    #[test]
    fn a_typo_is_rejected_rather_than_ignored() {
        let path = temp("typo");
        std::fs::write(&path, r#"{"memory":{"contxt_budget":400}}"#).unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(
            err.contains("contxt_budget") || err.contains("unknown field"),
            "{err}"
        );
    }

    #[test]
    fn round_trips_through_disk() {
        let path = temp("roundtrip");
        let mut cfg = Config::default();
        cfg.memory.daemon_address = "127.0.0.1:1800".into();
        cfg.memory.context_budget = 900;
        cfg.memory.context_token_budget = 300;
        cfg.save(&path).unwrap();
        let back = Config::load(&path).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn a_remote_daemon_address_fails_validation() {
        let mut cfg = Config::default();
        cfg.memory.daemon_address = "typedb.example.com:1729".into();
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("local-only"), "{err}");
    }

    #[test]
    fn an_unknown_backend_fails_validation() {
        let mut cfg = Config::default();
        cfg.memory.backend = "postgres".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn a_tiny_context_budget_fails_validation() {
        // A 20-character budget cannot fit a single memory entry; failing here beats
        // silently injecting nothing.
        let mut cfg = Config::default();
        cfg.memory.context_budget = 20;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn a_zero_memory_token_budget_is_rejected() {
        let mut cfg = Config::default();
        cfg.memory.context_token_budget = 0;
        let error = cfg.validate().unwrap_err();
        assert!(error.contains("context_token_budget"), "{error}");
    }
}
