//! GATT service and characteristic definitions (ADR-004 simplified).
//!
//! Defines a single GATT service with two characteristics:
//! - Command Write (`e3a12001`): client writes NDJSON request chunks
//! - Response Notify (`e3a12002`): server sends NDJSON response chunks
//!
//! Writable characteristics use [`CharacteristicWriteMethod::Fun`] callbacks
//! that forward data through `mpsc` channels.

use bluer::Uuid;
use bluer::gatt::local::{
    Application, Characteristic, CharacteristicControl, CharacteristicNotify,
    CharacteristicNotifyMethod, CharacteristicWrite, CharacteristicWriteMethod, ReqError, Service,
    characteristic_control,
};
use tokio::sync::mpsc;

// ── Service & Characteristic UUIDs ─────────────────────────────────────

/// nomon GATT Service UUID.
pub const NOMON_SERVICE_UUID: Uuid = Uuid::from_u128(0xe3a10001_7b2a_4b9c_8f5a_2b7d6e4f1a3c);

/// Command Write characteristic — client writes NDJSON request chunks.
pub const COMMAND_WRITE_CHAR: Uuid = Uuid::from_u128(0xe3a12001_7b2a_4b9c_8f5a_2b7d6e4f1a3c);

/// Response Notify characteristic — server sends NDJSON response chunks.
pub const RESPONSE_NOTIFY_CHAR: Uuid = Uuid::from_u128(0xe3a12002_7b2a_4b9c_8f5a_2b7d6e4f1a3c);

/// Channel depth for write characteristic data forwarding.
const WRITE_CHANNEL_DEPTH: usize = 32;

/// UUIDs to include in the LE advertising data.
pub fn advertised_service_uuids() -> Vec<Uuid> {
    vec![NOMON_SERVICE_UUID]
}

/// Handles returned from GATT application construction.
pub struct GattHandles {
    /// Receives raw byte chunks from Command Write characteristic.
    pub command_rx: mpsc::Receiver<Vec<u8>>,
    /// Control for Response Notify characteristic.
    pub response_control: CharacteristicControl,
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

/// Build the GATT application with a single nomon service.
pub fn build_gatt_application() -> (Application, GattHandles) {
    let (command_tx, command_rx) = mpsc::channel::<Vec<u8>>(WRITE_CHANNEL_DEPTH);

    let (command_write_control, command_write_handle) = characteristic_control();
    let (response_control, response_handle) = characteristic_control();

    // We don't consume command_write_control events — the Fun callback
    // delivers data via the mpsc channel instead.
    drop(command_write_control);

    let service = Service {
        uuid: NOMON_SERVICE_UUID,
        primary: true,
        characteristics: vec![
            Characteristic {
                uuid: COMMAND_WRITE_CHAR,
                write: Some(CharacteristicWrite {
                    write_without_response: true,
                    // Require OS-level bonding before accepting writes (ADR-004 security model)
                    encrypt_authenticated_write: true,
                    method: write_fun_forwarder(command_tx),
                    ..Default::default()
                }),
                control_handle: command_write_handle,
                ..Default::default()
            },
            Characteristic {
                uuid: RESPONSE_NOTIFY_CHAR,
                notify: Some(CharacteristicNotify {
                    notify: true,
                    method: CharacteristicNotifyMethod::Io,
                    ..Default::default()
                }),
                control_handle: response_handle,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let app = Application {
        services: vec![service],
        ..Default::default()
    };

    let handles = GattHandles {
        command_rx,
        response_control,
    };

    (app, handles)
}
