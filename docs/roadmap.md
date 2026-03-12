# nomopractic — Development Roadmap

## Overview

nomopractic is the Rust HAT hardware daemon for the nomon fleet. Development is
organized into phases, each building on the previous. This aligns with Phase 5
milestones in the nomothetic roadmap.

---

## Phase 1 — Foundation & IPC Scaffold

**Goal**: Minimal daemon that listens on a Unix socket, parses NDJSON, and
responds to `health` requests. No hardware access yet.

### 1.1 — Project Bootstrap
- [x] Cargo project initialized with dependencies
- [x] Source module tree scaffolded
- [x] systemd unit file
- [x] Example config (TOML)
- [x] Copilot instructions
- [x] Architecture & roadmap docs

### 1.2 — Configuration
- [x] `config.rs`: Load TOML config file with serde
- [x] Environment variable overrides (`NOMON_HAT_*`)
- [x] CLI argument parsing (clap): `--config <path>`
- [x] Validation: reject invalid log_level, zero TTL/watchdog
- [x] Unit tests: 9 tests (defaults, file load, env overrides, validation errors)

### 1.3 — Tracing & Logging
- [x] `tracing-subscriber` init with `env-filter`
- [x] Log level from config / `RUST_LOG` env var
- [x] Structured fields in log output

### 1.4 — IPC Listener
- [x] `ipc/mod.rs`: Tokio `UnixListener` on configured socket path
- [x] Socket permissions (mode `0660`)
- [x] Per-client task spawning with graceful shutdown (ctrl-c)
- [x] Read NDJSON lines (max 4096 bytes), parse `Request`
- [x] Write `Response` + newline back to client
- [x] Client disconnect cleanup (log)
- [x] Integration tests: 5 tests (health, unknown method, malformed JSON,
      multiple requests, concurrent clients)

### 1.5 — Health Method
- [x] `ipc/handler.rs`: Route `health` method with uptime tracking
- [x] Response: `schema_version`, `status`, `version`, `uptime_s`, `hat_address`, `i2c_bus`
- [x] Error response for unknown methods (`UNKNOWN_METHOD`)
- [x] Error response for malformed JSON (`INVALID_PARAMS`)
- [x] Unit tests: 5 tests (health, unknown method, malformed, missing field, default params)

### Phase 1 Exit Criteria
- `cargo test` — all tests pass (no hardware required)
- `cargo clippy -- -D warnings` — zero warnings
- `cargo fmt --check` — clean
- Daemon starts, accepts socket connections, responds to `health`
- Can be verified with `socat`:
  ```
  echo '{"id":"1","method":"health","params":{}}' | \
    socat - UNIX-CONNECT:/run/nomopractic/nomopractic.sock
  ```

---

## Phase 2 — I2C & Battery Voltage (P0)

**Goal**: Read battery voltage from the Robot HAT V4 via I2C. First real
hardware interaction.

### 2.1 — I2C Helpers
- [x] `hat/i2c.rs`: Open I2C bus via rppal
- [x] `read_register(addr, reg, buf)` helper
- [x] `write_register(addr, reg, data)` helper
- [x] Shared `Hat` struct holding `rppal::i2c::I2c` behind `tokio::sync::Mutex`
- [x] Unit tests with mock I2C (trait-based abstraction)

### 2.2 — ADC Read
- [x] `hat/adc.rs`: Write command byte, read 2-byte result
- [x] Channel validation (A0–A7)
- [x] Error handling for I2C failures

### 2.3 — Battery Voltage
- [x] `hat/battery.rs`: Read ADC channel A4
- [x] Scaling: `voltage_v = (raw / 4095) × 3.3 × 3.0` (12-bit ADC, 3.3 V ref, 3:1 voltage divider)
- [x] `get_battery_voltage` IPC method wired up in handler
- [x] Unit test: mock ADC → verify voltage calculation

### Phase 2 Exit Criteria
- [x] Battery voltage readable via IPC
- [x] I2C errors returned as `HARDWARE_ERROR` to client
- [x] All tests pass without hardware (mocked I2C)

---

## Phase 3 — PWM & Servo Control (P0)

**Goal**: Drive servos on PWM channels 0–11. Includes TTL lease safety watchdog.

