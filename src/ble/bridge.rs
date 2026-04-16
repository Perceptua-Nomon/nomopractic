//! BLE command bridge — maps BLE binary requests to IPC handler dispatch.
//!
//! Converts each [`BleRequest`] variant to a JSON IPC request, dispatches
//! through the existing [`Handler`], and parses the JSON response back into
//! a [`BleResponse`].  This guarantees BLE and Unix socket IPC commands
//! follow identical validation and hardware paths.

use serde_json::json;

use super::protocol::{
    BleErrorCode, BleRequest, BleResponse, angle_to_u16, distance_to_u16, speed_to_i16,
    voltage_to_mv,
};
use crate::ipc::handler::Handler;

/// BLE connection ID for motor/servo lease tracking.
///
/// Distinct from the routine engine's `ROUTINE_CONN_ID = 0`.
pub const BLE_CONN_ID: u64 = 1;

/// Dispatch a BLE request through the IPC handler and return a BLE response.
///
/// The request is serialized to JSON, dispatched via [`Handler::dispatch`],
/// and the JSON response is parsed back into the appropriate [`BleResponse`]
/// variant.  Errors are mapped to [`BleResponse::Error`] with the correct
/// [`BleErrorCode`].
pub async fn dispatch(request: &BleRequest, seq_nr: u8, handler: &Handler) -> BleResponse {
    let (method, params) = request_to_ipc(request);

    let ipc_json = serde_json::to_string(&json!({
        "id": format!("ble-{seq_nr}"),
        "method": method,
        "params": params,
    }))
    .unwrap_or_default();

    let response_str = handler.dispatch(&ipc_json, BLE_CONN_ID).await;

    let response: serde_json::Value = match serde_json::from_str(&response_str) {
        Ok(v) => v,
        Err(_) => {
            return BleResponse::Error {
                error_code: BleErrorCode::InternalError,
                ref_seq: seq_nr,
            };
        }
    };

    if response.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        let result = &response["result"];
        response_to_ble(request, result, seq_nr)
    } else {
        let code = response
            .pointer("/error/code")
            .and_then(|v| v.as_str())
            .unwrap_or("INTERNAL_ERROR");
        BleResponse::Error {
            error_code: BleErrorCode::from_ipc_code(code),
            ref_seq: seq_nr,
        }
    }
}

/// Map a BLE request to an IPC method name and JSON params.
fn request_to_ipc(request: &BleRequest) -> (&'static str, serde_json::Value) {
    match request {
        BleRequest::Heartbeat | BleRequest::GetHealth => ("health", json!({})),
        BleRequest::GetBattery => ("get_battery_voltage", json!({})),
        BleRequest::SetMotorSpeed {
            channel,
            speed_pct,
            ttl_ms,
        } => (
            "set_motor_speed",
            json!({
                "channel": channel,
                "speed_pct": speed_pct,
                "ttl_ms": ttl_ms,
            }),
        ),
        BleRequest::StopAllMotors => ("stop_all_motors", json!({})),
        BleRequest::SetServoAngle {
            channel,
            angle_deg,
            ttl_ms,
        } => (
            "set_servo_angle",
            json!({
                "channel": channel,
                "angle_deg": angle_deg,
                "ttl_ms": ttl_ms,
            }),
        ),
        BleRequest::Drive { speed_pct, ttl_ms } => (
            "drive",
            json!({
                "speed_pct": speed_pct,
                "ttl_ms": ttl_ms,
            }),
        ),
        BleRequest::Steer { angle_deg, ttl_ms } => (
            "steer",
            json!({
                "angle_deg": angle_deg,
                "ttl_ms": ttl_ms,
            }),
        ),
        BleRequest::ReadUltrasonic => ("read_ultrasonic", json!({})),
        BleRequest::ReadGrayscale => ("read_grayscale", json!({})),
    }
}

