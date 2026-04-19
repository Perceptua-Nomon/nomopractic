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
    use bluer::agent::{Agent, AgentHandle, ReqError as AgentReqError};

    let bt_session = bluer::Session::new().await?;
    let adapter = bt_session.default_adapter().await?;
    adapter.set_powered(true).await?;

    info!(
        adapter = %adapter.name(),
        device_name = %config.device_name,
        "BLE adapter initialised"
    );

    // Read the numeric passkey from the pairing secret file.
    let passkey = read_passkey(&config.pairing_secret_path)?;
    info!(
        "BLE passkey loaded from {}",
        config.pairing_secret_path.display()
    );

    // Register BlueZ passkey agent.
    let agent = Agent {
        request_passkey: Some(Box::new(move |_req| {
            let pk = passkey;
            Box::pin(async move { Ok(pk) })
        })),
        ..Default::default()
    };
    let _agent_handle: AgentHandle = bt_session.register_agent(agent).await?;
    info!("BlueZ passkey agent registered (KeyboardDisplay)");

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