### 3.1 — PWM Control
- [x] `hat/pwm.rs`: Prescaler calculation from `CLOCK_HZ` / `SERVO_FREQ`
- [x] PWM initialization: set prescaler + auto-reload registers
- [x] Channel pulse write: `REG_CHN + channel * 4`
- [x] Frequency validation (50 Hz default for servos)

### 3.2 — Servo Abstraction
- [x] `hat/servo.rs`: Angle → pulse_us conversion
  - `pulse_us = 500 + (angle / 180) × 2000`
- [x] `set_servo_pulse_us` IPC method
- [x] `set_servo_angle` IPC method
- [x] Channel validation (0–11), pulse range validation (500–2500 µs)
- [x] Angle range validation (0–180°)

### 3.3 — TTL Lease Watchdog
- [x] Per-channel lease tracking: `(channel, expires_at)`
- [x] Background watchdog task (polls every `watchdog_poll_ms`)
- [x] Expired lease → idle channel (pulse_us = 0)
- [x] Client disconnect → release all leases for that client
- [x] Warning log on lease expiry
- [x] Unit tests for watchdog timing

### Phase 3 Exit Criteria
- [x] Servo commands work via IPC
- [x] TTL watchdog idles channels on expired leases
- [x] Client disconnect cleans up leases
- [x] All tests pass without hardware

---

## Phase 4 — GPIO & MCU Reset (P1)

**Goal**: Named GPIO pin abstraction and MCU reset capability.

### 4.1 — GPIO Pins
- [x] `hat/gpio.rs`: Named pin enum (D4, D5, MCURST, SW, LED)
- [x] BCM pin mapping
- [x] Direction configuration (input/output)
- [x] Read/write operations

### 4.2 — MCU Reset
- [x] `reset.rs`: Assert BCM5 low for ≥ 10 ms, release high
- [x] `reset_mcu` IPC method
- [x] Response includes `reset_ms` duration
- [x] Safety: debounce / rate-limit reset requests

### Phase 4 Exit Criteria
- [x] GPIO readable/writable via IPC
- [x] MCU reset works via IPC
- [x] All tests pass without hardware

---

## Phase 5 — Hardening & Deployment

**Goal**: Production-ready daemon with CI, cross-compilation, and deploy tooling.

### 5.1 — CI Pipeline
- [x] GitHub Actions workflow: test, clippy, fmt, cross-compile (`.github/workflows/ci.yml`)
- [x] Cross-compile for `aarch64-unknown-linux-gnu` (via `cross`)
- [x] Binary artifact uploaded to GitHub Releases (on `v*` tags via `softprops/action-gh-release`)

### 5.2 — Deploy Script
- [x] `scripts/deploy.sh`: Download binary, verify SHA-256, atomic swap, restart service
- [x] Version pinning in script arguments (`./deploy.sh <version> [<pi-host>]`)
- [x] Rollback support (keep previous binary as `nomopractic.bak`)

### 5.3 — Integration Testing on Pi
- [x] End-to-end test: start daemon → connect via socket → verify HAT responses
- [x] Battery voltage sanity check (voltage in expected range)
- [x] Servo sweep test (0° → 180° → 0°)
- [x] MCU reset test

**Results (v0.1.0, 2026-03-10, Pi Zero 2W / PicarX):**

| Test | Result | Notes |
|------|--------|-------|
| T1 Health | PASS | `status:ok`, `schema_version:1.0.0`, `hat_address:0x14` |
| T2 Battery voltage | PASS | `raw:3329`, `voltage_v:8.06 V` (2S LiPo, healthy) |
| T3 Servo sweep | PASS | P0: 0°→180°→90°, pulses 500/2500/1500 µs, physical movement confirmed |
| T4 MCU reset | PASS | `reset_ms:10` |
| T5 Post-reset health | PASS | Daemon survived reset, `uptime_s:934` |

**Bugs found and fixed during testing:**
- ADC command byte was `0x10 + channel`; correct formula is `0x10 \| (7 - channel)` (robot-hat register map)
- Battery scaling was `raw × 3`; correct formula is `(raw / 4095) × 3.3 × 3.0` (12-bit ADC, 3.3 V ref, 3:1 divider)
- Single-shot `socat` kills servo immediately via TTL-on-disconnect; use a persistent connection for servo testing

