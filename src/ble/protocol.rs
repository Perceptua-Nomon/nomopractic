//! BLE binary protocol codec.
//!
//! Implements the compact binary frame format for BLE GATT communication
//! as specified in ADR-002.  This module is **not** behind the `ble` feature
//! flag — it contains pure data encoding/decoding with no BlueZ dependencies,
//! allowing the binary codec to be tested in CI on any platform.
//!
//! # Frame Format
//!
//! ```text
//! +--------+--------+--------+------------------------------+
//! | opcode | seq_nr | length | payload                      |
//! | 1 byte | 1 byte | 1 byte | 0–241 bytes                 |
//! +--------+--------+--------+------------------------------+
//! ```
//!
//! All multi-byte integers are little-endian (matches ARM aarch64 and BLE
//! standard practice).

use thiserror::Error;

/// Maximum payload size in bytes (244 max ATT MTU − 3 header bytes).
pub const MAX_PAYLOAD_LEN: usize = 241;

/// Frame header size: opcode + seq_nr + length.
pub const HEADER_LEN: usize = 3;

// ── Request Opcodes ────────────────────────────────────────────────────

/// Request opcode values (`0x01`–`0x7F`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    Heartbeat = 0x01,
    GetBattery = 0x02,
    SetMotorSpeed = 0x03,
    StopAllMotors = 0x04,
    SetServoAngle = 0x05,
    Drive = 0x06,
    Steer = 0x07,
    ReadUltrasonic = 0x08,
    ReadGrayscale = 0x09,
    GetHealth = 0x0A,
}

impl Opcode {
    /// Compute the response opcode (request opcode OR'd with `0x80`).
    pub fn response_opcode(self) -> u8 {
        self as u8 | 0x80
    }

    /// Expected request payload size for this opcode.
    pub fn expected_payload_len(self) -> usize {
        match self {
            Self::Heartbeat
            | Self::GetBattery
            | Self::StopAllMotors
            | Self::ReadUltrasonic
            | Self::ReadGrayscale
            | Self::GetHealth => 0,
            Self::SetMotorSpeed | Self::SetServoAngle => 5,
            Self::Drive | Self::Steer => 4,
        }
    }
}

impl TryFrom<u8> for Opcode {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::Heartbeat),
            0x02 => Ok(Self::GetBattery),
            0x03 => Ok(Self::SetMotorSpeed),
            0x04 => Ok(Self::StopAllMotors),
            0x05 => Ok(Self::SetServoAngle),
            0x06 => Ok(Self::Drive),
            0x07 => Ok(Self::Steer),
            0x08 => Ok(Self::ReadUltrasonic),
            0x09 => Ok(Self::ReadGrayscale),
            0x0A => Ok(Self::GetHealth),
            _ => Err(ProtocolError::UnknownOpcode(value)),
        }
    }
}

// ── Error Codes ────────────────────────────────────────────────────────

/// Binary error codes transmitted in `Error` (`0xFF`) response frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BleErrorCode {
    UnknownCommand = 0x01,
    InvalidParams = 0x02,
    HardwareError = 0x03,
    NotAuthenticated = 0x04,
    NotReady = 0x05,
    InternalError = 0x06,
}

impl BleErrorCode {
    /// Map an IPC error code string to the corresponding binary error code.
    pub fn from_ipc_code(code: &str) -> Self {
        match code {
            "UNKNOWN_METHOD" => Self::UnknownCommand,
            "INVALID_PARAMS" => Self::InvalidParams,
            "HARDWARE_ERROR" | "TIMEOUT" | "NO_ECHO" => Self::HardwareError,
            "NOT_READY" => Self::NotReady,
            _ => Self::InternalError,
        }
    }
}

// ── Protocol Errors ────────────────────────────────────────────────────

/// Errors during binary protocol decode.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("unknown opcode: 0x{0:02x}")]
    UnknownOpcode(u8),
    #[error("frame too short: {actual} bytes, minimum {minimum}")]
    FrameTooShort { actual: usize, minimum: usize },
    #[error("payload length mismatch: header says {header_len}, opcode expects {expected}")]
    PayloadMismatch { header_len: u8, expected: usize },
    #[error("payload too large: {0} bytes, maximum {MAX_PAYLOAD_LEN}")]
    PayloadTooLarge(u8),
}

