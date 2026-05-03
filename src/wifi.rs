//! WiFi provisioning — scan, connect, and status via `nmcli`.
//!
//! Uses `std::process::Command` to interact with NetworkManager, following
//! the same subprocess pattern as `hat/audio.rs` (AmixerControl).  The
//! [`WifiControl`] trait enables mock injection in tests.
//!
//! This module is **not** behind the `ble` feature flag — WiFi IPC methods
//! should work even without BLE compiled in.

use serde::Serialize;
use thiserror::Error;

// ── Types ──────────────────────────────────────────────────────────────

/// A WiFi network discovered during a scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WifiNetwork {
    /// Network SSID (may be empty for hidden networks).
    pub ssid: String,
    /// Signal strength (0–100 percentage from `nmcli`).
    pub signal_pct: u8,
    /// Security type string (e.g. "WPA2", "WPA3", "").
    pub security: String,
}

/// Current WiFi connection state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WifiStatus {
    Disconnected,
    Connected { ssid: String, signal_pct: u8 },
}

// ── Errors ─────────────────────────────────────────────────────────────

/// Errors from WiFi provisioning operations.
#[derive(Debug, Error)]
pub enum WifiError {
    #[error("WiFi scan failed: {0}")]
    ScanFailed(String),
    #[error("WiFi connection failed: {0}")]
    ConnectionFailed(String),
    #[error("nmcli command failed: {0}")]
    CommandFailed(String),
}

// ── Trait ───────────────────────────────────────────────────────────────

/// Abstraction over WiFi operations for mock injection in tests.
pub trait WifiControl: Send + Sync {
    /// Scan for available WiFi networks.
    fn scan(&self) -> Result<Vec<WifiNetwork>, WifiError>;
    /// Connect to a WiFi network.
    fn connect(&self, ssid: &str, password: &str) -> Result<(), WifiError>;
    /// Get the current WiFi connection status.
    fn status(&self) -> Result<WifiStatus, WifiError>;
}

// ── nmcli implementation ───────────────────────────────────────────────

/// WiFi control via NetworkManager CLI (`nmcli`).
pub struct NmcliWifi;