### 5.4 — Raw ADC IPC Method
- [x] `read_adc` IPC method: expose raw ADC reads for all channels A0–A7
- [x] Channel validation (0–7), `INVALID_PARAMS` on out-of-range
- [x] `HARDWARE_ERROR` propagated on I2C failure
- [x] Response: `{ channel, raw_value }`
- [x] Unit tests: valid channel, invalid channel, raw value passthrough

### 5.5 — Code Consolidation
- [x] Deduplicate `MAX_CHANNEL = 11`: export as `pub const` from `pwm.rs`, import in `servo.rs`

### 5.6 — Daemon State Methods
- [x] `get_servo_status` IPC method: active lease list with `channel`, `ttl_remaining_ms`, `conn_id`
- [x] `get_mcu_status` IPC method: `resets_since_start` counter, `last_reset_s_ago` (null if never reset)
- [x] `reset_count` and `last_reset_at` unified under a single `Mutex<McuState>` in `Handler`
- [x] Unit tests for both methods

### 5.7 — nomothetic Integration
- [x] Implement `nomothetic.hat.HatClient` in Python repo
- [x] Add HAT REST endpoints to `nomothetic.api` (GET /api/hat/battery, POST /api/hat/servo, POST /api/hat/reset)
- [x] End-to-end: REST → HatClient → Unix socket → nomopractic → I2C → HAT
- [x] Mock-socket tests in nomothetic (20 tests in `tests/test_hat.py`)

### Phase 5 Exit Criteria
- CI green on every push
- Binary deployable to Pi via script
- nomothetic REST endpoints work end-to-end with nomopractic

---

## Phase 6 — DC Motor Control (P1)

**Goal**: Drive PicarX DC wheels (and up to 4 motors generically) via the
Robot HAT V4 TC1508S H-bridge driver. Includes TTL lease watchdog (same
safety model as servos) and config-driven wiring.

**Pre-requisite bug fix (6.0)**: The existing `set_channel_pulse_us` register
formula `REG_CHN + channel * 4` is only correct for channel 0; channels 1–11
compute the wrong register address, and channels 12–13 collide with timer
config registers. The correct formula (from the SunFounder reference
implementation) is `REG_CHN + channel`. Discovered during Phase 6 analysis;
fixed as the first step of this phase.

### 6.0 — PWM Register Formula Fix (prerequisite)
- [x] `hat/pwm.rs`: Fix `set_channel_pulse_us` register: `REG_CHN + channel`
      (was `REG_CHN + channel * 4`)
- [x] Fix `init_pwm` to initialize timers 0–2 (channels 0–11, stride-1 per
      timer group) instead of only timers 0 and 4
- [x] Add `init_motor_pwm(hat, freq_hz)` — initializes timer 3 (channels 12–15)
- [x] Add `set_motor_channel_duty_pct(hat, channel, pct)` — percentage-based
      duty write for motor channels 12–15 (bypasses servo pulse-width path)
- [x] Add `MOTOR_FREQ`, `MOTOR_MIN_CHANNEL`, `MOTOR_MAX_CHANNEL` constants
- [x] Update all affected unit tests

### 6.1 — Motor Config
- [x] `config.rs`: `MotorConfig { pwm_channel, dir_pin_bcm, reversed }` struct
- [x] `motors: Vec<MotorConfig>` array (up to 4 entries) in `Config`
- [x] `motor_default_ttl_ms: u64` field in `Config`
- [x] Default: PicarX 2-motor wiring
  - Motor 0: `pwm_channel=12`, `dir_pin_bcm=24` (D5)
  - Motor 1: `pwm_channel=13`, `dir_pin_bcm=23` (D4)
- [x] Validation: `pwm_channel` ∈ 12–15, max 4 motors, `motor_default_ttl_ms > 0`
- [x] Update `apply_env_overrides` and config tests

### 6.2 — GPIO BCM Helper
- [x] `hat/gpio.rs`: `write_gpio_bcm(gpio, bcm, high)` — drives an arbitrary
      BCM output pin (used by motor driver for config-specified direction pins)

### 6.3 — Motor Driver (`hat/motor.rs`)
- [x] New module `hat/motor.rs`
- [x] `set_motor_speed(hat, gpio, pwm_channel, dir_pin_bcm, reversed, speed_pct)`:
  - `speed_pct` clamped to −100.0–+100.0 (negative = reverse)
  - Direction computed as `forward = (speed_pct >= 0) XOR reversed`
  - Writes direction GPIO before PWM duty (avoid momentary wrong-direction torque)
