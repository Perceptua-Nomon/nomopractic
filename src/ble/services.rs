//! GATT service and characteristic definitions.
//!
//! Defines the 4 GATT services (pairing, command, WiFi, status) with their
//! characteristics and permissions, and builds a `bluer::gatt::local::Application`
//! ready for registration with the BlueZ stack.
//!
//! Writable characteristics (pairing secret, command write, WiFi command) use
//! [`CharacteristicWriteMethod::Fun`] callbacks that forward data through
//! `mpsc` channels, avoiding the socket-based IO approach that is designed
//! for large streaming payloads rather than our small binary frames.

use std::path::Path;
use std::sync::Arc;

use bluer::Uuid;
use bluer::gatt::CharacteristicWriter;
use bluer::gatt::local::{
    Application, Characteristic, CharacteristicControl, CharacteristicControlEvent,
    CharacteristicNotify, CharacteristicNotifyMethod, CharacteristicRead, CharacteristicWrite,
    CharacteristicWriteMethod, ReqError, Service, characteristic_control,
};
use tokio::sync::{Mutex, mpsc};
use tokio_stream::StreamExt;
use tracing::{debug, info, warn};

use super::bridge;
use super::protocol;
use super::session::SessionState;
use super::wifi;
use super::wifi::WifiControl;
use crate::ipc::handler::Handler;

// ── Service UUIDs ──────────────────────────────────────────────────────

/// nomon Pairing Service.
pub const PAIRING_SERVICE_UUID: Uuid = Uuid::from_u128(0xe3a10001_7b2a_4b9c_8f5a_2b7d6e4f1a3c);
/// nomon Command Service.
pub const COMMAND_SERVICE_UUID: Uuid = Uuid::from_u128(0xe3a10002_7b2a_4b9c_8f5a_2b7d6e4f1a3c);
/// nomon WiFi Provisioning Service.
pub const WIFI_SERVICE_UUID: Uuid = Uuid::from_u128(0xe3a10003_7b2a_4b9c_8f5a_2b7d6e4f1a3c);
/// nomon Status Service.
pub const STATUS_SERVICE_UUID: Uuid = Uuid::from_u128(0xe3a10004_7b2a_4b9c_8f5a_2b7d6e4f1a3c);

// ── Characteristic UUIDs ───────────────────────────────────────────────

// Pairing Service characteristics.
const PAIRING_SECRET_CHAR: Uuid = Uuid::from_u128(0xe3a11001_7b2a_4b9c_8f5a_2b7d6e4f1a3c);
const AUTH_TOKEN_CHAR: Uuid = Uuid::from_u128(0xe3a11002_7b2a_4b9c_8f5a_2b7d6e4f1a3c);
const SESSION_STATE_CHAR: Uuid = Uuid::from_u128(0xe3a11003_7b2a_4b9c_8f5a_2b7d6e4f1a3c);

// Command Service characteristics.
const COMMAND_WRITE_CHAR: Uuid = Uuid::from_u128(0xe3a12001_7b2a_4b9c_8f5a_2b7d6e4f1a3c);
const COMMAND_RESPONSE_CHAR: Uuid = Uuid::from_u128(0xe3a12002_7b2a_4b9c_8f5a_2b7d6e4f1a3c);

// WiFi Service characteristics.
const WIFI_COMMAND_CHAR: Uuid = Uuid::from_u128(0xe3a13001_7b2a_4b9c_8f5a_2b7d6e4f1a3c);
const WIFI_RESULT_CHAR: Uuid = Uuid::from_u128(0xe3a13002_7b2a_4b9c_8f5a_2b7d6e4f1a3c);

// Status Service characteristics.
const DEVICE_STATE_CHAR: Uuid = Uuid::from_u128(0xe3a14001_7b2a_4b9c_8f5a_2b7d6e4f1a3c);
const BATTERY_LEVEL_CHAR: Uuid = Uuid::from_u128(0xe3a14004_7b2a_4b9c_8f5a_2b7d6e4f1a3c);

/// Channel depth for write characteristic data forwarding.
const WRITE_CHANNEL_DEPTH: usize = 16;

/// UUIDs to include in the LE advertising data.
pub fn advertised_service_uuids() -> Vec<Uuid> {
    vec![PAIRING_SERVICE_UUID]
}

/// Handles returned from GATT application construction.
///
/// Write channels receive raw bytes forwarded from GATT write callbacks.
/// Notify controls are `CharacteristicControl` streams for sending
/// notifications to the connected client.
pub struct GattHandles {
    /// Receives pairing secret write data.
    pub pairing_rx: mpsc::Receiver<Vec<u8>>,
    /// Receives binary command frame write data.
    pub command_rx: mpsc::Receiver<Vec<u8>>,
    /// Receives WiFi command write data.
    pub wifi_rx: mpsc::Receiver<Vec<u8>>,
    /// Control for auth token notifications.
    pub auth_token_control: CharacteristicControl,
    /// Control for command response notifications.
    pub command_response_control: CharacteristicControl,
    /// Control for session state notifications.
    pub session_state_control: CharacteristicControl,
    /// Control for WiFi result notifications.
    pub wifi_result_control: CharacteristicControl,
    /// Control for device state notifications.
    pub device_state_control: CharacteristicControl,
    /// Control for battery level notifications.
    pub battery_level_control: CharacteristicControl,
}

