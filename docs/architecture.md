# nomopractic — Architecture

## System Context

```
┌──────────────────────────────────────────────────────────────────────┐
│                    Raspberry Pi Zero 2W (aarch64)                    │
│                                                                      │
│  ┌──────────────────────────────────┐                                │
│  │     nomothetic (Python)          │                                │
│  │  ┌─────────┐  ┌──────────────┐  │                                │
│  │  │   API   │  │  Telemetry   │  │                                │
│  │  │ :8443   │  │  (MQTT)      │  │                                │
│  │  └────┬────┘  └──────────────┘  │                                │
│  │       │                          │                                │
│  │  ┌────┴────┐  ┌──────────────┐  │                                │
│  │  │  Camera │  │  Streaming   │  │                                │
│  │  │         │  │  :8000       │  │                                │
│  │  └─────────┘  └──────────────┘  │                                │
│  │       │                          │                                │
│  │  ┌────┴────┐                     │                                │
│  │  │HatClient│ ← IPC client (no HW logic)                         │
│  │  └────┬────┘                     │                                │
│  └───────┼──────────────────────────┘                                │
│          │ Unix socket (NDJSON)                                      │
│          │ /run/nomopractic/nomopractic.sock                         │
│  ┌───────┴──────────────────────────┐                                │
│  │   nomopractic (Rust daemon)      │  ← THIS REPO                  │
│  │                                   │                                │
│  │  ┌─────────┐  ┌──────────────┐   │                                │
│  │  │   IPC   │  │   Config     │   │                                │
│  │  │ listener│  │  (TOML+env)  │   │                                │
│  │  └────┬────┘  └──────────────┘   │                                │
│  │       │                           │                                │
│  │  ┌────┴────────────────────────┐  │                                │
│  │  │     HAT Driver Layer        │  │                                │
│  │  │  ┌─────┐ ┌─────┐ ┌──────┐  │  │                                │
│  │  │  │Servo│ │ ADC │ │ GPIO │  │  │                                │
│  │  │  └──┬──┘ └──┬──┘ └──┬───┘  │  │                                │
│  │  │     │       │       │       │  │                                │
│  │  │  ┌──┴───────┴───────┴────┐  │  │                                │
│  │  │  │   PWM    │    I2C     │  │  │                                │
│  │  │  └──────────┴────────────┘  │  │                                │
│  │  └─────────────────────────────┘  │                                │
│  └───────────────────────────────────┘                                │
│          │                                                            │
│  ════════╪════════════════════════════════════════  Hardware bus      │
│          │                                                            │
│  ┌───────┴──────────────────────────┐                                │
│  │   SunFounder Robot HAT V4       │                                │
│  │   I2C bus 1, address 0x14       │                                │
│  │                                   │                                │
│  │   PWM channels 0–11 (servos)     │                                │
│  │   ADC A4 (battery voltage)       │                                │
│  │   GPIO: D4, D5, MCURST, SW, LED │                                │
│  └───────────────────────────────────┘                                │
└──────────────────────────────────────────────────────────────────────┘
```

## Module Dependency Graph

