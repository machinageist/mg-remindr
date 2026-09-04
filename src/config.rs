use std::{env, fmt, path::PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct};
use thiserror::Error;
use url::Url;

const APP: &str = "mg-todo";
const DB_ENV: &str = "MG_TODO_DATABASE_URL";
const TEST_ENV: &str = "MG_TODO_ALLOW_INTEGRATION_TESTS";
// The unconfigured authority is the local peer-authenticated socket, the same shape mg-calr uses
const DEFAULT_DATABASE: &str = "mg_todo";
const DEFAULT_SOCKET_DIR: &str = "/run/postgresql";

#[derive(Clone, PartialEq, Eq)]
pub struct DatabaseUrl(String);
impl DatabaseUrl {
    pub fn parse(value: String) -> Result<Self, ConfigError> {
        validate_local_database_url(&value)?;
        Ok(Self(value))
    }
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for DatabaseUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DatabaseUrl(\"[REDACTED]\")")
    }
}
impl<'de> Deserialize<'de> for DatabaseUrl {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = String::deserialize(d)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("configuration file could not be read")]
    Read,
    #[error("configuration file is invalid")]
    Parse,
    #[error(
        "database configuration is local-only and must use a localhost, loopback, or Unix socket host"
    )]
    RemoteDatabase,
    #[error("database URL is invalid")]
    InvalidDatabaseUrl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub paths: Paths,
}
#[derive(Clone, PartialEq, Eq, Deserialize, Default)]
pub struct DatabaseConfig {
    #[serde(default)]
    pub database_url: Option<DatabaseUrl>,
}
impl fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DatabaseConfig")
            .field("database_url", &self.database_url)
            .finish()
    }
}
impl Serialize for DatabaseConfig {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // Credentials are runtime input and are never written back to config serialization.
        s.serialize_struct("DatabaseConfig", 0)?.end()
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Paths {
    #[serde(default = "default_config_dir")]
    pub config_dir: PathBuf,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
}
impl Config {
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
        if let Ok(value) = env::var(DB_ENV) {
            config.database.database_url = Some(DatabaseUrl::parse(value)?);
        }
        if config.database.database_url.is_none() {
            config.database.database_url = Some(DatabaseUrl::parse(default_database_url())?);
        }
        config.validate().map(|()| config)
    }
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Some(url) = &self.database.database_url {
            validate_local_database_url(url.as_str())?;
        }
        Ok(())
    }
    pub fn integration_tests_enabled() -> bool {
        env::var(TEST_ENV).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    }
    pub fn redacted_database_error() -> RedactedDatabaseError {
        RedactedDatabaseError
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedactedDatabaseError;
impl fmt::Display for RedactedDatabaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("mg-todo database operation failed")
    }
}
impl std::error::Error for RedactedDatabaseError {}

/// The local socket authority used when nothing is configured.
#[must_use]
pub fn default_database_url() -> String {
    format!("postgresql:///{DEFAULT_DATABASE}?host={DEFAULT_SOCKET_DIR}")
}

pub(crate) fn validate_local_database_url(value: &str) -> Result<(), ConfigError> {
    let url = Url::parse(value).map_err(|_| ConfigError::InvalidDatabaseUrl)?;
    if !matches!(url.scheme(), "postgres" | "postgresql") {
        return Err(ConfigError::RemoteDatabase);
    }

    let authority_host = url.host_str().map(str::to_owned);
    if let Some(host) = &authority_host {
        validate_local_host(host)?;
    }

    let mut query_hosts = Vec::new();
    let mut query_hostaddrs = Vec::new();
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "host" => query_hosts.push(value.into_owned()),
            "hostaddr" => query_hostaddrs.push(value.into_owned()),
            _ => {}
        }
    }

    // libpq permits repeated options and host lists; validate every target rather than
    // trusting only the first query value or allowing a later value to redirect a connection.
    for host_value in &query_hosts {
        for host in host_value.split(',') {
            validate_local_host_or_socket(host)?;
        }
    }
    for hostaddr_value in &query_hostaddrs {
        for hostaddr in hostaddr_value.split(',') {
            validate_loopback_address(hostaddr)?;
        }
    }

    if authority_host.is_none() && query_hosts.is_empty() && query_hostaddrs.is_empty() {
        return Err(ConfigError::RemoteDatabase);
    }
    if authority_host.is_some() && query_hosts.iter().any(|value| value.starts_with('/')) {
        return Err(ConfigError::RemoteDatabase);
    }
    Ok(())
}

fn validate_local_host_or_socket(host: &str) -> Result<(), ConfigError> {
    if host.starts_with('/') {
        return (!host.contains('\0'))
            .then_some(())
            .ok_or(ConfigError::RemoteDatabase);
    }
    validate_local_host(host)
}

fn validate_local_host(host: &str) -> Result<(), ConfigError> {
    let host = host.trim_matches(['[', ']']);
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(());
    }
    validate_loopback_address(host)
}

fn validate_loopback_address(address: &str) -> Result<(), ConfigError> {
    address
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
        .then_some(())
        .ok_or(ConfigError::RemoteDatabase)
}

impl Paths {
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
    let path = paths.config_dir.join("config.toml");
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
    fn the_unconfigured_default_is_a_valid_local_socket_authority() {
        let url = default_database_url();
        assert!(url.contains(DEFAULT_DATABASE));
        assert!(url.contains(DEFAULT_SOCKET_DIR));
        assert!(validate_local_database_url(&url).is_ok());
        assert!(DatabaseUrl::parse(url).is_ok());
    }
    #[test]
    fn local_url_validation_is_exact() {
        for url in [
            "postgres://localhost/db",
            "postgres://127.0.0.1/db",
            "postgres://[::1]/db",
            "postgres://user:***@localhost/db",
            "postgresql:///db?host=%2Fvar%2Frun%2Fpostgresql",
            "postgres://localhost/db?host=localhost&hostaddr=127.0.0.1",
            "postgresql:///db?hostaddr=::1",
        ] {
            assert!(
                validate_local_database_url(url).is_ok(),
                "{url}: {:?}",
                validate_local_database_url(url)
            );
        }
        for url in [
            "postgres://localhost.evil/db",
            "postgres://localhost@evil.test/db",
            "postgres://example.test/db",
            "http://localhost/db",
            "postgres://localhost/db?host=evil",
            "postgres://localhost/db?host=localhost&host=evil",
            "postgres:///db?hostaddr=10.0.0.7",
            "postgres:///db?host=/var/run/postgresql&host=evil",
            "postgres://localhost/db?host=/var/run/postgresql",
        ] {
            assert!(validate_local_database_url(url).is_err(), "{url}");
        }
    }
    #[test]
    fn secrets_do_not_enter_debug_or_serde() {
        let c = DatabaseConfig {
            database_url: Some(
                DatabaseUrl::parse("postgres://user:***@localhost/db".into()).unwrap(),
            ),
        };
        assert!(!format!("{c:?}").contains("secret"));
        assert!(!toml::to_string(&c).unwrap().contains("secret"));
    }
    #[test]
    fn minimal_config_uses_defaults() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.database.database_url.is_none());
    }
}