// ── Request Types ──────────────────────────────────────────────────────

/// Parsed BLE request with typed fields (decoded from a binary frame).
#[derive(Debug, Clone, PartialEq)]
pub enum BleRequest {
    Heartbeat,
    GetBattery,
    SetMotorSpeed {
        channel: u8,
        speed_pct: f64,
        ttl_ms: u16,
    },
    StopAllMotors,
    SetServoAngle {
        channel: u8,
        angle_deg: f64,
        ttl_ms: u16,
    },
    Drive {
        speed_pct: f64,
        ttl_ms: u16,
    },
    Steer {
        angle_deg: f64,
        ttl_ms: u16,
    },
    ReadUltrasonic,
    ReadGrayscale,
    GetHealth,
}

// ── Response Types ─────────────────────────────────────────────────────

/// BLE response variants for encoding into binary frames.
#[derive(Debug, Clone, PartialEq)]
pub enum BleResponse {
    HeartbeatAck {
        uptime_s: u32,
    },
    BatteryResult {
        voltage_mv: u16,
        raw_adc: u16,
    },
    MotorAck {
        channel: u8,
        speed_x100: i16,
    },
    StopAck {
        count: u8,
    },
    ServoAck {
        channel: u8,
        angle_x10: u16,
        pulse_us: u16,
    },
    DriveAck {
        speed_x100: i16,
        motors: u8,
    },
    SteerAck {
        angle_x10: u16,
        channel: u8,
    },
    UltrasonicResult {
        distance_x10: u16,
    },
    GrayscaleResult {
        values: [u16; 3],
    },
    HealthResult {
        status: u8,
        uptime_s: u32,
    },
    Error {
        error_code: BleErrorCode,
        ref_seq: u8,
    },
}

// ── Fixed-Point Helpers ────────────────────────────────────────────────

/// Convert floating-point speed percentage (−100.0–100.0) to ×100 i16.
pub fn speed_to_i16(speed_pct: f64) -> i16 {
    (speed_pct * 100.0).clamp(-10_000.0, 10_000.0) as i16
}

/// Convert ×100 i16 to floating-point speed percentage.
pub fn i16_to_speed(raw: i16) -> f64 {
    f64::from(raw) / 100.0
}

/// Convert floating-point angle (0.0–180.0) to ×10 u16.
pub fn angle_to_u16(angle_deg: f64) -> u16 {
    (angle_deg * 10.0).clamp(0.0, 1800.0) as u16
}

/// Convert ×10 u16 to floating-point angle.
pub fn u16_to_angle(raw: u16) -> f64 {
    f64::from(raw) / 10.0
}

/// Convert floating-point voltage (V) to millivolts (u16).
pub fn voltage_to_mv(voltage_v: f64) -> u16 {
    (voltage_v * 1000.0).clamp(0.0, 65535.0) as u16
}

/// Convert millivolts (u16) to floating-point voltage (V).
pub fn mv_to_voltage(mv: u16) -> f64 {
    f64::from(mv) / 1000.0
}

/// Convert floating-point distance (cm) to ×10 u16.
pub fn distance_to_u16(distance_cm: f64) -> u16 {
    (distance_cm * 10.0).clamp(0.0, 65535.0) as u16
}

/// Convert ×10 u16 to floating-point distance (cm).
pub fn u16_to_distance(raw: u16) -> f64 {
    f64::from(raw) / 10.0
}

// ── Decode ─────────────────────────────────────────────────────────────

