//! BLE NDJSON relay bridge (ADR-004).
//!
//! Accumulates incoming BLE writes into a buffer until a complete NDJSON
//! line (terminated by `\n`) is received.  The line is dispatched through
//! the IPC [`Handler`], and the response is chunked at the MTU boundary
//! and sent as notifications on the Response characteristic.

use std::sync::Arc;

use bluer::gatt::CharacteristicWriter;
use bluer::gatt::local::CharacteristicControlEvent;
use tokio_stream::StreamExt;
use tracing::{debug, warn};

use super::services::GattHandles;
use crate::ipc::handler::Handler;

/// BLE connection ID for motor/servo lease tracking.
///
/// Distinct from the routine engine's `ROUTINE_CONN_ID = 0`.
pub const BLE_CONN_ID: u64 = 1;

// Assumes negotiated MTU ≥ 244 (ATT overhead = 3 bytes); client requests MTU 247.
/// Default ATT payload size: negotiated MTU (244) minus ATT header (3).
const DEFAULT_CHUNK_SIZE: usize = 241;

/// Run the NDJSON relay loop.
///
/// Reads from the Command Write characteristic, accumulates bytes until
/// a complete `\n`-terminated JSON line is received, dispatches through
/// the handler, and sends the response as chunked notifications.
pub async fn run_json_relay(
    handles: GattHandles,
    handler: Arc<Handler>,
) -> Result<(), super::BleError> {
    let GattHandles {
        mut command_rx,
        response_control,
    } = handles;

    tokio::pin!(response_control);

    let mut response_writer: Option<CharacteristicWriter> = None;
    let mut buffer = Vec::new();

    loop {
        tokio::select! {
            // Track Response Notify subscriptions.
            Some(event) = response_control.next() => {
                if let CharacteristicControlEvent::Notify(writer) = event {
                    debug!("BLE response notify: client subscribed");
                    response_writer = Some(writer);
                }
            }
            Some(data) = command_rx.recv() => {
                buffer.extend_from_slice(&data);

                // Process all complete lines in the buffer.
                while let Some(newline_pos) = buffer.iter().position(|&b| b == b'\n') {
                    let line_bytes = buffer.drain(..=newline_pos).collect::<Vec<u8>>();
                    let line = match std::str::from_utf8(&line_bytes) {
                        Ok(s) => s.trim().to_string(),
                        Err(_) => {
                            warn!("BLE command: invalid UTF-8, skipping");
                            continue;
                        }
                    };

                    if line.is_empty() {
                        continue;
                    }

                    debug!("BLE command received: {}", &line[..line.len().min(80)]);

                    // Dispatch through the IPC handler.
                    let response_str = handler.dispatch(&line, BLE_CONN_ID).await;

                    // Send response as NDJSON (with trailing newline).
                    let response_bytes = format!("{response_str}\n");
                    let chunks = chunk_bytes(response_bytes.as_bytes(), DEFAULT_CHUNK_SIZE);

                    if let Some(ref writer) = response_writer {
                        for chunk in chunks {
                            if let Err(e) = writer.send(&chunk).await {
                                warn!(error = %e, "failed to send BLE response chunk");
                                break;
                            }
                        }
                    } else {
                        // NOTE: clients must subscribe to Response Notify before writing commands; responses are silently dropped when no subscriber is registered.
                        warn!("no BLE response subscriber — response dropped");
                    }
                }

                // Safety: prevent unbounded buffer growth from malformed input.
                if buffer.len() > 8192 {
                    warn!("BLE command buffer overflow (>8KB without newline), clearing");
                    buffer.clear();
                }
            }
            else => break,
        }
    }

    Ok(())
}

/// Split a byte slice into chunks of at most `max_size` bytes.
fn chunk_bytes(data: &[u8], max_size: usize) -> Vec<Vec<u8>> {
    data.chunks(max_size).map(|c| c.to_vec()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_bytes_single() {
        let data = b"hello\n";
        let chunks = chunk_bytes(data, 241);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], b"hello\n");
    }

    #[test]
    fn chunk_bytes_multi() {
        let data = vec![0x41u8; 500]; // 500 bytes of 'A'
        let chunks = chunk_bytes(&data, 241);
        assert_eq!(chunks.len(), 3); // 241 + 241 + 18
        assert_eq!(chunks[0].len(), 241);
        assert_eq!(chunks[1].len(), 241);
        assert_eq!(chunks[2].len(), 18);
    }

    #[test]
    fn chunk_bytes_exact_boundary() {
        let data = vec![0x42u8; 241];
        let chunks = chunk_bytes(&data, 241);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 241);
    }

    #[test]
    fn chunk_bytes_empty() {
        let chunks = chunk_bytes(b"", 241);
        assert!(chunks.is_empty());
    }
}