- [x] `idle_motor(hat, pwm_channel)` — zero duty without touching direction pin
- [x] `MotorError { Hat(HatError), Gpio(GpioError) }` error type
- [x] Unit tests: forward, backward, stop, reversed flag, speed clamping,
      invalid channel rejection

### 6.4 — Motor IPC Methods
- [x] `set_motor_speed`: `{ channel, speed_pct, ttl_ms? }` — IPC channel 0–3
      maps to `config.motors[channel]`
- [x] `stop_all_motors`: `{}` — idle all configured motors, clear motor leases
- [x] `get_motor_status`: returns active motor leases
- [x] Wired up in `ipc/handler.rs` with `motor_lease_manager: Arc<LeaseManager>`
- [x] Motor channels idled on client disconnect (same pattern as servos)
- [x] `motor_error_code()` helper for IPC error classification

### 6.5 — Motor Watchdog
- [x] `servo.rs`: add `revoke_channel(channel)` to `LeaseManager`
- [x] `ipc/mod.rs`: poll motor leases in `watchdog_task`; call `idle_motor` on expiry

### 6.6 — Startup Init
- [x] `main.rs`: call `pwm::init_motor_pwm` when `config.motors` is non-empty

### Phase 6 Exit Criteria
- [x] `set_motor_speed` drives wheels via IPC with signed-percentage control
- [x] TTL watchdog stops motors on lease expiry
- [x] Client disconnect stops all held motor channels
- [x] All tests pass without hardware
- [x] `config.toml` documents motor wiring

---

## Phase 7 — Vehicle Convenience Methods (P1)

**Goal**: High-level IPC methods that operate on named peripherals (steering
servo, camera servos, all motors together, grayscale sensors) using a single,
coordinated IPC call. Channel-to-peripheral mappings are defined in
`config.toml` so the daemon is the single source of truth.

### 7.1 — Named Peripheral Config
- [x] `config.rs`: `ServoChannels { camera_pan, camera_tilt, steering }` struct
  - Each field is `Option<u8>`, allowing individual servos to be disabled
  - Defaults: `camera_pan=Some(0)`, `camera_tilt=Some(1)`, `steering=Some(2)`
  - Validation: channel must be in 0–11 range when `Some`
- [x] `config.rs`: `SensorChannels { grayscale: [u8; 3] }` struct
  - Default: `[0, 1, 2]` (A0 = left, A1 = centre, A2 = right for PicarX)
  - Validation: each channel must be in 0–7 range
- [x] Both structs added to `Config`; `config.toml` updated with
      `[servos]` and `[sensors]` sections

### 7.2 — Drive IPC Method
- [x] `drive { speed_pct, ttl_ms? }`: set all configured motors simultaneously
  - Atomic — no inter-motor gap or round trips
  - Returns `{ speed_pct, motors: N }`

### 7.3 — Named Servo IPC Methods
- [x] `steer { angle_deg, ttl_ms? }`: set steering servo (`config.servos.steering`)
- [x] `pan_camera { angle_deg, ttl_ms? }`: set camera pan (`config.servos.camera_pan`)
- [x] `tilt_camera { angle_deg, ttl_ms? }`: set camera tilt (`config.servos.camera_tilt`)
- [x] All three return `{ servo, channel, angle_deg, pulse_us }`
- [x] `INVALID_PARAMS` returned if the named servo is disabled (`None`)

### 7.4 — Grayscale Sensor IPC Method
- [x] `read_grayscale {}`: read all three grayscale ADC channels in one call
  - Channel indices taken from `config.sensors.grayscale`
  - Returns `{ channels: [u8; 3], values: [u16; 3] }`

### 7.5 — Tests
- [x] config.rs: 6 new unit tests for ServoChannels/SensorChannels validation
- [x] handler.rs: 10 new unit tests for all 5 new IPC methods

### Phase 7 Exit Criteria
- [x] All 5 new IPC methods work via socket
- [x] Named peripheral channels configurable in `config.toml`
- [x] All tests pass without hardware (138 total)
- [x] `cargo clippy -- -D warnings` clean
- [x] `cargo fmt --check` clean

---

## Priority Legend

| Label | Meaning |
|-------|---------|
| **P0** | Must-have for initial deployment |
| **P1** | Important but not blocking deployment |
| **P2** | Future enhancement |

