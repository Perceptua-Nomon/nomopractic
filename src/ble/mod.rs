//! BLE GATT server module — JSON relay over a single GATT service.
//!
//! Implements the simplified BLE architecture from ADR-004:
//! - OS-level Bluetooth passkey pairing (BlueZ agent)
//! - Single GATT service with Command Write and Response Notify characteristics
//! - NDJSON relay — same format as Unix socket IPC
//!
//! All sub-modules require the `ble` Cargo feature flag (BlueZ D-Bus
//! bindings via `bluer`).

#[cfg(feature = "ble")]
pub mod bridge;
#[cfg(feature = "ble")]
pub use bridge::BLE_CONN_ID;
#[cfg(feature = "ble")]
pub mod services;

#[cfg(feature = "ble")]
use std::sync::Arc;

#[cfg(feature = "ble")]
use thiserror::Error;
#[cfg(feature = "ble")]
use tracing::{error, info, warn};

#[cfg(feature = "ble")]
use crate::config::BleConfig;
#[cfg(feature = "ble")]
use crate::ipc::handler::Handler;
#[cfg(feature = "ble")]
use std::fs;
#[cfg(feature = "ble")]
use std::io::Read;
#[cfg(feature = "ble")]
use std::os::unix::fs::PermissionsExt;
#[cfg(feature = "ble")]
use std::path::Path;

/// BLE server errors.
#[cfg(feature = "ble")]
#[derive(Debug, Error)]
pub enum BleError {
    #[error("BlueZ D-Bus error: {0}")]
    Dbus(#[from] bluer::Error),
    #[error("BLE not available: {0}")]
    NotAvailable(String),
    #[error("invalid passkey: {0}")]
    InvalidPasskey(String),
}

/// Start the BLE GATT server with OS-level passkey pairing.
///
/// Registers a BlueZ passkey agent that reads the numeric passkey from
/// `config.pairing_secret_path`, starts LE advertising with the device name
/// and service UUID, and registers a single GATT service with Command Write
/// and Response Notify characteristics.
///
/// Incoming BLE writes are accumulated into complete NDJSON lines and
/// dispatched through the shared [`Handler`].  Responses are chunked at
/// the MTU boundary and sent as notifications.
///
/// On shutdown or client disconnect, clears all motor/servo leases held
/// by the BLE connection.
#[cfg(feature = "ble")]
pub async fn start_ble_server(
    config: &BleConfig,
    handler: Arc<Handler>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<(), BleError> {
    use bluer::adv::Advertisement;
    use bluer::agent::{Agent, AgentHandle};

    let bt_session = bluer::Session::new().await?;
    let adapter = bt_session.default_adapter().await?;
    adapter.set_powered(true).await?;

    info!(
        adapter = %adapter.name(),
        device_name = %config.device_name,
        "BLE adapter initialised"
    );

    // Read the numeric passkey from the pairing secret file.
    // Ensure a pairing secret exists so nomopractic can run standalone in
    // developer/test environments where `nomothetic` may not have created
    // the shared file yet. This attempts to create the directory and
    // seed a random 6-digit passkey if the file is missing. Failures are
    // non-fatal here (we'll surface them when reading the file below).
    if let Err(e) = ensure_pairing_secret(&config.pairing_secret_path) {
        warn!(error = %e, "could not ensure pairing secret exists; continuing and will attempt to read it")
    }

    let passkey = read_passkey(&config.pairing_secret_path)?;
    info!(
        "BLE passkey loaded from {}",
        config.pairing_secret_path.display()
    );

    // Register BlueZ passkey agent and request that it become the default
    // agent for passkey pairing.
    let agent = Agent {
        request_default: true,
        request_passkey: Some(Box::new(move |_req| {
            let pk = passkey;
            info!("BLE passkey requested by BlueZ agent");
            Box::pin(async move { Ok(pk) })
        })),
        ..Default::default()
    };
    let _agent_handle: AgentHandle = bt_session.register_agent(agent).await?;
    info!("BlueZ passkey agent registered (KeyboardOnly)");

    // Build GATT application with single service.
    let (app, char_handles) = services::build_gatt_application();

    let _app_handle = adapter.serve_gatt_application(app).await?;

    // Start LE advertising.
    let le_adv = Advertisement {
        advertisement_type: bluer::adv::Type::Peripheral,
        local_name: Some(config.device_name.clone()),
        service_uuids: services::advertised_service_uuids().into_iter().collect(),
        ..Default::default()
    };
    let _adv_handle = adapter.advertise(le_adv).await?;

    info!(device_name = %config.device_name, "BLE GATT server advertising");

    // Spawn the NDJSON bridge I/O task.
    let handler_clone = handler.clone();
    let io_task = tokio::spawn(async move {
        if let Err(e) = bridge::run_json_relay(char_handles, handler_clone).await {
            error!(error = %e, "BLE JSON relay error");
        }
    });

    // Wait for shutdown signal.
    let _ = shutdown.changed().await;

    // Cleanup: release all BLE leases.
    handler.on_client_disconnect(bridge::BLE_CONN_ID).await;

    // Abort the I/O task.
    io_task.abort();

    info!("BLE GATT server stopped");

    Ok(())
}

/// Attempt to create the pairing secret file if it does not exist.
///
/// This will try to create the parent directory, write a random 6-digit
/// numeric passkey, and set permissive permissions (0640). Errors are
/// returned so callers can log or handle them; creation failures are not
/// treated as fatal by callers to allow deployments that manage the
/// directory via tmpfiles.d to proceed.
#[cfg(feature = "ble")]
fn ensure_pairing_secret(path: &Path) -> Result<(), std::io::Error> {
    if path.exists() {
        return Ok(());
    }

    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        // try to create the directory; may fail if not permitted
        fs::create_dir_all(parent)?;
        // try to set reasonable directory permissions
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o750));
    }

    // Generate a 6-digit numeric passkey from /dev/urandom if available.
    let pk = match fs::File::open("/dev/urandom") {
        Ok(mut f) => {
            let mut buf = [0u8; 4];
            if f.read_exact(&mut buf).is_ok() {
                u32::from_le_bytes(buf) % 1_000_000
            } else {
                // NOTE: This fallback is only reached on non-Linux dev machines where /dev/urandom is unavailable; entropy quality is intentionally low in that case.
                // fallback to time-based seed
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos()
                    % 1_000_000
            }
        }
        Err(_) => {
            // NOTE: This fallback is only reached on non-Linux dev machines where /dev/urandom is unavailable; entropy quality is intentionally low in that case.
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
                % 1_000_000
        }
    };

    let tmp = path.with_extension(".tmp");
    fs::write(&tmp, format!("{:06}\n", pk))?;
    let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o640));
    fs::rename(&tmp, path)?;
    info!(path = %path.display(), "created pairing secret file");
    Ok(())
}

