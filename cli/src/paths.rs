//! Local paths. One place, so `--home`, `$STOIC_CODEX_HOME` and the real `$HOME` cannot
//! disagree about where the config lives.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use stoic_codex_brand::{CONFIG_DIR, LEGACY_CONFIG_DIR, LEGACY_ENV_PREFIX};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigMigration {
    /// No previous-product config directory was present.
    Unchanged,
    /// The old directory was moved to the new Stoic Codex location.
    Migrated,
    /// Both locations exist; the new one wins and the old one is left untouched.
    BothExist,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The `~` to use. Injected, so tests never touch the real home directory.
    home: PathBuf,
}

impl Paths {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Paths { home: home.into() }
    }

    /// `$STOIC_CODEX_HOME` wins; `$ZENITHAL_HOME` remains a migration alias for existing
    /// installs. Both point at the home root, with the app config directory beneath it.
    pub fn from_env(home: PathBuf) -> Self {
        let legacy_home = format!("{LEGACY_ENV_PREFIX}HOME");
        match std::env::var_os("STOIC_CODEX_HOME")
            .or_else(|| std::env::var_os(legacy_home))
        {
            Some(dir) => Paths::new(PathBuf::from(dir)),
            None => Paths::new(home),
        }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn config_dir(&self) -> PathBuf {
        self.home.join(CONFIG_DIR)
    }

    /// Migrate the previous product's config directory without merging or overwriting data.
    /// If both names already exist, leave the old directory intact and let the caller warn.
    pub fn migrate_legacy_config(&self) -> io::Result<ConfigMigration> {
        let current = self.config_dir();
        let previous = self.home.join(LEGACY_CONFIG_DIR);
        match (current.exists(), previous.exists()) {
            (true, true) => Ok(ConfigMigration::BothExist),
            (false, true) => {
                fs::rename(previous, current)?;
                Ok(ConfigMigration::Migrated)
            }
            _ => Ok(ConfigMigration::Unchanged),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir().join("config.json")
    }

    /// User-defined provider specs, one JSON file each.
    pub fn providers_dir(&self) -> PathBuf {
        self.config_dir().join("providers")
    }

    /// The default single-file memory store, used when TypeDB is not in play.
    pub fn memory_file(&self) -> PathBuf {
        self.config_dir().join("memory.json")
    }

    /// Private, local chat transcripts used by `run --continue` and `run --resume`.
    pub fn sessions_dir(&self) -> PathBuf {
        self.config_dir().join("sessions")
    }

    pub fn typedb_data_dir(&self) -> PathBuf {
        self.config_dir().join("typedb")
    }

    pub fn typedb_log(&self) -> PathBuf {
        self.config_dir().join("typedb.log")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_path_lives_under_the_config_dir() {
        let p = Paths::new("/tmp/home");
        assert_eq!(p.config_dir(), PathBuf::from("/tmp/home/.stoic-codex"));
        for path in [
            p.config_file(),
            p.providers_dir(),
            p.memory_file(),
            p.sessions_dir(),
            p.typedb_data_dir(),
            p.typedb_log(),
        ] {
            assert!(
                path.starts_with(p.config_dir()),
                "{path:?} escaped the config dir"
            );
        }
    }

    #[test]
    fn stoic_codex_home_wins_and_legacy_home_is_the_fallback() {
        let original_new = std::env::var_os("STOIC_CODEX_HOME");
        let original_legacy = std::env::var_os("ZENITHAL_HOME");
        std::env::set_var("STOIC_CODEX_HOME", "/tmp/stoic-home");
        std::env::set_var("ZENITHAL_HOME", "/tmp/zenithal-home");

        let new_home = Paths::from_env(PathBuf::from("/tmp/home"));
        assert_eq!(new_home.home(), Path::new("/tmp/stoic-home"));

        std::env::remove_var("STOIC_CODEX_HOME");
        let legacy_home = Paths::from_env(PathBuf::from("/tmp/home"));
        assert_eq!(legacy_home.home(), Path::new("/tmp/zenithal-home"));

        match original_new {
            Some(value) => std::env::set_var("STOIC_CODEX_HOME", value),
            None => std::env::remove_var("STOIC_CODEX_HOME"),
        }
        match original_legacy {
            Some(value) => std::env::set_var("ZENITHAL_HOME", value),
            None => std::env::remove_var("ZENITHAL_HOME"),
        }
    }

    #[test]
    fn legacy_config_is_moved_once_without_overwriting() {
        let home = std::env::temp_dir().join(format!(
            "stoic-codex-migration-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&home);
        let legacy = home.join(LEGACY_CONFIG_DIR);
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("config.json"), "{}").unwrap();
        let paths = Paths::new(home.clone());
        assert_eq!(
            paths.migrate_legacy_config().unwrap(),
            ConfigMigration::Migrated
        );
        assert_eq!(fs::read_to_string(paths.config_file()).unwrap(), "{}");
        assert!(!legacy.exists());
        assert_eq!(paths.migrate_legacy_config().unwrap(), ConfigMigration::Unchanged);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn when_both_config_dirs_exist_the_legacy_copy_is_left_untouched() {
        let home = std::env::temp_dir().join(format!("stoic-codex-both-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let legacy = home.join(LEGACY_CONFIG_DIR);
        let current = home.join(CONFIG_DIR);
        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(&current).unwrap();
        fs::write(legacy.join("marker"), "old").unwrap();
        fs::write(current.join("marker"), "new").unwrap();
        let paths = Paths::new(home.clone());
        assert_eq!(
            paths.migrate_legacy_config().unwrap(),
            ConfigMigration::BothExist
        );
        assert_eq!(fs::read_to_string(legacy.join("marker")).unwrap(), "old");
        assert_eq!(fs::read_to_string(current.join("marker")).unwrap(), "new");
        let _ = fs::remove_dir_all(home);
    }
}
