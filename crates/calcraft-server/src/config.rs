//! Server configuration.
//!
//! Sources, in increasing priority: built-in defaults, TOML file, environment
//! variables (`CALCRAFT_` prefix, `__` as section separator) and CLI flags.
//! Unknown keys are rejected so that typos in security-relevant settings do
//! not go unnoticed.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::{Deserialize, Serialize};

/// Environment variable prefix for configuration overrides.
pub const ENV_PREFIX: &str = "CALCRAFT_";

/// Complete server configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub security: SecurityConfig,
    pub web_ui: WebUiConfig,
    pub api: ApiConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Address to listen on. Defaults to loopback only.
    pub bind: SocketAddr,
    /// Maximum accepted request body size in bytes.
    pub max_body_bytes: usize,
    /// Maximum time to handle a single request, in seconds.
    pub request_timeout_secs: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 5232)),
            max_body_bytes: 10 * 1024 * 1024,
            request_timeout_secs: 30,
        }
    }
}

impl ServerConfig {
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// Path of the SQLite database file. Created on first start.
    pub database: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            database: PathBuf::from("calcraft.db"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityConfig {
    /// Send `Strict-Transport-Security`. Enable only when served over HTTPS.
    pub hsts: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebUiConfig {
    pub enabled: bool,
}

impl Default for WebUiConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiConfig {
    pub enabled: bool,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    /// Log filter, e.g. `info` or `calcraft_server=debug`. `RUST_LOG` takes precedence.
    pub level: String,
    pub format: LogFormat,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_owned(),
            format: LogFormat::Pretty,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Pretty,
    Json,
}

/// Errors while loading or validating the configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("configuration file {0} does not exist")]
    FileNotFound(String),
    #[error(transparent)]
    Parse(#[from] Box<figment::Error>),
    #[error("invalid configuration: {0}")]
    Invalid(&'static str),
}

impl Config {
    /// Loads the configuration from defaults, the optional file and the environment.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let mut figment = Figment::from(Serialized::defaults(Config::default()));
        if let Some(path) = path {
            if !path.is_file() {
                return Err(ConfigError::FileNotFound(path.display().to_string()));
            }
            figment = figment.merge(Toml::file(path));
        }
        // `CALCRAFT_CONFIG` selects the file and is not a configuration key.
        figment = figment.merge(Env::prefixed(ENV_PREFIX).ignore(&["config"]).split("__"));
        Self::from_figment(figment)
    }

    fn from_figment(figment: Figment) -> Result<Self, ConfigError> {
        let config: Config = figment.extract().map_err(Box::new)?;
        config.validate()?;
        Ok(config)
    }

    /// Checks invariants that serde cannot express.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.server.max_body_bytes == 0 {
            return Err(ConfigError::Invalid(
                "server.max_body_bytes must be greater than 0",
            ));
        }
        if self.server.request_timeout_secs == 0 {
            return Err(ConfigError::Invalid(
                "server.request_timeout_secs must be greater than 0",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_toml(toml: &str) -> Result<Config, ConfigError> {
        Config::from_figment(
            Figment::from(Serialized::defaults(Config::default())).merge(Toml::string(toml)),
        )
    }

    #[test]
    fn defaults_are_secure() {
        let config = Config::default();
        assert!(config.server.bind.ip().is_loopback());
        assert!(!config.security.hsts);
        config.validate().unwrap();
    }

    #[test]
    fn partial_file_keeps_defaults() {
        let config = from_toml("[web_ui]\nenabled = false\n").unwrap();
        assert!(!config.web_ui.enabled);
        assert!(config.api.enabled);
        assert_eq!(config.server, ServerConfig::default());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(from_toml("[security]\nhts = true\n").is_err());
        assert!(from_toml("[unknown]\nkey = 1\n").is_err());
    }

    #[test]
    fn zero_limits_are_rejected() {
        assert!(from_toml("[server]\nmax_body_bytes = 0\n").is_err());
        assert!(from_toml("[server]\nrequest_timeout_secs = 0\n").is_err());
    }

    #[test]
    fn missing_file_is_an_error() {
        let result = Config::load(Some(Path::new("does-not-exist.toml")));
        assert!(matches!(result, Err(ConfigError::FileNotFound(_))));
    }
}
