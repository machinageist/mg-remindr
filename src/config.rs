// Author: Jeff
// Date: 2026-09-19
// Description: Where mg-remindr keeps its one SQLite file, and how that is overridden
// Notes: $MG_REMINDR_DB wins, then $XDG_CONFIG_HOME/mg-remindr/config.toml, then the
//        XDG data directory. Nothing here opens the file; it only says where it is,
//        so a bad path is reported before any work starts.

use std::{env, fmt, path::PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const APP: &str = "mg-remindr";
const DB_ENV: &str = "MG_REMINDR_DB";
const DEFAULT_DATABASE_FILE: &str = "remindr.sqlite";
const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("configuration file could not be read")]
    Read,
    #[error("configuration file is invalid")]
    Parse,
    #[error("database path is empty")]
    EmptyDatabasePath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub paths: Paths,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DatabaseConfig {
    #[serde(default)]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Paths {
    #[serde(default = "default_config_dir")]
    pub config_dir: PathBuf,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
}

impl Config {
    /// Read the configuration file, then let the environment override it.
    ///
    /// # Errors
    /// Returns an error when the file cannot be read or parsed, or names an empty path.
    pub fn load() -> Result<Self, ConfigError> {
        let paths = Paths::discover();
        let mut config = config_file(&paths)
            .map(|path| std::fs::read_to_string(path).map_err(|_| ConfigError::Read))
            .transpose()?
            .map(|text| toml::from_str::<Self>(&text).map_err(|_| ConfigError::Parse))
            .transpose()?
            .unwrap_or_else(|| Self {
                database: DatabaseConfig::default(),
                paths: paths.clone(),
            });
        config.paths = paths;
        if let Some(value) = env::var_os(DB_ENV) {
            config.database.path = Some(PathBuf::from(value));
        }
        if config.database.path.is_none() {
            config.database.path = Some(config.paths.data_dir.join(DEFAULT_DATABASE_FILE));
        }
        config.validate().map(|()| config)
    }

    /// The file this configuration points at.
    ///
    /// # Errors
    /// Returns an error when the configured path is empty.
    pub fn database_path(&self) -> Result<PathBuf, ConfigError> {
        self.database
            .path
            .clone()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or(ConfigError::EmptyDatabasePath)
    }

    /// Check that the configured path could name a file.
    ///
    /// # Errors
    /// Returns an error when the configured path is empty.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.database.path.is_some() {
            self.database_path().map(|_| ())
        } else {
            Ok(())
        }
    }

    #[must_use]
    pub const fn redacted_database_error() -> RedactedDatabaseError {
        RedactedDatabaseError
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedactedDatabaseError;
impl fmt::Display for RedactedDatabaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("mg-remindr database operation failed")
    }
}
impl std::error::Error for RedactedDatabaseError {}

/// The file used when nothing is configured.
#[must_use]
pub fn default_database_path() -> PathBuf {
    env::var_os(DB_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| default_data_dir().join(DEFAULT_DATABASE_FILE))
}

impl Paths {
    #[must_use]
    pub fn discover() -> Self {
        Self {
            config_dir: default_config_dir(),
            data_dir: default_data_dir(),
        }
    }
}

fn home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn default_config_dir() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join(APP)
}

fn default_data_dir() -> PathBuf {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join(APP)
}

fn config_file(paths: &Paths) -> Option<PathBuf> {
    let path = paths.config_dir.join(CONFIG_FILE);
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_app_scoped() {
        let p = Paths::discover();
        assert!(p.config_dir.ends_with(APP));
        assert!(p.data_dir.ends_with(APP));
    }

    #[test]
    fn the_unconfigured_default_is_one_sqlite_file_under_the_data_directory() {
        let path = default_database_path();
        assert!(path.ends_with(DEFAULT_DATABASE_FILE));
        assert!(path.parent().is_some_and(|parent| parent.ends_with(APP)));
    }

    #[test]
    fn an_empty_configured_path_is_refused() {
        let config = Config {
            database: DatabaseConfig {
                path: Some(PathBuf::new()),
            },
            paths: Paths::discover(),
        };
        assert_eq!(config.validate(), Err(ConfigError::EmptyDatabasePath));
        assert_eq!(config.database_path(), Err(ConfigError::EmptyDatabasePath));
    }

    #[test]
    fn a_configured_path_is_carried_through() {
        let config = Config {
            database: DatabaseConfig {
                path: Some(PathBuf::from("/tmp/elsewhere.sqlite")),
            },
            paths: Paths::discover(),
        };
        assert_eq!(config.validate(), Ok(()));
        assert_eq!(
            config.database_path().unwrap(),
            PathBuf::from("/tmp/elsewhere.sqlite")
        );
    }

    #[test]
    fn minimal_config_uses_defaults() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.database.path.is_none());
    }
}