/// Decode a binary frame into a typed request and sequence number.
///
/// Returns `(BleRequest, seq_nr)` on success.  The frame must contain at
/// least [`HEADER_LEN`] bytes and the payload must match the opcode's
/// expected size (security checklist B11).
pub fn decode_request(data: &[u8]) -> Result<(BleRequest, u8), ProtocolError> {
    if data.len() < HEADER_LEN {
        return Err(ProtocolError::FrameTooShort {
            actual: data.len(),
            minimum: HEADER_LEN,
        });
    }

    let opcode = Opcode::try_from(data[0])?;
    let seq_nr = data[1];
    let length = data[2];

    if length as usize > MAX_PAYLOAD_LEN {
        return Err(ProtocolError::PayloadTooLarge(length));
    }

    let expected = opcode.expected_payload_len();
    if length as usize != expected {
        return Err(ProtocolError::PayloadMismatch {
            header_len: length,
            expected,
        });
    }

    // Verify the frame actually contains the declared payload bytes.
    if data.len() < HEADER_LEN + expected {
        return Err(ProtocolError::FrameTooShort {
            actual: data.len(),
            minimum: HEADER_LEN + expected,
        });
    }

    let payload = &data[HEADER_LEN..HEADER_LEN + expected];

    let request = match opcode {
        Opcode::Heartbeat => BleRequest::Heartbeat,
        Opcode::GetBattery => BleRequest::GetBattery,
        Opcode::StopAllMotors => BleRequest::StopAllMotors,
        Opcode::ReadUltrasonic => BleRequest::ReadUltrasonic,
        Opcode::ReadGrayscale => BleRequest::ReadGrayscale,
        Opcode::GetHealth => BleRequest::GetHealth,
        Opcode::SetMotorSpeed => {
            let channel = payload[0];
            let speed_x100 = i16::from_le_bytes([payload[1], payload[2]]);
            let ttl_ms = u16::from_le_bytes([payload[3], payload[4]]);
            BleRequest::SetMotorSpeed {
                channel,
                speed_pct: i16_to_speed(speed_x100),
                ttl_ms,
            }
        }
        Opcode::SetServoAngle => {
            let channel = payload[0];
            let angle_x10 = u16::from_le_bytes([payload[1], payload[2]]);
            let ttl_ms = u16::from_le_bytes([payload[3], payload[4]]);
            BleRequest::SetServoAngle {
                channel,
                angle_deg: u16_to_angle(angle_x10),
                ttl_ms,
            }
        }
        Opcode::Drive => {
            let speed_x100 = i16::from_le_bytes([payload[0], payload[1]]);
            let ttl_ms = u16::from_le_bytes([payload[2], payload[3]]);
            BleRequest::Drive {
                speed_pct: i16_to_speed(speed_x100),
                ttl_ms,
            }
        }
        Opcode::Steer => {
            let angle_x10 = u16::from_le_bytes([payload[0], payload[1]]);
            let ttl_ms = u16::from_le_bytes([payload[2], payload[3]]);
            BleRequest::Steer {
                angle_deg: u16_to_angle(angle_x10),
                ttl_ms,
            }
        }
    };

    Ok((request, seq_nr))
}

// ── Encode ─────────────────────────────────────────────────────────────

