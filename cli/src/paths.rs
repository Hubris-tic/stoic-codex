//! Local paths. One place, so `--home`, `$ZENITHAL_HOME` and the real `$HOME` cannot
//! disagree about where the config lives.

use std::path::{Path, PathBuf};

use zenithal_brand::CONFIG_DIR;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The `~` to use. Injected, so tests never touch the real home directory.
    home: PathBuf,
}

impl Paths {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Paths { home: home.into() }
    }

    /// `$ZENITHAL_HOME` wins over `$HOME`: useful for tests, containers and job runners.
    pub fn from_env(home: PathBuf) -> Self {
        match std::env::var_os("ZENITHAL_HOME") {
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
        assert_eq!(p.config_dir(), PathBuf::from("/tmp/home/.zenithal"));
        for path in [
            p.config_file(),
            p.providers_dir(),
            p.memory_file(),
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
    fn zenithal_home_overrides_the_real_home() {
        std::env::set_var("ZENITHAL_HOME", "/tmp/elsewhere");
        let p = Paths::from_env(PathBuf::from("/tmp/home"));
        std::env::remove_var("ZENITHAL_HOME");
        assert_eq!(p.home(), Path::new("/tmp/elsewhere"));
    }
}