/// Create a `CharacteristicWriteMethod::Fun` that forwards written data to `tx`.
fn write_fun_forwarder(tx: mpsc::Sender<Vec<u8>>) -> CharacteristicWriteMethod {
    CharacteristicWriteMethod::Fun(Box::new(move |data, _req| {
        let tx = tx.clone();
        Box::pin(async move {
            tx.send(data).await.map_err(|_| ReqError::Failed)?;
            Ok(())
        })
    }))
}

/// Build the GATT application with all 4 services and return it along
/// with write-data receivers and notify control handles.
pub fn build_gatt_application() -> (Application, GattHandles) {
    // Write data channels (Fun callbacks → mpsc → processing loop).
    let (pairing_tx, pairing_rx) = mpsc::channel::<Vec<u8>>(WRITE_CHANNEL_DEPTH);
    let (command_tx, command_rx) = mpsc::channel::<Vec<u8>>(WRITE_CHANNEL_DEPTH);
    let (wifi_tx, wifi_rx) = mpsc::channel::<Vec<u8>>(WRITE_CHANNEL_DEPTH);

    // ── Pairing Service ────────────────────────────────────────────────

    let (pairing_write_control, pairing_write_handle) = characteristic_control();
    let (auth_token_control, auth_token_handle) = characteristic_control();
    let (session_state_control, session_state_handle) = characteristic_control();

    // We don't consume pairing_write_control events — the Fun callback
    // delivers data via the mpsc channel instead.
    drop(pairing_write_control);

    let pairing_service = Service {
        uuid: PAIRING_SERVICE_UUID,
        primary: true,
        characteristics: vec![
            Characteristic {
                uuid: PAIRING_SECRET_CHAR,
                write: Some(CharacteristicWrite {
                    write: true,
                    method: write_fun_forwarder(pairing_tx),
                    ..Default::default()
                }),
                control_handle: pairing_write_handle,
                ..Default::default()
            },
            Characteristic {
                uuid: AUTH_TOKEN_CHAR,
                read: Some(CharacteristicRead {
                    read: true,
                    ..Default::default()
                }),
                notify: Some(CharacteristicNotify {
                    notify: true,
                    method: CharacteristicNotifyMethod::Io,
                    ..Default::default()
                }),
                control_handle: auth_token_handle,
                ..Default::default()
            },
            Characteristic {
                uuid: SESSION_STATE_CHAR,
                read: Some(CharacteristicRead {
                    read: true,
                    ..Default::default()
                }),
                notify: Some(CharacteristicNotify {
                    notify: true,
                    method: CharacteristicNotifyMethod::Io,
                    ..Default::default()
                }),
                control_handle: session_state_handle,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    // ── Command Service ────────────────────────────────────────────────

    let (command_write_control, command_write_handle) = characteristic_control();
    let (command_response_control, command_response_handle) = characteristic_control();

    drop(command_write_control);

    let command_service = Service {
        uuid: COMMAND_SERVICE_UUID,
        primary: true,
        characteristics: vec![
            Characteristic {
                uuid: COMMAND_WRITE_CHAR,
                write: Some(CharacteristicWrite {
                    write: true,
                    method: write_fun_forwarder(command_tx),
                    ..Default::default()
                }),
                control_handle: command_write_handle,
                ..Default::default()
            },
            Characteristic {
                uuid: COMMAND_RESPONSE_CHAR,
                notify: Some(CharacteristicNotify {
                    notify: true,
                    method: CharacteristicNotifyMethod::Io,
                    ..Default::default()
                }),
                control_handle: command_response_handle,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    // ── WiFi Provisioning Service ──────────────────────────────────────

    let (wifi_write_control, wifi_write_handle) = characteristic_control();
    let (wifi_result_control, wifi_result_handle) = characteristic_control();

    drop(wifi_write_control);

    let wifi_service = Service {
        uuid: WIFI_SERVICE_UUID,
        primary: true,
        characteristics: vec![
            Characteristic {
                uuid: WIFI_COMMAND_CHAR,
                write: Some(CharacteristicWrite {
                    write: true,
                    method: write_fun_forwarder(wifi_tx),
                    ..Default::default()
                }),
                control_handle: wifi_write_handle,
                ..Default::default()
            },
            Characteristic {
                uuid: WIFI_RESULT_CHAR,
                notify: Some(CharacteristicNotify {
                    notify: true,
                    method: CharacteristicNotifyMethod::Io,
                    ..Default::default()
                }),
                control_handle: wifi_result_handle,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    // ── Status Service ─────────────────────────────────────────────────

    let (device_state_control, device_state_handle) = characteristic_control();
    let (battery_level_control, battery_level_handle) = characteristic_control();

    let status_service = Service {
        uuid: STATUS_SERVICE_UUID,
        primary: true,
        characteristics: vec![
            Characteristic {
                uuid: DEVICE_STATE_CHAR,
                read: Some(CharacteristicRead {
                    read: true,
                    ..Default::default()
                }),
                notify: Some(CharacteristicNotify {
                    notify: true,
                    method: CharacteristicNotifyMethod::Io,
                    ..Default::default()
                }),
                control_handle: device_state_handle,
                ..Default::default()
            },
            Characteristic {
                uuid: BATTERY_LEVEL_CHAR,
                read: Some(CharacteristicRead {
                    read: true,
                    ..Default::default()
                }),
                notify: Some(CharacteristicNotify {
                    notify: true,
                    method: CharacteristicNotifyMethod::Io,
                    ..Default::default()
                }),
                control_handle: battery_level_handle,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let app = Application {
        services: vec![
            pairing_service,
            command_service,
            wifi_service,
            status_service,
        ],
        ..Default::default()
    };

    let handles = GattHandles {
        pairing_rx,
        command_rx,
        wifi_rx,
        auth_token_control,
        command_response_control,
        session_state_control,
        wifi_result_control,
        device_state_control,
        battery_level_control,
    };

    (app, handles)
}

/// Main characteristic I/O loop — handles writes to pairing, command, and
/// WiFi characteristics, dispatches to the handler/session, and sends
/// notifications for responses.
///
/// Write data arrives via `mpsc` channels populated by GATT `Fun` callbacks.
/// Notification writers are obtained from `CharacteristicControl` streams
/// when a remote client subscribes.
pub async fn run_characteristic_io(
    handles: GattHandles,
    handler: Arc<Handler>,
    session: Arc<Mutex<SessionState>>,
    pairing_secret_path: &Path,
    jwt_secret_env: &str,
) -> Result<(), super::BleError> {
    let GattHandles {
        mut pairing_rx,
        mut command_rx,
        mut wifi_rx,
        auth_token_control,
        command_response_control,
        session_state_control: _,
        wifi_result_control,
        device_state_control: _,
        battery_level_control: _,
    } = handles;

    tokio::pin!(auth_token_control);
    tokio::pin!(command_response_control);
    tokio::pin!(wifi_result_control);

    // Active notification writers — set when the remote client subscribes.
    let mut auth_token_writer: Option<CharacteristicWriter> = None;
    let mut command_response_writer: Option<CharacteristicWriter> = None;
    let mut wifi_result_writer: Option<CharacteristicWriter> = None;

    loop {
        tokio::select! {
            // Track auth token notification subscriptions.
            Some(event) = auth_token_control.next() => {
                if let CharacteristicControlEvent::Notify(writer) = event {
                    debug!("auth token characteristic: client subscribed");
                    auth_token_writer = Some(writer);
                }
            }
            // Track command response notification subscriptions.
            Some(event) = command_response_control.next() => {
                if let CharacteristicControlEvent::Notify(writer) = event {
                    debug!("command response characteristic: client subscribed");
                    command_response_writer = Some(writer);
                }
            }
            // Track WiFi result notification subscriptions.
            Some(event) = wifi_result_control.next() => {
                if let CharacteristicControlEvent::Notify(writer) = event {
                    debug!("WiFi result characteristic: client subscribed");
                    wifi_result_writer = Some(writer);
                }
            }
            Some(secret_bytes) = pairing_rx.recv() => {
                let secret = String::from_utf8_lossy(&secret_bytes);
                debug!("BLE pairing secret write received");

                let stored = match std::fs::read_to_string(pairing_secret_path) {
                    Ok(s) => s.trim().to_string(),
                    Err(e) => {
                        warn!(error = %e, "failed to read pairing secret file");
                        continue;
                    }
                };

                let jwt_secret = match std::env::var(jwt_secret_env) {
                    Ok(s) => s,
                    Err(_) => {
                        warn!("JWT secret env var not set");
                        continue;
                    }
                };

                let mut sess = session.lock().await;
                match super::session::pair(&secret, &stored, &jwt_secret) {
                    Ok((ble_session, auth_payload)) => {
                        sess.set_session(ble_session);
                        info!("BLE pairing successful");

                        // Send auth payload (salt + JWT) to client (ADR-003 step 4).
                        if let Some(ref writer) = auth_token_writer {
                            if let Err(e) = writer.send(&auth_payload).await {
                                warn!(error = %e, "failed to send auth token notification");
                            }
                        } else {
                            warn!("no auth token subscriber — client will not receive salt");
                        }
                        drop(sess);

                        // Consume pairing secret file (security checklist B2).
                        if let Err(e) = std::fs::remove_file(pairing_secret_path) {
                            warn!(error = %e, "failed to delete pairing secret file");
                        } else {
                            info!("pairing secret consumed via BLE");
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "BLE pairing failed");
                    }
                }
            }
            Some(data) = command_rx.recv() => {
                if data.len() < protocol::HEADER_LEN {
                    warn!("BLE command frame too short");
                    continue;
                }

                // Header is authenticated but not encrypted (AAD).
                let header = &data[..protocol::HEADER_LEN];
                let encrypted = &data[protocol::HEADER_LEN..];

                // Decrypt the encrypted payload.
                let decrypted = {
                    let mut sess = session.lock().await;
                    let ble_session = match sess.session_mut() {
                        Some(s) => s,
                        None => {
                            debug!("BLE command rejected: not paired");
                            continue;
                        }
                    };
                    match super::session::decrypt(ble_session, encrypted, header) {
                        Ok(plaintext) => plaintext,
                        Err(e) => {
                            warn!(error = %e, "BLE command decryption failed");
                            continue;
                        }
                    }
                };

                // Reconstruct plaintext frame: header + decrypted payload.
                let mut frame = Vec::with_capacity(header.len() + decrypted.len());
                frame.extend_from_slice(header);
                frame.extend_from_slice(&decrypted);

                match protocol::decode_request(&frame) {
                    Ok((ble_req, seq_nr)) => {
                        let response = bridge::dispatch(
                            &ble_req, seq_nr, &handler,
                        ).await;
                        let encoded = protocol::encode_response(&response, seq_nr);
                        debug!(seq_nr, "BLE command dispatched");

                        // Encrypt and send the response.
                        let resp_header = &encoded[..protocol::HEADER_LEN];
                        let resp_payload = &encoded[protocol::HEADER_LEN..];

                        let mut sess = session.lock().await;
                        if let Some(ble_session) = sess.session_mut() {
                            match super::session::encrypt(
                                ble_session, resp_payload, resp_header,
                            ) {
                                Ok(encrypted_resp) => {
                                    let mut resp_frame = Vec::with_capacity(
                                        resp_header.len() + encrypted_resp.len(),
                                    );
                                    resp_frame.extend_from_slice(resp_header);
                                    resp_frame.extend_from_slice(&encrypted_resp);

                                    if let Some(ref writer) = command_response_writer
                                        && let Err(e) = writer.send(&resp_frame).await
                                    {
                                        warn!(error = %e, "failed to send command response");
                                    }
                                }
                                Err(e) => {
                                    warn!(error = %e, "BLE response encryption failed");
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "BLE command decode error");
                    }
                }
            }
            Some(data) = wifi_rx.recv() => {
                // WiFi commands require an authenticated session.
                {
                    let sess = session.lock().await;
                    if !sess.is_paired() {
                        debug!("BLE WiFi command rejected: not paired");
                        continue;
                    }
                }

                let wifi_ctrl = wifi::NmcliWifi;
                let result = match wifi::decode_wifi_command(&data) {
                    Some(wifi::WifiCommand::Scan) => {
                        match wifi_ctrl.scan() {
                            Ok(networks) => {
                                info!(count = networks.len(), "WiFi scan complete");
                                wifi::WifiResult::ScanResult(networks)
                            }
                            Err(e) => {
                                warn!(error = %e, "WiFi scan failed");
                                continue;
                            }
                        }
                    }
                    Some(wifi::WifiCommand::Connect { ssid, password }) => {
                        info!(ssid = %ssid, "WiFi connect requested");
                        match wifi_ctrl.connect(&ssid, &password) {
                            Ok(()) => wifi::WifiResult::ConnectResult { success: true },
                            Err(e) => {
                                warn!(error = %e, ssid = %ssid, "WiFi connect failed");
                                wifi::WifiResult::ConnectResult { success: false }
                            }
                        }
                    }
                    Some(wifi::WifiCommand::Status) => {
                        match wifi_ctrl.status() {
                            Ok(status) => wifi::WifiResult::StatusResult(status),
                            Err(e) => {
                                warn!(error = %e, "WiFi status query failed");
                                continue;
                            }
                        }
                    }
                    None => {
                        warn!("invalid WiFi command byte");
                        continue;
                    }
                };

                let encoded = wifi::encode_wifi_result(&result);
                if let Some(ref writer) = wifi_result_writer
                    && let Err(e) = writer.send(&encoded).await
                {
                    warn!(error = %e, "failed to send WiFi result notification");
                }
            }
            else => break,
        }
    }

    Ok(())
}