impl WifiControl for NmcliWifi {
    fn scan(&self) -> Result<Vec<WifiNetwork>, WifiError> {
        let output = std::process::Command::new("nmcli")
            .args(["-t", "-f", "SSID,SIGNAL,SECURITY", "dev", "wifi", "list"])
            .output()
            .map_err(|e| WifiError::CommandFailed(e.to_string()))?;

        if !output.status.success() {
            return Err(WifiError::ScanFailed(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }

        Ok(parse_scan_output(&String::from_utf8_lossy(&output.stdout)))
    }

    fn connect(&self, ssid: &str, password: &str) -> Result<(), WifiError> {
        // Use argument list (no shell interpolation) — security checklist P2.
        let output = std::process::Command::new("nmcli")
            .args(["dev", "wifi", "connect", ssid, "password", password])
            .output()
            .map_err(|e| WifiError::CommandFailed(e.to_string()))?;

        if !output.status.success() {
            return Err(WifiError::ConnectionFailed(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }

        Ok(())
    }

    fn status(&self) -> Result<WifiStatus, WifiError> {
        let output = std::process::Command::new("nmcli")
            .args(["-t", "-f", "ACTIVE,SSID,SIGNAL", "dev", "wifi"])
            .output()
            .map_err(|e| WifiError::CommandFailed(e.to_string()))?;

        if !output.status.success() {
            return Err(WifiError::CommandFailed(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }

        Ok(parse_status_output(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }
}

// ── Mock implementation ────────────────────────────────────────────────

/// Mock WiFi control for testing.
pub struct MockWifiControl {
    /// Networks returned by `scan()`.
    pub scan_result: Result<Vec<WifiNetwork>, String>,
    /// Result returned by `connect()`.
    pub connect_result: Result<(), String>,
    /// Status returned by `status()`.
    pub status_result: Result<WifiStatus, String>,
}

impl Default for MockWifiControl {
    fn default() -> Self {
        Self {
            scan_result: Ok(vec![
                WifiNetwork {
                    ssid: "TestNetwork".into(),
                    signal_pct: 85,
                    security: "WPA2".into(),
                },
                WifiNetwork {
                    ssid: "OpenWifi".into(),
                    signal_pct: 42,
                    security: String::new(),
                },
            ]),
            connect_result: Ok(()),
            status_result: Ok(WifiStatus::Connected {
                ssid: "TestNetwork".into(),
                signal_pct: 85,
            }),
        }
    }
}

impl WifiControl for MockWifiControl {
    fn scan(&self) -> Result<Vec<WifiNetwork>, WifiError> {
        self.scan_result.clone().map_err(WifiError::ScanFailed)
    }

    fn connect(&self, _ssid: &str, _password: &str) -> Result<(), WifiError> {
        self.connect_result
            .clone()
            .map_err(WifiError::ConnectionFailed)
    }

    fn status(&self) -> Result<WifiStatus, WifiError> {
        self.status_result.clone().map_err(WifiError::CommandFailed)
    }
}

// ── Parsing ────────────────────────────────────────────────────────────

/// Parse `nmcli -t -f SSID,SIGNAL,SECURITY dev wifi list` output.
fn parse_scan_output(output: &str) -> Vec<WifiNetwork> {
    let mut networks = Vec::new();
    for line in output.lines() {
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() >= 2 {
            let ssid = parts[0].to_string();
            let signal_pct = parts[1].parse::<u8>().unwrap_or(0);
            let security = parts.get(2).unwrap_or(&"").to_string();
            // Skip empty SSIDs (hidden networks) in the list.
            if !ssid.is_empty() {
                networks.push(WifiNetwork {
                    ssid,
                    signal_pct,
                    security,
                });
            }
        }
    }
    networks
}

/// Parse `nmcli -t -f ACTIVE,SSID,SIGNAL dev wifi` output to find the
/// active connection.
fn parse_status_output(output: &str) -> WifiStatus {
    for line in output.lines() {
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() >= 3 && parts[0] == "yes" {
            let ssid = parts[1].to_string();
            let signal_pct = parts[2].parse::<u8>().unwrap_or(0);
            return WifiStatus::Connected { ssid, signal_pct };
        }
    }
    WifiStatus::Disconnected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scan_output_multiple_networks() {
        let output = "HomeNetwork:85:WPA2\nCoffeeShop:42:WPA2\nOpenWifi:30:\n";
        let networks = parse_scan_output(output);
        assert_eq!(networks.len(), 3);
        assert_eq!(networks[0].ssid, "HomeNetwork");
        assert_eq!(networks[0].signal_pct, 85);
        assert_eq!(networks[0].security, "WPA2");
        assert_eq!(networks[1].ssid, "CoffeeShop");
        assert_eq!(networks[1].signal_pct, 42);
        assert_eq!(networks[2].ssid, "OpenWifi");
        assert_eq!(networks[2].signal_pct, 30);
        assert_eq!(networks[2].security, "");
    }

    #[test]
    fn parse_scan_output_empty() {
        let networks = parse_scan_output("");
        assert!(networks.is_empty());
    }

    #[test]
    fn parse_scan_output_skips_hidden() {
        let output = ":50:WPA2\nVisible:80:WPA3\n";
        let networks = parse_scan_output(output);
        assert_eq!(networks.len(), 1);
        assert_eq!(networks[0].ssid, "Visible");
    }

    #[test]
    fn parse_status_connected() {
        let output = "no:OtherNet:50\nyes:MyNetwork:90\n";
        let status = parse_status_output(output);
        assert_eq!(
            status,
            WifiStatus::Connected {
                ssid: "MyNetwork".into(),
                signal_pct: 90,
            }
        );
    }

    #[test]
    fn parse_status_disconnected() {
        let output = "no:SomeNet:50\n";
        let status = parse_status_output(output);
        assert_eq!(status, WifiStatus::Disconnected);
    }

    #[test]
    fn parse_status_empty() {
        let status = parse_status_output("");
        assert_eq!(status, WifiStatus::Disconnected);
    }

    #[test]
    fn mock_wifi_scan_returns_default_networks() {
        let mock = MockWifiControl::default();
        let networks = mock.scan().unwrap();
        assert_eq!(networks.len(), 2);
        assert_eq!(networks[0].ssid, "TestNetwork");
    }

    #[test]
    fn mock_wifi_connect_succeeds() {
        let mock = MockWifiControl::default();
        assert!(mock.connect("Test", "pass").is_ok());
    }

    #[test]
    fn mock_wifi_status_returns_connected() {
        let mock = MockWifiControl::default();
        let status = mock.status().unwrap();
        assert_eq!(
            status,
            WifiStatus::Connected {
                ssid: "TestNetwork".into(),
                signal_pct: 85,
            }
        );
    }
}
