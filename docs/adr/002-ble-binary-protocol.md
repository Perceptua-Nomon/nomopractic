# ADR-002: Binary Protocol for BLE GATT

> ⚠️ **SUPERSEDED — Do not implement.**
> This ADR describes a binary frame protocol (opcodes, fixed-point encoding) that was fully replaced by [ADR-004: BLE Simplification — Native OS Pairing + NDJSON Relay](004-ble-simplification.md).
> The binary codec, all opcode tables, and fixed-point helpers no longer exist in either `nomopractic` or `nomotactic`.
> Retain this document for historical reference only.

## Status

Superseded by [ADR-004](004-ble-simplification.md)

## Date

2026-04-15

## Context

BLE GATT characteristic writes are limited by the negotiated MTU (typically
20–244 bytes per write). The existing IPC protocol between nomothetic and
nomopractic uses NDJSON — a text-based format with significant overhead.

A minimal NDJSON motor command:
```json
{"id":"1","method":"set_motor_speed","params":{"channel":0,"speed_pct":50.0,"ttl_ms":500}}
```
is **87 bytes** — nearly half the maximum MTU and exceeding the minimum.

Motor control commands are sent at 10+ Hz during active driving. BLE bandwidth
is precious (BCM43436s shares the antenna with WiFi). A compact encoding is
needed.

## Decision

Use a **compact binary protocol** for BLE GATT characteristic writes and
notifications. The existing NDJSON protocol remains unchanged for Unix socket
IPC and HTTPS communication.

### Frame Format

```
+--------+--------+--------+------------------------------+
| opcode | seq_nr | length | payload                      |
| 1 byte | 1 byte | 1 byte | 0–241 bytes                 |
+--------+--------+--------+------------------------------+
```

- **opcode** (u8): Command identifier. Request opcodes: `0x01`–`0x7F`.
  Response opcodes: request opcode OR'd with `0x80`.
- **seq_nr** (u8): Sequence number for request/response correlation.
  Client increments per request (wraps at 255). Server echoes in response.
- **length** (u8): Payload byte count (0–241). Max total frame: 244 bytes
  (matches BLE 4.2 max ATT MTU).
- **payload**: Opcode-specific binary fields.

### Endianness

Little-endian throughout. Matches ARM (aarch64) native byte order and BLE
standard practice.

### Fixed-Point Encoding

Floating-point values are transmitted as scaled integers to avoid IEEE 754
overhead:

| Value | Encoding | Type | Resolution | Range |
|-------|----------|------|------------|-------|
| speed_pct | × 100 | i16 LE | 0.01% | −100.00–100.00 |
| angle_deg | × 10 | u16 LE | 0.1° | 0.0–180.0 |
| voltage | × 1000 (mV) | u16 LE | 1 mV | 0–65.535 V |
| distance_cm | × 10 | u16 LE | 1 mm | 0–6553.5 cm |
| ttl_ms | raw | u16 LE | 1 ms | 0–65535 ms |

### Opcode Table — Requests

| Opcode | Name | Payload | Size |
|--------|------|---------|------|
| `0x01` | Heartbeat | (none) | 0 |
| `0x02` | GetBattery | (none) | 0 |
| `0x03` | SetMotorSpeed | `channel: u8, speed_x100: i16, ttl_ms: u16` | 5 |
| `0x04` | StopAllMotors | (none) | 0 |
| `0x05` | SetServoAngle | `channel: u8, angle_x10: u16, ttl_ms: u16` | 5 |
| `0x06` | Drive | `speed_x100: i16, ttl_ms: u16` | 4 |
| `0x07` | Steer | `angle_x10: u16, ttl_ms: u16` | 4 |
| `0x08` | ReadUltrasonic | (none) | 0 |
| `0x09` | ReadGrayscale | (none) | 0 |
| `0x0A` | GetHealth | (none) | 0 |

### Opcode Table — Responses