/// Read the numeric passkey (000000–999999) from the pairing secret file.
#[cfg(feature = "ble")]
fn read_passkey(path: &std::path::Path) -> Result<u32, BleError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| BleError::InvalidPasskey(format!("cannot read {}: {e}", path.display())))?;
    let trimmed = content.trim();
    let passkey: u32 = trimmed.parse().map_err(|_| {
        BleError::InvalidPasskey(format!(
            "passkey must be a numeric value 000000–999999, got '{trimmed}'"
        ))
    })?;
    if passkey > 999_999 {
        return Err(BleError::InvalidPasskey(format!(
            "passkey {passkey} exceeds maximum 999999"
        )));
    }
    Ok(passkey)
}

#[cfg(all(test, feature = "ble"))]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_passkey_ok() {
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        write!(tmp, "012345\n").expect("write passkey");
        let pk = read_passkey(tmp.path()).expect("should parse passkey");
        assert_eq!(pk, 12345);
    }

    #[test]
    fn read_passkey_missing_file() {
        let path = std::path::Path::new("/tmp/this_file_should_not_exist_hopefully");
        let res = read_passkey(path);
        assert!(matches!(res, Err(BleError::InvalidPasskey(_))));
    }

    #[test]
    fn read_passkey_invalid_content() {
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        write!(tmp, "not-a-number").expect("write invalid");
        let res = read_passkey(tmp.path());
        assert!(matches!(res, Err(BleError::InvalidPasskey(_))));
    }

    #[test]
    fn read_passkey_too_large() {
        let mut tmp = tempfile::NamedTempFile::new().expect("create temp file");
        write!(tmp, "1000000").expect("write large");
        let res = read_passkey(tmp.path());
        assert!(matches!(res, Err(BleError::InvalidPasskey(_))));
    }
}
