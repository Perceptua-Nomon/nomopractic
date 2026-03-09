// Configuration: CLI args → env vars → config file (priority order).

use std::path::{Path, PathBuf};
use std::{env, fs};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid {field}: {reason}")]
    Validation { field: &'static str, reason: String },
}

/// Daemon configuration.
///
/// Loaded from TOML file, overridden by `NOMON_HAT_*` environment variables.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub i2c_bus: u8,
    pub hat_address: u8,
    pub socket_path: PathBuf,
    pub socket_mode: u32,
    pub log_level: String,
    pub servo_default_ttl_ms: u64,
    pub watchdog_poll_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            i2c_bus: 1,
            hat_address: 0x14,
            socket_path: PathBuf::from("/run/nomopractic/nomopractic.sock"),
            socket_mode: 0o660,
            log_level: "info".into(),
            servo_default_ttl_ms: 500,
            watchdog_poll_ms: 100,
        }
    }
}

impl Config {
    /// Load configuration from a TOML file, then apply environment variable
    /// overrides. Returns compiled defaults if the file does not exist.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let mut config = if path.exists() {
            let contents = fs::read_to_string(path).map_err(|e| ConfigError::ReadFile {
                path: path.to_owned(),
                source: e,
            })?;
            toml::from_str(&contents).map_err(|e| ConfigError::Parse {
                path: path.to_owned(),
                source: e,
            })?
        } else {
            Self::default()
        };

        config.apply_env_overrides(|k| env::var(k));
        config.validate()?;
        Ok(config)
    }

    /// Override fields from `NOMON_HAT_*` environment variables.
    ///
    /// Accepts a `get_env` function so callers can inject a deterministic
    /// accessor in tests instead of mutating the process environment.
    fn apply_env_overrides<F>(&mut self, get_env: F)
    where
        F: for<'a> Fn(&'a str) -> Result<String, env::VarError>,
    {
        if let Ok(v) = get_env("NOMON_HAT_I2C_BUS")
            && let Ok(n) = v.parse()
        {
            self.i2c_bus = n;
        }
        if let Ok(v) = get_env("NOMON_HAT_ADDRESS") {
            // Accept "0x14" or "20" decimal.
            let n = if let Some(hex) = v.strip_prefix("0x") {
                u8::from_str_radix(hex, 16).ok()
            } else {
                v.parse().ok()
            };
            if let Some(n) = n {
                self.hat_address = n;
            }
        }
        if let Ok(v) = get_env("NOMON_HAT_SOCKET_PATH") {
            self.socket_path = PathBuf::from(v);
        }
        if let Ok(v) = get_env("NOMON_HAT_SOCKET_MODE")
            && let Ok(n) = u32::from_str_radix(&v, 8)
        {
            self.socket_mode = n;
        }
        if let Ok(v) = get_env("NOMON_HAT_LOG_LEVEL") {
            self.log_level = v;
        }
        if let Ok(v) = get_env("NOMON_HAT_SERVO_DEFAULT_TTL_MS")
            && let Ok(n) = v.parse()
        {
            self.servo_default_ttl_ms = n;
        }
        if let Ok(v) = get_env("NOMON_HAT_WATCHDOG_POLL_MS")
            && let Ok(n) = v.parse()
        {
            self.watchdog_poll_ms = n;
        }
    }

    /// Validate configuration values.
    fn validate(&self) -> Result<(), ConfigError> {
        const VALID_LOG_LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];
        if !VALID_LOG_LEVELS.contains(&self.log_level.to_lowercase().as_str()) {
            return Err(ConfigError::Validation {
                field: "log_level",
                reason: format!("'{}' is not one of {:?}", self.log_level, VALID_LOG_LEVELS),
            });
        }
        if self.servo_default_ttl_ms == 0 {
            return Err(ConfigError::Validation {
                field: "servo_default_ttl_ms",
                reason: "must be > 0".into(),
            });
        }
        if self.watchdog_poll_ms == 0 {
            return Err(ConfigError::Validation {
                field: "watchdog_poll_ms",
                reason: "must be > 0".into(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn defaults_are_valid() {
        let config = Config::default();
        assert_eq!(config.i2c_bus, 1);
        assert_eq!(config.hat_address, 0x14);
        assert_eq!(config.socket_mode, 0o660);
        assert_eq!(config.log_level, "info");
        assert_eq!(config.servo_default_ttl_ms, 500);
        assert_eq!(config.watchdog_poll_ms, 100);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let config = Config::load(Path::new("/nonexistent/config.toml")).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn load_toml_file() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(
            f,
            r#"
i2c_bus = 2
hat_address = 0x15
log_level = "debug"
servo_default_ttl_ms = 1000
"#
        )
        .unwrap();

        let config = Config::load(f.path()).unwrap();
        assert_eq!(config.i2c_bus, 2);
        assert_eq!(config.hat_address, 0x15);
        assert_eq!(config.log_level, "debug");
        assert_eq!(config.servo_default_ttl_ms, 1000);
        // Non-specified fields keep defaults.
        assert_eq!(config.watchdog_poll_ms, 100);
    }

    #[test]
    fn load_empty_file_returns_defaults() {
        let f = NamedTempFile::new().unwrap();
        let config = Config::load(f.path()).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn invalid_log_level_rejected() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, r#"log_level = "verbose""#).unwrap();
        let err = Config::load(f.path()).unwrap_err();
        assert!(err.to_string().contains("log_level"));
    }

    #[test]
    fn zero_ttl_rejected() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "servo_default_ttl_ms = 0").unwrap();
        let err = Config::load(f.path()).unwrap_err();
        assert!(err.to_string().contains("servo_default_ttl_ms"));
    }

    #[test]
    fn zero_watchdog_rejected() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "watchdog_poll_ms = 0").unwrap();
        let err = Config::load(f.path()).unwrap_err();
        assert!(err.to_string().contains("watchdog_poll_ms"));
    }

    #[test]
    fn malformed_toml_rejected() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "this is not valid toml {{{{").unwrap();
        let err = Config::load(f.path()).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn env_overrides_apply() {
        let mut config = Config::default();
        config.apply_env_overrides(|key| match key {
            "NOMON_HAT_I2C_BUS" => Ok("3".into()),
            "NOMON_HAT_ADDRESS" => Ok("0x20".into()),
            "NOMON_HAT_LOG_LEVEL" => Ok("debug".into()),
            _ => Err(env::VarError::NotPresent),
        });

        assert_eq!(config.i2c_bus, 3);
        assert_eq!(config.hat_address, 0x20);
        assert_eq!(config.log_level, "debug");
    }
}