| Opcode | Name | Payload | Size |
|--------|------|---------|------|
| `0x81` | HeartbeatAck | `uptime_s: u32` | 4 |
| `0x82` | BatteryResult | `voltage_mv: u16, raw_adc: u16` | 4 |
| `0x83` | MotorAck | `channel: u8, speed_x100: i16` | 3 |
| `0x84` | StopAck | `count: u8` | 1 |
| `0x85` | ServoAck | `channel: u8, angle_x10: u16, pulse_us: u16` | 5 |
| `0x86` | DriveAck | `speed_x100: i16, motors: u8` | 3 |
| `0x87` | SteerAck | `angle_x10: u16, channel: u8` | 3 |
| `0x88` | UltrasonicResult | `distance_x10: u16` | 2 |
| `0x89` | GrayscaleResult | `v0: u16, v1: u16, v2: u16` | 6 |
| `0x8A` | HealthResult | `status: u8, uptime_s: u32` | 5 |
| `0xFF` | Error | `error_code: u8, ref_seq: u8` | 2 |

### Error Codes (Binary)

| Code | Name | Maps to IPC |
|------|------|-------------|
| `0x01` | UnknownCommand | `UNKNOWN_METHOD` |
| `0x02` | InvalidParams | `INVALID_PARAMS` |
| `0x03` | HardwareError | `HARDWARE_ERROR` |
| `0x04` | NotAuthenticated | (BLE-specific) |
| `0x05` | NotReady | `NOT_READY` |
| `0x06` | InternalError | `INTERNAL_ERROR` |

### Authenticated Frame Format

After BLE pairing establishes a session key (see ADR-003), command frames
are wrapped in AES-128-CCM encryption:

```
+--------+--------+--------+-----------+---------------------+-----+
| opcode | seq_nr | length | ctr       | encrypted_payload   | tag |
| 1 byte | 1 byte | 1 byte | 2 bytes   | (length - 6) bytes | 4 B |
+--------+--------+--------+-----------+---------------------+-----+
```

- **ctr** (u16 LE): Monotonic counter per direction (client→server and
  server→client maintain separate counters). Used as nonce component.
- **tag** (4 bytes): Truncated AES-128-CCM authentication tag.
- AES-CCM nonce (13 bytes): `"NM" || direction_u8 || ctr_u16_LE || 0x00 × 8`
  - `direction_u8`: `0x00` = client→server, `0x01` = server→client.
- Associated data (AAD): `opcode || seq_nr || length` (the 3 header bytes).
  Header is authenticated but not encrypted (readable for routing).

Pre-pairing messages (pairing secret write, status reads) are sent
unencrypted on their dedicated GATT characteristics.

## Alternatives Considered

### 1. JSON over BLE

- Minimum motor command: 87 bytes. Exceeds 20-byte minimum MTU.
- Parsing overhead: JSON deserialization on every 100ms motor tick.
- **Rejected** — too verbose for BLE bandwidth constraints.

### 2. CBOR (RFC 8949)

- Compact binary, self-describing, widely supported.
- Motor command in CBOR: ~30 bytes (map keys add overhead).
- Adds `ciborium` or `minicbor` crate dependency.
- Self-describing format is unnecessary when both sides know the schema.
- **Rejected** — still larger than fixed-format, adds decoding complexity.

### 3. MessagePack

- Similar trade-offs to CBOR. ~25 bytes for a motor command.
- **Rejected** — same reasoning as CBOR.

### 4. Protocol Buffers

- Requires `.proto` schema files and code generation.
- Varint encoding adds variable-length complexity.
- Heavy toolchain dependency for simple fixed-format messages.
- **Rejected** — over-engineered for 10 fixed-format opcodes.

### 5. Custom binary (chosen)

- Motor command: 8 bytes total (3 header + 5 payload). 10× smaller than JSON.
- Fixed-format: compile-time struct layout, zero allocation decode.
- No external codec dependency.
- **Accepted.**

## Consequences

- A `ble/protocol.rs` module implements encode/decode for the binary frame
  format. All types are `#[repr(C)]` or manual byte-level serialization.
- nomotactic must implement the same binary codec in TypeScript for
  `react-native-ble-plx` characteristic writes.
- The binary protocol is documented in this ADR and referenced from the
  nomopractic and nomotactic architecture docs.
- The NDJSON IPC protocol is unaffected — it continues to serve Unix socket
  and HTTPS communication paths.
- Protocol versioning is handled at the opcode table level (adding new opcodes)
  rather than embedding a version field in individual responses.

## References

- BLE 4.2 Core Spec, Vol 3, Part F (ATT MTU)
- nomopractic IPC schema: `nomothetic/docs/hat_ipc_schema.md`
- BCM43436s shared WiFi/BLE antenna constraints