/// Map a successful IPC JSON result to a typed BLE response.
fn response_to_ble(request: &BleRequest, result: &serde_json::Value, _seq_nr: u8) -> BleResponse {
    match request {
        BleRequest::Heartbeat | BleRequest::GetHealth => {
            let uptime_s = result.get("uptime_s").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let status = if result.get("status").and_then(|v| v.as_str()) == Some("ok") {
                1u8
            } else {
                0u8
            };
            match request {
                BleRequest::Heartbeat => BleResponse::HeartbeatAck { uptime_s },
                _ => BleResponse::HealthResult { status, uptime_s },
            }
        }
        BleRequest::GetBattery => {
            let voltage_v = result
                .get("voltage_v")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let raw_adc = result.get("raw_adc").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            BleResponse::BatteryResult {
                voltage_mv: voltage_to_mv(voltage_v),
                raw_adc,
            }
        }
        BleRequest::SetMotorSpeed {
            channel, speed_pct, ..
        } => BleResponse::MotorAck {
            channel: *channel,
            speed_x100: speed_to_i16(*speed_pct),
        },
        BleRequest::StopAllMotors => {
            let count = result.get("stopped").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
            BleResponse::StopAck { count }
        }
        BleRequest::SetServoAngle {
            channel, angle_deg, ..
        } => {
            let pulse_us = result.get("pulse_us").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            BleResponse::ServoAck {
                channel: *channel,
                angle_x10: angle_to_u16(*angle_deg),
                pulse_us,
            }
        }
        BleRequest::Drive { speed_pct, .. } => {
            let motors = result.get("motors").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
            BleResponse::DriveAck {
                speed_x100: speed_to_i16(*speed_pct),
                motors,
            }
        }
        BleRequest::Steer { angle_deg, .. } => {
            let channel = result.get("channel").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
            BleResponse::SteerAck {
                angle_x10: angle_to_u16(*angle_deg),
                channel,
            }
        }
        BleRequest::ReadUltrasonic => {
            let distance_cm = result
                .get("distance_cm")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            BleResponse::UltrasonicResult {
                distance_x10: distance_to_u16(distance_cm),
            }
        }
        BleRequest::ReadGrayscale => {
            let values_arr = result.get("values").and_then(|v| v.as_array());
            let mut values = [0u16; 3];
            if let Some(arr) = values_arr {
                for (i, v) in arr.iter().take(3).enumerate() {
                    values[i] = v.as_u64().unwrap_or(0) as u16;
                }
            }
            BleResponse::GrayscaleResult { values }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::config::Config;
    use crate::hat::gpio::HatGpio;
    use crate::hat::i2c::Hat;
    use crate::testing::{MockGpio, MockI2c};

    fn test_handler() -> Handler {
        let hat = Arc::new(Hat::new(MockI2c { response: [0, 0] }, 0x14));
        let gpio = Arc::new(HatGpio::new(MockGpio::new()));
        Handler::new(Arc::new(Config::default()), hat, gpio)
    }

    #[tokio::test]
    async fn dispatch_heartbeat() {
        let handler = test_handler();
        let resp = dispatch(&BleRequest::Heartbeat, 0x01, &handler).await;
        match resp {
            BleResponse::HeartbeatAck { uptime_s } => {
                // Uptime should be 0 or very small since handler was just created.
                assert!(uptime_s < 10);
            }
            other => panic!("expected HeartbeatAck, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_get_health() {
        let handler = test_handler();
        let resp = dispatch(&BleRequest::GetHealth, 0x02, &handler).await;
        match resp {
            BleResponse::HealthResult { status, .. } => {
                assert_eq!(status, 1); // "ok" → 1
            }
            other => panic!("expected HealthResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_stop_all_motors() {
        let handler = test_handler();
        let resp = dispatch(&BleRequest::StopAllMotors, 0x03, &handler).await;
        match resp {
            BleResponse::StopAck { count } => {
                assert_eq!(count, 2); // default config has 2 motors
            }
            other => panic!("expected StopAck, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_drive_returns_ack() {
        let handler = test_handler();
        let resp = dispatch(
            &BleRequest::Drive {
                speed_pct: 50.0,
                ttl_ms: 500,
            },
            0x04,
            &handler,
        )
        .await;
        match resp {
            BleResponse::DriveAck { speed_x100, motors } => {
                assert_eq!(speed_x100, 5000);
                assert_eq!(motors, 2);
            }
            other => panic!("expected DriveAck, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_invalid_motor_returns_error() {
        let handler = test_handler();
        let resp = dispatch(
            &BleRequest::SetMotorSpeed {
                channel: 99, // invalid
                speed_pct: 50.0,
                ttl_ms: 500,
            },
            0x05,
            &handler,
        )
        .await;
        match resp {
            BleResponse::Error {
                error_code,
                ref_seq,
            } => {
                assert_eq!(error_code, BleErrorCode::InvalidParams);
                assert_eq!(ref_seq, 0x05);
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn request_to_ipc_maps_all_variants() {
        // Verify all request variants produce a valid method name.
        let variants: Vec<BleRequest> = vec![
            BleRequest::Heartbeat,
            BleRequest::GetBattery,
            BleRequest::SetMotorSpeed {
                channel: 0,
                speed_pct: 50.0,
                ttl_ms: 500,
            },
            BleRequest::StopAllMotors,
            BleRequest::SetServoAngle {
                channel: 0,
                angle_deg: 90.0,
                ttl_ms: 500,
            },
            BleRequest::Drive {
                speed_pct: 30.0,
                ttl_ms: 250,
            },
            BleRequest::Steer {
                angle_deg: 45.0,
                ttl_ms: 500,
            },
            BleRequest::ReadUltrasonic,
            BleRequest::ReadGrayscale,
            BleRequest::GetHealth,
        ];
        for variant in &variants {
            let (method, params) = request_to_ipc(variant);
            assert!(!method.is_empty(), "empty method for {variant:?}");
            assert!(params.is_object(), "params not object for {variant:?}");
        }
    }
}