```
main.rs
  ├── config.rs          (CLI + TOML + env)
  ├── ipc/
  │   ├── mod.rs          (Unix socket listener)
  │   ├── schema.rs       (NDJSON request/response types)
  │   ├── handler.rs      (method dispatch → HAT drivers)
  │   └── params.rs       (typed IPC parameter extraction helpers)
  ├── hat/
  │   ├── mod.rs          (HAT abstraction, shared state)
  │   ├── i2c.rs          (rppal I2C read/write)
  │   ├── pwm.rs          (prescaler + channel register writes)
  │   ├── adc.rs          (ADC command + read)
  │   ├── servo.rs        (angle ↔ pulse, TTL lease watchdog)
  │   ├── battery.rs      (ADC A4 → voltage)
  │   ├── motor.rs        (H-bridge dir GPIO + PWM duty; TTL lease watchdog)
  │   ├── gpio.rs         (named pin abstraction; D2/D3/MCURST/SpeakerEn…)
  │   └── ultrasonic.rs   (HC-SR04 TRIG/ECHO GPIO timing)
  ├── calibration.rs      (CalibrationStore: motor/servo/grayscale calibration)
  ├── routine/
  │   ├── mod.rs          (RoutineEngine, RoutineState, RoutineStats)
  │   └── explore.rs      (explore_task: ultrasonic + normalised grayscale sensor-actuator loop)
  ├── ble/                (BLE GATT server — behind `ble` Cargo feature flag)
  │   ├── mod.rs          (GATT server lifecycle, BlueZ passkey agent, advertising)
  │   ├── services.rs     (single GATT service + 2 characteristics: Command Write, Response Notify)
  │   └── bridge.rs       (NDJSON relay: accumulate chunks → Handler::dispatch() → chunk response)
  ├── wifi.rs             (WiFi control: nmcli scan/connect/status — WifiControl trait)
  ├── reset.rs            (MCU reset via BCM5)
  └── testing.rs          (shared test mocks: MockI2c, MockGpio, MockAlsaControl — #[cfg(test)] only)
```

### Dependency Rules

1. `ipc/handler.rs` depends on `hat/` modules — it is the only bridge.
2. `hat/` sub-modules depend only on `hat/i2c.rs` for bus access.
3. `config.rs` has no internal dependencies.
4. `reset.rs` uses `rppal::gpio` directly (BCM5 is not on HAT I2C).
5. No module depends on `main.rs` — all logic is in `lib.rs`.
6. `ble/` depends on `ipc/handler.rs` for command dispatch (same handler serves
   both Unix socket IPC and BLE GATT commands).
7. `ble/` modules depend only on `bluer`; no `hat/` imports, no crypto crates.
8. `wifi.rs` has no internal dependencies (uses `std::process::Command` for nmcli).

## Data Flow

### Servo Command

```
HatClient (Python)
  → JSON: {"id":"1","method":"set_servo_angle","params":{"channel":0,"angle_deg":90.0,"ttl_ms":500}}
  → Unix socket write + \n
  → ipc/mod.rs (read line from stream)
  → ipc/schema.rs (deserialize Request)
  → ipc/handler.rs (match method → call hat::servo::set_angle)
  → hat/servo.rs (convert angle → pulse_us, register TTL lease)
  → hat/pwm.rs (calculate prescaler, write channel)
  → hat/i2c.rs (rppal I2C write to 0x14)
  → Response: {"id":"1","ok":true,"result":{"channel":0,"angle_deg":90.0,"pulse_us":1500}}
  → Unix socket write + \n
  → HatClient reads, returns to Python
```

### Battery Read

```
HatClient (Python)
  → {"id":"2","method":"get_battery_voltage","params":{}}
  → ipc → handler → hat::battery::read_voltage()
  → hat::adc::read_channel(A4)
  → hat::i2c::write + read (0x14)
  → raw_adc: (raw / 4095) × 3.3 × 3.0 = voltage_v
  → {"id":"2","ok":true,"result":{"voltage_v":7.42}}
```

### Servo TTL Watchdog

```
set_servo_angle(ch=0, ttl_ms=500)
  → hat/servo.rs registers lease: (channel=0, expires=now+500ms)
  → watchdog task (polls every watchdog_poll_ms):
      - checks all active leases
      - if lease.expires < now:
          → hat/pwm.rs write pulse_us=0 (idle channel)
          → remove lease
          → log warning: "servo lease expired, channel 0 idled"
```

## IPC Protocol

See `nomothetic/docs/hat_ipc_schema.md` for the full specification.

**Transport**: Unix domain socket, `SOCK_STREAM`
**Framing**: NDJSON (one JSON object per line, `\n` terminated)
**Max message**: 4096 bytes
**Encoding**: UTF-8

