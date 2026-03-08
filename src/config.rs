// Configuration: CLI args → env vars → config file (priority order).

use std::path::PathBuf;

use serde::Deserialize;

/// Daemon configuration.
///
/// Loaded from TOML file, overridden by `NOMON_HAT_*` environment variables.
#[derive(Debug, Clone, Deserialize)]
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
