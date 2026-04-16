//! BLE GATT server module.
//!
//! The binary protocol codec ([`protocol`]) is always compiled — it is pure
//! data encoding/decoding with no BlueZ dependencies, enabling CI testing on
//! any platform.
//!
//! All other sub-modules require the `ble` Cargo feature flag (BlueZ D-Bus
//! bindings via `bluer`).

pub mod protocol;

#[cfg(feature = "ble")]
pub mod bridge;
#[cfg(feature = "ble")]
pub mod services;
#[cfg(feature = "ble")]
pub mod session;
#[cfg(feature = "ble")]
pub mod wifi;

#[cfg(feature = "ble")]
use std::sync::Arc;

#[cfg(feature = "ble")]
use thiserror::Error;
#[cfg(feature = "ble")]
use tracing::{error, info};

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
}

/// Start the BLE GATT server.
///
/// Connects to BlueZ via D-Bus, registers all 4 GATT services (pairing,
/// command, WiFi, status), starts LE advertising, and spawns tokio tasks
/// for characteristic I/O.  Runs until the shutdown signal is received.
///
/// On shutdown or client disconnect, clears the BLE session and releases
/// all motor/servo leases held by the BLE connection.
#[cfg(feature = "ble")]
pub async fn start_ble_server(
    config: &BleConfig,
    handler: Arc<Handler>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<(), BleError> {
    use bluer::adv::Advertisement;

    let bt_session: bluer::Session = bluer::Session::new().await?;
    let adapter: bluer::Adapter = bt_session.default_adapter().await?;
    adapter.set_powered(true).await?;

    info!(
        adapter = %adapter.name(),
        device_name = %config.device_name,
        "BLE adapter initialised"
    );

    // Build GATT application with all 4 services.
    let (app, char_handles) = services::build_gatt_application();

    let app_handle = adapter.serve_gatt_application(app).await?;

    // Start LE advertising.
    let le_adv = Advertisement {
        advertisement_type: bluer::adv::Type::Peripheral,
        local_name: Some(config.device_name.clone()),
        service_uuids: services::advertised_service_uuids().into_iter().collect(),
        ..Default::default()
    };
    let adv_handle = adapter.advertise(le_adv).await?;

    info!(device_name = %config.device_name, "BLE GATT server advertising");

    // Shared BLE session state.
    let ble_session = Arc::new(tokio::sync::Mutex::new(session::SessionState::new()));

    // Spawn characteristic I/O handler task.
    let handler_clone = handler.clone();
    let session_clone = ble_session.clone();
    let pairing_secret_path = config.pairing_secret_path.clone();
    let jwt_secret_env = config.jwt_secret_env.clone();

    let io_task = tokio::spawn(async move {
        if let Err(e) = services::run_characteristic_io(
            char_handles,
            handler_clone,
            session_clone,
            &pairing_secret_path,
            &jwt_secret_env,
        )
        .await
        {
            error!(error = %e, "BLE characteristic I/O error");
        }
    });

    // Wait for shutdown signal.
    let _ = shutdown.changed().await;

    // Cleanup: clear session and release all BLE leases.
    {
        let mut sess = ble_session.lock().await;
        sess.clear();
    }
    handler.on_client_disconnect(bridge::BLE_CONN_ID).await;

    // Abort the I/O task (it may be blocked on characteristic events).
    io_task.abort();

    info!("BLE GATT server stopped");

    drop(adv_handle);
    drop(app_handle);

    Ok(())
}