### Methods Summary

| Method | Params | Result |
|--------|--------|--------|
| `health` | — | `schema_version`, `status`, `version`, `hat_address`, `i2c_bus`, `uptime_s` |
| `get_battery_voltage` | — | `voltage_v`, `raw_adc` |
| `set_servo_pulse_us` | `channel`, `pulse_us`, `ttl_ms` | `channel`, `pulse_us` |
| `set_servo_angle` | `channel`, `angle_deg`, `ttl_ms` | `channel`, `angle_deg`, `pulse_us` |
| `get_servo_status` | — | `active_leases: [{channel, ttl_remaining_ms, conn_id}]` |
| `reset_mcu` | — | `reset_ms` |
| `get_mcu_status` | — | `resets_since_start`, `last_reset_s_ago` |
| `read_adc` | `channel` (0–7) | `channel`, `raw_value` |
| `set_motor_speed` | `channel` (0–3), `speed_pct`, `ttl_ms` | `channel`, `speed_pct` |
| `stop_all_motors` | — | `stopped` (count) |
| `get_motor_status` | — | `active_leases: [{channel, ttl_remaining_ms, conn_id}]` |
| `drive` | `speed_pct`, `ttl_ms` | `speed_pct`, `motors` |
| `steer` | `angle_deg`, `ttl_ms` | `servo`, `channel`, `angle_deg`, `pulse_us` |
| `pan_camera` | `angle_deg`, `ttl_ms` | `servo`, `channel`, `angle_deg`, `pulse_us` |
| `tilt_camera` | `angle_deg`, `ttl_ms` | `servo`, `channel`, `angle_deg`, `pulse_us` |
| `read_grayscale` | — | `channels: [u8; 3]`, `values: [u16; 3]` |
| `read_ultrasonic` | — | `distance_cm` |
| `enable_speaker` | — | `enabled: true`, `pin_bcm` |
| `disable_speaker` | — | `enabled: false`, `pin_bcm` |
| `set_volume` | `volume_pct` (0–100) | `volume_pct` |
| `get_volume` | — | `volume_pct` |
| `set_mic_gain` | `gain_pct` (0–100) | `gain_pct` |
| `get_mic_gain` | — | `gain_pct` |
| `get_calibration` | — | `motors: [...]`, `servos: {...}`, `grayscale: [...]` |
| `set_motor_calibration` | `channel`, `speed_scale?`, `deadband_pct?`, `reversed?` | `channel`, `speed_scale`, `deadband_pct`, `reversed` |
| `set_servo_calibration` | `servo`, `trim_us` | `servo`, `trim_us` |
| `calibrate_grayscale` | `channel` (0–2), `surface` | `channel`, `adc_channel`, `surface`, `raw_value`, `stored` |
| `read_grayscale_normalized` | — | `channels: [u8; 3]`, `normalized: [f64; 3]` |
| `save_calibration` | — | `saved`, `path` |
| `reset_calibration` | — | `reset` |
| `start_routine` | `name`, `speed_pct?`, `obstacle_threshold_cm?`, `cliff_threshold_normalized?`, `max_duration_s?` | `name`, `started_at_uptime_s` |
| `stop_routine` | — | `name`, `ran_for_s`, `obstacles_avoided`, `cliffs_avoided`, `stop_reason` |
| `get_routine_status` | — | `running`, `name?`, `elapsed_s?`, `obstacles_avoided?`, `cliffs_avoided?` |
| `wifi_scan` | — | `networks: [{ ssid, signal, security }]` |
| `wifi_connect` | `ssid`, `password` | `success` |
| `wifi_status` | — | `state`, `ssid?`, `signal?` |
| `authenticate` | — | `token`, `expires_at` (transport-agnostic) |
| `read_gpio` | `pin` | `pin`, `high` |
| `write_gpio` | `pin`, `high` | `pin`, `high` |