/// Encode a response into a binary frame (`opcode | seq_nr | length | payload`).
pub fn encode_response(response: &BleResponse, seq_nr: u8) -> Vec<u8> {
    let (opcode, payload) = match response {
        BleResponse::HeartbeatAck { uptime_s } => (0x81u8, uptime_s.to_le_bytes().to_vec()),
        BleResponse::BatteryResult {
            voltage_mv,
            raw_adc,
        } => {
            let mut p = Vec::with_capacity(4);
            p.extend_from_slice(&voltage_mv.to_le_bytes());
            p.extend_from_slice(&raw_adc.to_le_bytes());
            (0x82, p)
        }
        BleResponse::MotorAck {
            channel,
            speed_x100,
        } => {
            let mut p = Vec::with_capacity(3);
            p.push(*channel);
            p.extend_from_slice(&speed_x100.to_le_bytes());
            (0x83, p)
        }
        BleResponse::StopAck { count } => (0x84, vec![*count]),
        BleResponse::ServoAck {
            channel,
            angle_x10,
            pulse_us,
        } => {
            let mut p = Vec::with_capacity(5);
            p.push(*channel);
            p.extend_from_slice(&angle_x10.to_le_bytes());
            p.extend_from_slice(&pulse_us.to_le_bytes());
            (0x85, p)
        }
        BleResponse::DriveAck { speed_x100, motors } => {
            let mut p = Vec::with_capacity(3);
            p.extend_from_slice(&speed_x100.to_le_bytes());
            p.push(*motors);
            (0x86, p)
        }
        BleResponse::SteerAck { angle_x10, channel } => {
            let mut p = Vec::with_capacity(3);
            p.extend_from_slice(&angle_x10.to_le_bytes());
            p.push(*channel);
            (0x87, p)
        }
        BleResponse::UltrasonicResult { distance_x10 } => {
            (0x88, distance_x10.to_le_bytes().to_vec())
        }
        BleResponse::GrayscaleResult { values } => {
            let mut p = Vec::with_capacity(6);
            for v in values {
                p.extend_from_slice(&v.to_le_bytes());
            }
            (0x89, p)
        }
        BleResponse::HealthResult { status, uptime_s } => {
            let mut p = Vec::with_capacity(5);
            p.push(*status);
            p.extend_from_slice(&uptime_s.to_le_bytes());
            (0x8A, p)
        }
        BleResponse::Error {
            error_code,
            ref_seq,
        } => (0xFF, vec![*error_code as u8, *ref_seq]),
    };

    let length = payload.len() as u8;
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.push(opcode);
    frame.push(seq_nr);
    frame.push(length);
    frame.extend_from_slice(&payload);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Round-trip tests ───────────────────────────────────────────────

    #[test]
    fn roundtrip_heartbeat() {
        let frame = [0x01, 0x42, 0x00];
        let (req, seq) = decode_request(&frame).unwrap();
        assert_eq!(req, BleRequest::Heartbeat);
        assert_eq!(seq, 0x42);

        let resp = BleResponse::HeartbeatAck { uptime_s: 12345 };
        let encoded = encode_response(&resp, 0x42);
        assert_eq!(encoded[0], 0x81);
        assert_eq!(encoded[1], 0x42);
        assert_eq!(encoded[2], 4);
        let uptime = u32::from_le_bytes([encoded[3], encoded[4], encoded[5], encoded[6]]);
        assert_eq!(uptime, 12345);
    }

    #[test]
    fn roundtrip_get_battery() {
        let frame = [0x02, 0x01, 0x00];
        let (req, seq) = decode_request(&frame).unwrap();
        assert_eq!(req, BleRequest::GetBattery);
        assert_eq!(seq, 0x01);

        let resp = BleResponse::BatteryResult {
            voltage_mv: 7420,
            raw_adc: 3329,
        };
        let encoded = encode_response(&resp, 0x01);
        assert_eq!(encoded[0], 0x82);
        assert_eq!(encoded[2], 4);
        let mv = u16::from_le_bytes([encoded[3], encoded[4]]);
        let raw = u16::from_le_bytes([encoded[5], encoded[6]]);
        assert_eq!(mv, 7420);
        assert_eq!(raw, 3329);
    }

    #[test]
    fn roundtrip_set_motor_speed() {
        let speed_x100: i16 = 5000; // 50.00%
        let ttl: u16 = 500;
        let mut frame = vec![0x03, 0x10, 5];
        frame.push(0); // channel
        frame.extend_from_slice(&speed_x100.to_le_bytes());
        frame.extend_from_slice(&ttl.to_le_bytes());

        let (req, seq) = decode_request(&frame).unwrap();
        assert_eq!(seq, 0x10);
        match req {
            BleRequest::SetMotorSpeed {
                channel,
                speed_pct,
                ttl_ms,
            } => {
                assert_eq!(channel, 0);
                assert!((speed_pct - 50.0).abs() < 0.01);
                assert_eq!(ttl_ms, 500);
            }
            other => panic!("unexpected: {other:?}"),
        }

        let resp = BleResponse::MotorAck {
            channel: 0,
            speed_x100: 5000,
        };
        let encoded = encode_response(&resp, 0x10);
        assert_eq!(encoded[0], 0x83);
        assert_eq!(encoded[2], 3);
        assert_eq!(encoded[3], 0);
        let s = i16::from_le_bytes([encoded[4], encoded[5]]);
        assert_eq!(s, 5000);
    }

    #[test]
    fn roundtrip_stop_all_motors() {
        let frame = [0x04, 0x05, 0x00];
        let (req, seq) = decode_request(&frame).unwrap();
        assert_eq!(req, BleRequest::StopAllMotors);
        assert_eq!(seq, 0x05);

        let resp = BleResponse::StopAck { count: 2 };
        let encoded = encode_response(&resp, 0x05);
        assert_eq!(encoded[0], 0x84);
        assert_eq!(encoded[2], 1);
        assert_eq!(encoded[3], 2);
    }

    #[test]
    fn roundtrip_set_servo_angle() {
        let angle_x10: u16 = 900; // 90.0°
        let ttl: u16 = 1000;
        let mut frame = vec![0x05, 0x20, 5];
        frame.push(2); // channel
        frame.extend_from_slice(&angle_x10.to_le_bytes());
        frame.extend_from_slice(&ttl.to_le_bytes());

        let (req, _) = decode_request(&frame).unwrap();
        match req {
            BleRequest::SetServoAngle {
                channel,
                angle_deg,
                ttl_ms,
            } => {
                assert_eq!(channel, 2);
                assert!((angle_deg - 90.0).abs() < 0.1);
                assert_eq!(ttl_ms, 1000);
            }
            other => panic!("unexpected: {other:?}"),
        }

        let resp = BleResponse::ServoAck {
            channel: 2,
            angle_x10: 900,
            pulse_us: 1500,
        };
        let encoded = encode_response(&resp, 0x20);
        assert_eq!(encoded[0], 0x85);
        assert_eq!(encoded[2], 5);
    }

    #[test]
    fn roundtrip_drive() {
        let speed_x100: i16 = -3000; // -30.00%
        let ttl: u16 = 250;
        let mut frame = vec![0x06, 0x30, 4];
        frame.extend_from_slice(&speed_x100.to_le_bytes());
        frame.extend_from_slice(&ttl.to_le_bytes());

        let (req, _) = decode_request(&frame).unwrap();
        match req {
            BleRequest::Drive { speed_pct, ttl_ms } => {
                assert!((speed_pct - (-30.0)).abs() < 0.01);
                assert_eq!(ttl_ms, 250);
            }
            other => panic!("unexpected: {other:?}"),
        }

        let resp = BleResponse::DriveAck {
            speed_x100: -3000,
            motors: 2,
        };
        let encoded = encode_response(&resp, 0x30);
        assert_eq!(encoded[0], 0x86);
        assert_eq!(encoded[2], 3);
    }

    #[test]
    fn roundtrip_steer() {
        let angle_x10: u16 = 450; // 45.0°
        let ttl: u16 = 500;
        let mut frame = vec![0x07, 0x40, 4];
        frame.extend_from_slice(&angle_x10.to_le_bytes());
        frame.extend_from_slice(&ttl.to_le_bytes());

        let (req, _) = decode_request(&frame).unwrap();
        match req {
            BleRequest::Steer { angle_deg, ttl_ms } => {
                assert!((angle_deg - 45.0).abs() < 0.1);
                assert_eq!(ttl_ms, 500);
            }
            other => panic!("unexpected: {other:?}"),
        }

        let resp = BleResponse::SteerAck {
            angle_x10: 450,
            channel: 2,
        };
        let encoded = encode_response(&resp, 0x40);
        assert_eq!(encoded[0], 0x87);
        assert_eq!(encoded[2], 3);
    }

    #[test]
    fn roundtrip_read_ultrasonic() {
        let frame = [0x08, 0x50, 0x00];
        let (req, _) = decode_request(&frame).unwrap();
        assert_eq!(req, BleRequest::ReadUltrasonic);

        let resp = BleResponse::UltrasonicResult { distance_x10: 250 }; // 25.0 cm
        let encoded = encode_response(&resp, 0x50);
        assert_eq!(encoded[0], 0x88);
        assert_eq!(encoded[2], 2);
        let d = u16::from_le_bytes([encoded[3], encoded[4]]);
        assert_eq!(d, 250);
    }

    #[test]
    fn roundtrip_read_grayscale() {
        let frame = [0x09, 0x60, 0x00];
        let (req, _) = decode_request(&frame).unwrap();
        assert_eq!(req, BleRequest::ReadGrayscale);

        let resp = BleResponse::GrayscaleResult {
            values: [100, 200, 300],
        };
        let encoded = encode_response(&resp, 0x60);
        assert_eq!(encoded[0], 0x89);
        assert_eq!(encoded[2], 6);
        let v0 = u16::from_le_bytes([encoded[3], encoded[4]]);
        let v1 = u16::from_le_bytes([encoded[5], encoded[6]]);
        let v2 = u16::from_le_bytes([encoded[7], encoded[8]]);
        assert_eq!([v0, v1, v2], [100, 200, 300]);
    }

    #[test]
    fn roundtrip_get_health() {
        let frame = [0x0A, 0x70, 0x00];
        let (req, _) = decode_request(&frame).unwrap();
        assert_eq!(req, BleRequest::GetHealth);

        let resp = BleResponse::HealthResult {
            status: 1,
            uptime_s: 99999,
        };
        let encoded = encode_response(&resp, 0x70);
        assert_eq!(encoded[0], 0x8A);
        assert_eq!(encoded[2], 5);
        assert_eq!(encoded[3], 1);
        let up = u32::from_le_bytes([encoded[4], encoded[5], encoded[6], encoded[7]]);
        assert_eq!(up, 99999);
    }

    #[test]
    fn encode_error_response() {
        let resp = BleResponse::Error {
            error_code: BleErrorCode::NotAuthenticated,
            ref_seq: 0xAB,
        };
        let encoded = encode_response(&resp, 0x00);
        assert_eq!(encoded[0], 0xFF);
        assert_eq!(encoded[1], 0x00);
        assert_eq!(encoded[2], 2);
        assert_eq!(encoded[3], 0x04); // NotAuthenticated
        assert_eq!(encoded[4], 0xAB);
    }

    // ── Error cases ────────────────────────────────────────────────────

    #[test]
    fn decode_empty_frame() {
        let result = decode_request(&[]);
        assert!(matches!(
            result,
            Err(ProtocolError::FrameTooShort { actual: 0, .. })
        ));
    }

    #[test]
    fn decode_too_short_header() {
        let result = decode_request(&[0x01, 0x00]);
        assert!(matches!(result, Err(ProtocolError::FrameTooShort { .. })));
    }

    #[test]
    fn decode_unknown_opcode() {
        let frame = [0x7F, 0x00, 0x00];
        let result = decode_request(&frame);
        assert!(matches!(result, Err(ProtocolError::UnknownOpcode(0x7F))));
    }

    #[test]
    fn decode_payload_length_mismatch() {
        // SetMotorSpeed expects 5 bytes payload, header says 3.
        let frame = [0x03, 0x00, 3, 0, 0, 0];
        let result = decode_request(&frame);
        assert!(matches!(result, Err(ProtocolError::PayloadMismatch { .. })));
    }

    #[test]
    fn decode_truncated_payload() {
        // Header says 5 bytes payload, but frame has only the 3-byte header.
        let frame = [0x03, 0x00, 5];
        let result = decode_request(&frame);
        assert!(matches!(result, Err(ProtocolError::FrameTooShort { .. })));
    }

    #[test]
    fn decode_payload_too_large() {
        let frame = [0x01, 0x00, 0xFF]; // 255 > MAX_PAYLOAD_LEN
        let result = decode_request(&frame);
        assert!(matches!(result, Err(ProtocolError::PayloadTooLarge(0xFF))));
    }

    // ── Fixed-point helpers ────────────────────────────────────────────

    #[test]
    fn speed_conversion_roundtrip() {
        assert_eq!(i16_to_speed(speed_to_i16(50.0)), 50.0);
        assert_eq!(i16_to_speed(speed_to_i16(-100.0)), -100.0);
        assert_eq!(i16_to_speed(speed_to_i16(0.0)), 0.0);
    }

    #[test]
    fn angle_conversion_roundtrip() {
        assert!((u16_to_angle(angle_to_u16(90.0)) - 90.0).abs() < 0.1);
        assert!((u16_to_angle(angle_to_u16(180.0)) - 180.0).abs() < 0.1);
        assert!((u16_to_angle(angle_to_u16(0.0))).abs() < 0.1);
    }

    #[test]
    fn speed_clamp_extremes() {
        assert_eq!(speed_to_i16(200.0), 10000); // clamped to 100.00
        assert_eq!(speed_to_i16(-200.0), -10000); // clamped to -100.00
    }

    #[test]
    fn voltage_conversion() {
        assert_eq!(voltage_to_mv(7.42), 7420);
        assert!((mv_to_voltage(7420) - 7.42).abs() < f64::EPSILON);
    }

    #[test]
    fn distance_conversion_roundtrip() {
        assert!((u16_to_distance(distance_to_u16(25.0)) - 25.0).abs() < 0.1);
    }

    // ── Opcode helpers ─────────────────────────────────────────────────

    #[test]
    fn response_opcode_has_high_bit() {
        assert_eq!(Opcode::Heartbeat.response_opcode(), 0x81);
        assert_eq!(Opcode::GetHealth.response_opcode(), 0x8A);
    }

    #[test]
    fn ble_error_code_from_ipc() {
        assert_eq!(
            BleErrorCode::from_ipc_code("UNKNOWN_METHOD"),
            BleErrorCode::UnknownCommand
        );
        assert_eq!(
            BleErrorCode::from_ipc_code("INVALID_PARAMS"),
            BleErrorCode::InvalidParams
        );
        assert_eq!(
            BleErrorCode::from_ipc_code("HARDWARE_ERROR"),
            BleErrorCode::HardwareError
        );
        assert_eq!(
            BleErrorCode::from_ipc_code("NOT_READY"),
            BleErrorCode::NotReady
        );
        assert_eq!(
            BleErrorCode::from_ipc_code("zz_unknown"),
            BleErrorCode::InternalError
        );
    }

    // ── Edge cases ─────────────────────────────────────────────────────

    #[test]
    fn set_motor_speed_negative_full() {
        let speed_x100: i16 = -10000; // -100.00%
        let ttl: u16 = 100;
        let mut frame = vec![0x03, 0x01, 5];
        frame.push(1); // channel
        frame.extend_from_slice(&speed_x100.to_le_bytes());
        frame.extend_from_slice(&ttl.to_le_bytes());

        let (req, _) = decode_request(&frame).unwrap();
        match req {
            BleRequest::SetMotorSpeed {
                channel,
                speed_pct,
                ttl_ms,
            } => {
                assert_eq!(channel, 1);
                assert!((speed_pct - (-100.0)).abs() < 0.01);
                assert_eq!(ttl_ms, 100);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn decode_with_extra_trailing_bytes() {
        // Heartbeat with extra bytes — still decodes the valid frame.
        let frame = [0x01, 0x42, 0x00, 0xFF, 0xFF];
        let (req, seq) = decode_request(&frame).unwrap();
        assert_eq!(req, BleRequest::Heartbeat);
        assert_eq!(seq, 0x42);
    }

    #[test]
    fn servo_angle_max() {
        let angle_x10: u16 = 1800; // 180.0°
        let ttl: u16 = 500;
        let mut frame = vec![0x05, 0x01, 5];
        frame.push(11); // max servo channel
        frame.extend_from_slice(&angle_x10.to_le_bytes());
        frame.extend_from_slice(&ttl.to_le_bytes());

        let (req, _) = decode_request(&frame).unwrap();
        match req {
            BleRequest::SetServoAngle {
                channel, angle_deg, ..
            } => {
                assert_eq!(channel, 11);
                assert!((angle_deg - 180.0).abs() < 0.1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn zero_seq_nr() {
        let frame = [0x01, 0x00, 0x00];
        let (_, seq) = decode_request(&frame).unwrap();
        assert_eq!(seq, 0);
    }

    #[test]
    fn max_seq_nr() {
        let frame = [0x01, 0xFF, 0x00];
        let (_, seq) = decode_request(&frame).unwrap();
        assert_eq!(seq, 255);
    }

    #[test]
    fn error_response_opcode_is_0xff() {
        let resp = BleResponse::Error {
            error_code: BleErrorCode::UnknownCommand,
            ref_seq: 0x10,
        };
        let encoded = encode_response(&resp, 0x00);
        assert_eq!(encoded[0], 0xFF);
    }

    #[test]
    fn zero_ttl_motor() {
        let mut frame = vec![0x03, 0x01, 5];
        frame.push(0);
        frame.extend_from_slice(&0i16.to_le_bytes()); // 0 speed
        frame.extend_from_slice(&0u16.to_le_bytes()); // 0 ttl

        let (req, _) = decode_request(&frame).unwrap();
        match req {
            BleRequest::SetMotorSpeed {
                speed_pct, ttl_ms, ..
            } => {
                assert!((speed_pct).abs() < 0.001);
                assert_eq!(ttl_ms, 0);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn max_ttl_servo() {
        let mut frame = vec![0x05, 0x01, 5];
        frame.push(0);
        frame.extend_from_slice(&0u16.to_le_bytes()); // angle 0
        frame.extend_from_slice(&u16::MAX.to_le_bytes()); // max ttl

        let (req, _) = decode_request(&frame).unwrap();
        match req {
            BleRequest::SetServoAngle { ttl_ms, .. } => {
                assert_eq!(ttl_ms, u16::MAX);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
