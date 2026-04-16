//! WiFi provisioning over BLE — scan, connect, and status via `nmcli`.
//!
//! Uses `std::process::Command` to interact with NetworkManager, following
//! the same subprocess pattern as `hat/audio.rs` (AmixerControl).  The
//! [`WifiControl`] trait enables mock injection in tests.

use thiserror::Error;

// ── Types ──────────────────────────────────────────────────────────────

/// A WiFi network discovered during a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiNetwork {
    /// Network SSID (may be empty for hidden networks).
    pub ssid: String,
    /// Signal strength (0–100 percentage from `nmcli`).
    pub signal: u8,
    /// Security type string (e.g. "WPA2", "WPA3", "").
    pub security: String,
}

/// Current WiFi connection state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WifiStatus {
    Disconnected,
    Connecting,
    Connected { ssid: String, signal: u8 },
}

/// WiFi command variants from the BLE WiFi Command characteristic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WifiCommand {
    /// Scan for available networks.
    Scan,
    /// Connect to a network with the given SSID and password.
    Connect { ssid: String, password: String },
    /// Query the current connection status.
    Status,
}

/// WiFi operation result variants.
#[derive(Debug, Clone, PartialEq)]
pub enum WifiResult {
    ScanResult(Vec<WifiNetwork>),
    ConnectResult { success: bool },
    StatusResult(WifiStatus),
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

// ── Parsing ────────────────────────────────────────────────────────────

/// Parse `nmcli -t -f SSID,SIGNAL,SECURITY dev wifi list` output.
fn parse_scan_output(output: &str) -> Vec<WifiNetwork> {
    let mut networks = Vec::new();
    for line in output.lines() {
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() >= 2 {
            let ssid = parts[0].to_string();
            let signal = parts[1].parse::<u8>().unwrap_or(0);
            let security = parts.get(2).unwrap_or(&"").to_string();
            // Skip empty SSIDs (hidden networks) in the list.
            if !ssid.is_empty() {
                networks.push(WifiNetwork {
                    ssid,
                    signal,
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
            let signal = parts[2].parse::<u8>().unwrap_or(0);
            return WifiStatus::Connected { ssid, signal };
        }
    }
    WifiStatus::Disconnected
}

// ── Binary encoding ────────────────────────────────────────────────────

/// Encode a WiFi scan result for the WiFi Result BLE characteristic.
///
/// Format: `count (u8) || [ssid_len (u8) || ssid_bytes || signal (u8)] × N`
pub fn encode_scan_result(networks: &[WifiNetwork]) -> Vec<u8> {
    let mut buf = Vec::new();
    let count = networks.len().min(255) as u8;
    buf.push(count);
    for net in networks.iter().take(count as usize) {
        let ssid_bytes = net.ssid.as_bytes();
        let ssid_len = ssid_bytes.len().min(255) as u8;
        buf.push(ssid_len);
        buf.extend_from_slice(&ssid_bytes[..ssid_len as usize]);
        buf.push(net.signal);
    }
    buf
}

/// Encode a WiFi status for the WiFi Result BLE characteristic.
///
/// Format: `state (u8) || signal (u8) || ssid_len (u8) || ssid_bytes`
///
/// State: `0x00` = disconnected, `0x01` = connecting, `0x02` = connected.
pub fn encode_wifi_status(status: &WifiStatus) -> Vec<u8> {
    match status {
        WifiStatus::Disconnected => vec![0x00, 0, 0],
        WifiStatus::Connecting => vec![0x01, 0, 0],
        WifiStatus::Connected { ssid, signal } => {
            let ssid_bytes = ssid.as_bytes();
            let ssid_len = ssid_bytes.len().min(255) as u8;
            let mut buf = Vec::with_capacity(3 + ssid_len as usize);
            buf.push(0x02);
            buf.push(*signal);
            buf.push(ssid_len);
            buf.extend_from_slice(&ssid_bytes[..ssid_len as usize]);
            buf
        }
    }
}

/// Decode a WiFi command from the WiFi Command characteristic payload.
///
/// Format:
/// - `0x01` = Scan (no additional data)
/// - `0x02` = Connect: `cmd(1) || ssid_len(1) || ssid(N) || pwd_len(1) || pwd(M)`
/// - `0x03` = Status (no additional data)
pub fn decode_wifi_command(data: &[u8]) -> Option<WifiCommand> {
    let &cmd = data.first()?;
    match cmd {
        0x01 => Some(WifiCommand::Scan),
        0x02 => {
            if data.len() < 2 {
                return None;
            }
            let ssid_len = data[1] as usize;
            if data.len() < 2 + ssid_len + 1 {
                return None;
            }
            let ssid = std::str::from_utf8(&data[2..2 + ssid_len])
                .ok()?
                .to_string();
            let pwd_len = data[2 + ssid_len] as usize;
            if data.len() < 2 + ssid_len + 1 + pwd_len {
                return None;
            }
            let password = std::str::from_utf8(&data[2 + ssid_len + 1..2 + ssid_len + 1 + pwd_len])
                .ok()?
                .to_string();
            Some(WifiCommand::Connect { ssid, password })
        }
        0x03 => Some(WifiCommand::Status),
        _ => None,
    }
}

/// Encode a [`WifiResult`] for the WiFi Result BLE characteristic.
///
/// Prepends a 1-byte result type before the type-specific payload:
/// - `0x01` + scan payload
/// - `0x02` + success byte
/// - `0x03` + status payload
pub fn encode_wifi_result(result: &WifiResult) -> Vec<u8> {
    match result {
        WifiResult::ScanResult(networks) => {
            let mut buf = vec![0x01];
            buf.extend_from_slice(&encode_scan_result(networks));
            buf
        }
        WifiResult::ConnectResult { success } => {
            vec![0x02, u8::from(*success)]
        }
        WifiResult::StatusResult(status) => {
            let mut buf = vec![0x03];
            buf.extend_from_slice(&encode_wifi_status(status));
            buf
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Parse tests ────────────────────────────────────────────────────

    #[test]
    fn parse_scan_output_multiple_networks() {
        let output = "HomeNetwork:85:WPA2\nCoffeeShop:42:WPA2\nOpenWifi:30:\n";
        let networks = parse_scan_output(output);
        assert_eq!(networks.len(), 3);
        assert_eq!(networks[0].ssid, "HomeNetwork");
        assert_eq!(networks[0].signal, 85);
        assert_eq!(networks[0].security, "WPA2");
        assert_eq!(networks[1].ssid, "CoffeeShop");
        assert_eq!(networks[1].signal, 42);
        assert_eq!(networks[2].ssid, "OpenWifi");
        assert_eq!(networks[2].signal, 30);
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
                signal: 90,
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

    // ── Binary encoding tests ──────────────────────────────────────────

    #[test]
    fn encode_scan_result_roundtrip() {
        let networks = vec![
            WifiNetwork {
                ssid: "Test".into(),
                signal: 75,
                security: "WPA2".into(),
            },
            WifiNetwork {
                ssid: "Net2".into(),
                signal: 42,
                security: "".into(),
            },
        ];
        let encoded = encode_scan_result(&networks);
        assert_eq!(encoded[0], 2); // count
        assert_eq!(encoded[1], 4); // "Test" length
        assert_eq!(&encoded[2..6], b"Test");
        assert_eq!(encoded[6], 75); // signal
        assert_eq!(encoded[7], 4); // "Net2" length
        assert_eq!(&encoded[8..12], b"Net2");
        assert_eq!(encoded[12], 42); // signal
    }

    #[test]
    fn encode_wifi_status_disconnected() {
        let encoded = encode_wifi_status(&WifiStatus::Disconnected);
        assert_eq!(encoded[0], 0x00);
    }

    #[test]
    fn encode_wifi_status_connected() {
        let encoded = encode_wifi_status(&WifiStatus::Connected {
            ssid: "MyNet".into(),
            signal: 80,
        });
        assert_eq!(encoded[0], 0x02);
        assert_eq!(encoded[1], 80);
        assert_eq!(encoded[2], 5); // "MyNet" length
        assert_eq!(&encoded[3..8], b"MyNet");
    }

    // ── WiFi result encoding tests ─────────────────────────────────────

    #[test]
    fn encode_wifi_result_scan() {
        let result = WifiResult::ScanResult(vec![WifiNetwork {
            ssid: "Net".into(),
            signal: 50,
            security: "WPA2".into(),
        }]);
        let encoded = encode_wifi_result(&result);
        assert_eq!(encoded[0], 0x01); // result type
        assert_eq!(encoded[1], 1); // network count
    }

    #[test]
    fn encode_wifi_result_connect_success() {
        let result = WifiResult::ConnectResult { success: true };
        let encoded = encode_wifi_result(&result);
        assert_eq!(encoded, [0x02, 0x01]);
    }

    #[test]
    fn encode_wifi_result_connect_failure() {
        let result = WifiResult::ConnectResult { success: false };
        let encoded = encode_wifi_result(&result);
        assert_eq!(encoded, [0x02, 0x00]);
    }

    #[test]
    fn encode_wifi_result_status() {
        let result = WifiResult::StatusResult(WifiStatus::Disconnected);
        let encoded = encode_wifi_result(&result);
        assert_eq!(encoded[0], 0x03); // result type
        assert_eq!(encoded[1], 0x00); // disconnected
    }

    // ── Command decode tests ───────────────────────────────────────────

    #[test]
    fn decode_wifi_command_scan() {
        let cmd = decode_wifi_command(&[0x01]);
        assert!(matches!(cmd, Some(WifiCommand::Scan)));
    }

    #[test]
    fn decode_wifi_command_connect() {
        // cmd(0x02) || ssid_len(4) || "Test" || pwd_len(6) || "secret"
        let data = [
            0x02, 4, b'T', b'e', b's', b't', 6, b's', b'e', b'c', b'r', b'e', b't',
        ];
        let cmd = decode_wifi_command(&data);
        assert!(matches!(
            cmd,
            Some(WifiCommand::Connect { ref ssid, ref password })
            if ssid == "Test" && password == "secret"
        ));
    }

    #[test]
    fn decode_wifi_command_connect_empty_password() {
        // cmd(0x02) || ssid_len(2) || "AB" || pwd_len(0)
        let data = [0x02, 2, b'A', b'B', 0];
        let cmd = decode_wifi_command(&data);
        assert!(matches!(
            cmd,
            Some(WifiCommand::Connect { ref ssid, ref password })
            if ssid == "AB" && password.is_empty()
        ));
    }

    #[test]
    fn decode_wifi_command_connect_too_short() {
        // Only command byte, no ssid_len
        assert!(decode_wifi_command(&[0x02]).is_none());
        // ssid_len says 5 but only 2 bytes follow
        assert!(decode_wifi_command(&[0x02, 5, b'A', b'B']).is_none());
    }

    #[test]
    fn decode_wifi_command_status() {
        let cmd = decode_wifi_command(&[0x03]);
        assert!(matches!(cmd, Some(WifiCommand::Status)));
    }

    #[test]
    fn decode_wifi_command_invalid() {
        assert!(decode_wifi_command(&[0xFF]).is_none());
    }

    #[test]
    fn decode_wifi_command_empty() {
        assert!(decode_wifi_command(&[]).is_none());
    }

    // ── Mock WiFi control tests ────────────────────────────────────────

    struct MockWifi {
        scan_result: Vec<WifiNetwork>,
        connect_ok: bool,
        status: WifiStatus,
    }

    impl WifiControl for MockWifi {
        fn scan(&self) -> Result<Vec<WifiNetwork>, WifiError> {
            Ok(self.scan_result.clone())
        }
        fn connect(&self, _ssid: &str, _password: &str) -> Result<(), WifiError> {
            if self.connect_ok {
                Ok(())
            } else {
                Err(WifiError::ConnectionFailed("mock failure".into()))
            }
        }
        fn status(&self) -> Result<WifiStatus, WifiError> {
            Ok(self.status.clone())
        }
    }

    #[test]
    fn mock_wifi_scan() {
        let mock = MockWifi {
            scan_result: vec![WifiNetwork {
                ssid: "TestNet".into(),
                signal: 90,
                security: "WPA2".into(),
            }],
            connect_ok: true,
            status: WifiStatus::Disconnected,
        };
        let networks = mock.scan().unwrap();
        assert_eq!(networks.len(), 1);
        assert_eq!(networks[0].ssid, "TestNet");
    }

    #[test]
    fn mock_wifi_connect_failure() {
        let mock = MockWifi {
            scan_result: vec![],
            connect_ok: false,
            status: WifiStatus::Disconnected,
        };
        let result = mock.connect("net", "pass");
        assert!(result.is_err());
    }

    #[test]
    fn mock_wifi_status_connected() {
        let mock = MockWifi {
            scan_result: vec![],
            connect_ok: true,
            status: WifiStatus::Connected {
                ssid: "HomeNet".into(),
                signal: 85,
            },
        };
        let status = mock.status().unwrap();
        assert_eq!(
            status,
            WifiStatus::Connected {
                ssid: "HomeNet".into(),
                signal: 85,
            }
        );
    }
}