### BLE NDJSON Relay (Phase 13.1)

BLE commands use the **same NDJSON framing** as the Unix socket IPC path
(ADR-004). OS-level Bluetooth passkey pairing provides authentication and
link-layer encryption. A single GATT service with 2 characteristics replaces
the original 4-service binary protocol layout.

**Transport:** BLE GATT characteristics (write + notify)
**Framing:** NDJSON — identical to Unix socket IPC
**Security:** OS-level Bluetooth passkey pairing + link-layer AES-CCM
**Max message:** No fixed limit (NDJSON chunked at MTU boundary)

The BLE bridge (`ble/bridge.rs`) accumulates NDJSON chunks from the Command
Write characteristic, dispatches complete JSON lines through the same
`Handler::dispatch()` used by Unix socket IPC, and sends the NDJSON response
back via Response Notify (chunked if needed).

| GATT Characteristic | UUID | Direction |
|---------------------|------|-----------|
| Command Write | `e3a12001-…` | Client → Server (NDJSON request) |
| Response Notify | `e3a12002-…` | Server → Client (NDJSON response) |

All 39 IPC methods are available over BLE — no per-method codec required.

### Error Codes

| Code | Meaning |
|------|---------|
| `UNKNOWN_METHOD` | Method name not recognized |
| `INVALID_PARAMS` | Missing or out-of-range parameters |
| `HARDWARE_ERROR` | I2C/SPI/GPIO bus failure |
| `NOT_READY` | Daemon still initializing |
| `SERVO_LEASE_EXPIRED` | TTL elapsed, servo was idled |
| `ALREADY_RUNNING` | A routine is already active; stop it before starting a new one |
| `INTERNAL_ERROR` | Unexpected daemon bug |

## Concurrency Model

- **Tokio async runtime** with multi-threaded scheduler.
- One spawned task per client connection (read lines → dispatch → respond).
- HAT I2C bus access serialized via `tokio::sync::Mutex` (one bus transaction at
  a time — required by I2C protocol).
- Servo TTL watchdog runs as a background `tokio::spawn` task, polling on
  `watchdog_poll_ms` interval.
- Client disconnect triggers cleanup: release servo leases for that client.

## Configuration

Priority order (highest wins):

1. **CLI arguments** (`--config <path>`)
2. **Environment variables** (`NOMON_HAT_*`)
3. **Config file** (TOML)
4. **Compiled defaults**

See `config.toml` for all options.

## Security

- Socket created with mode `0660`, group `nomon`.
- Daemon runs as root (required for I2C/GPIO), socket restricted to `nomon` group.
- No network listeners — Unix socket only (kernel-enforced access control).
- BLE GATT server (Phase 13.1): OS-level Bluetooth passkey pairing via BlueZ
  agent. See ADR-004 for the simplified security model.
  - Passkey Entry: 6-digit numeric code read from `pairing_secret_path`.
  - Link-layer AES-CCM encryption provided by OS bonding (replaces app-layer crypto).
  - JWT issued via `authenticate` IPC method after bonding; valid for HTTPS
    (shared `NOMON_JWT_SECRET` with nomothetic).
  - NDJSON commands over BLE — same format and validation as Unix socket IPC.
- Servo TTL lease prevents stall on client crash.
- Input validation on all IPC parameters (channel range, pulse range, etc.).

## Deployment

- **Binary**: Cross-compiled for `aarch64-unknown-linux-gnu`, deployed to
  `/usr/local/bin/nomopractic`.
- **Config**: `/etc/nomopractic/config.toml`.
- **Service**: `systemd/nomopractic.service` → `systemctl enable nomopractic`.
- **Socket dir**: `/run/nomopractic/` created by `ExecStartPre` in systemd unit.
- **Updates**: Binary download + SHA-256 verify + atomic swap + `systemctl restart`.