## Current Status

| Phase | Name | Status | Tests |
|-------|------|--------|-------|
| 1 | Foundation & IPC Scaffold | ✅ Complete | 19 |
| 2 | I2C & Battery Voltage | ✅ Complete | 31 |
| 3 | PWM & Servo Control | ✅ Complete | 62 |
| 4 | GPIO & MCU Reset | ✅ Complete | 82 |
| 5 | Hardening & Deployment | ✅ Complete | 89 |
| 6 | DC Motor Control | ✅ Complete | 112 |
| 7 | Vehicle Convenience Methods | ✅ Complete | 138 |
| 8 | Peripheral Expansion | ✅ Complete | 149 |

---

## Phase 8 — Peripheral Expansion (P1)

**Goal**: Add support for the Robot HAT V4 ultrasonic distance sensor and
speaker amplifier enable pin. Expands the GPIO pin table and adds three new
IPC methods backed by config-driven pin assignments.

### 8.1 — GPIO Pin Expansion
- [x] `hat/gpio.rs`: Added `D2` (BCM 27), `D3` (BCM 22), `SpeakerEn` (BCM 20)
  to `GpioPin` enum
- [x] `bcm()`, `name()`, `from_name()` match arms updated for new variants
- [x] `is_output()` updated: `D3` (ECHO, input) excluded alongside `Sw`

### 8.2 — Ultrasonic Sensor (`hat/ultrasonic.rs`)
- [x] New module `hat/ultrasonic.rs`
- [x] `read_distance_cm(gpio, trig_bcm, echo_bcm, timeout_ms) -> Result<f64, UltrasonicError>`:
  - Quiesce TRIG (low, sleep 1 ms) → pulse TRIG high for 10 µs → wait ECHO
    high → time ECHO pulse → compute `elapsed_s × 34330 / 2`
  - Valid range: 2–400 cm (HC-SR04 spec); out-of-range returns `NoEcho`
- [x] `UltrasonicError { Gpio(GpioError), Timeout(u64), NoEcho }`
- [x] 3 unit tests

### 8.3 — UltrasonicConfig
- [x] `config.rs`: `UltrasonicConfig { trig_pin_bcm: u8, echo_pin_bcm: u8, timeout_ms: u64 }`
  - Defaults: TRIG = BCM 27 (D2), ECHO = BCM 22 (D3), timeout = 20 ms
- [x] `speaker_en_pin_bcm: u8` field added to `Config` (default: 20 = BCM 20)
- [x] `config.toml` updated with `[ultrasonic]` section

### 8.4 — IPC Methods
- [x] `read_ultrasonic {}` → `{ distance_cm: f64 }`
  - Calls `ultrasonic::read_distance_cm` with config-specified pins/timeout
  - `HARDWARE_ERROR` on GPIO failure; `TIMEOUT` on measurement timeout; `NO_ECHO` when object is out of sensor range (2–400 cm)
- [x] `enable_speaker {}` → `{ enabled: true, pin_bcm: 20 }`
  - Writes BCM 20 (`SpeakerEn`) high via GPIO bus
- [x] `disable_speaker {}` → `{ enabled: false, pin_bcm: 20 }`
  - Writes BCM 20 low

### 8.5 — Tests
- [x] `ipc/handler.rs`: 3 unit tests — `enable_speaker_returns_enabled_true`,
      `disable_speaker_returns_enabled_false`, `enable_then_disable_speaker_toggles_pin`

### Phase 8 Exit Criteria
- [x] All 3 new IPC methods dispatched correctly
- [x] GPIO pin table complete for PicarX sensors & speaker
- [x] All 149 tests pass without hardware
- [x] `cargo clippy -- -D warnings` clean
- [x] `cargo fmt --check` clean

---

### v0.3.0 Release Prep

**Goal**: Align config strategy with `nomothetic`, remove legacy files, and
confirm checks pass before tagging.

- [x] `config.toml.example` renamed to `config.toml` — defaults committed to
      repo; no copy step required at install
- [x] `docs/releases/` removed (GitHub auto-generates release notes from tags)
- [x] Version bumped to `0.3.0` in `Cargo.toml`
- [x] `cargo clippy -- -D warnings` clean
- [x] `cargo fmt --check` clean
- [x] All tests pass

---

## Upcoming
