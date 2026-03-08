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
- [ ] `hat/i2c.rs`: Open I2C bus via rppal
- [ ] `read_register(addr, reg, buf)` helper
- [ ] `write_register(addr, reg, data)` helper
- [ ] Shared `Hat` struct holding `rppal::i2c::I2c` behind `tokio::sync::Mutex`
- [ ] Unit tests with mock I2C (trait-based abstraction)

### 2.2 — ADC Read
- [ ] `hat/adc.rs`: Write command byte, read 2-byte result
- [ ] Channel validation (A0–A7)
- [ ] Error handling for I2C failures

### 2.3 — Battery Voltage
- [ ] `hat/battery.rs`: Read ADC channel A4
- [ ] Scaling: `voltage_v = raw_adc × 3`
- [ ] `get_battery_voltage` IPC method wired up in handler
- [ ] Unit test: mock ADC → verify voltage calculation

### Phase 2 Exit Criteria
- Battery voltage readable via IPC
- I2C errors returned as `HARDWARE_ERROR` to client
- All tests pass without hardware (mocked I2C)

---

## Phase 3 — PWM & Servo Control (P0)

**Goal**: Drive servos on PWM channels 0–11. Includes TTL lease safety watchdog.

### 3.1 — PWM Control
- [ ] `hat/pwm.rs`: Prescaler calculation from `CLOCK_HZ` / `SERVO_FREQ`
- [ ] PWM initialization: set prescaler + auto-reload registers
- [ ] Channel pulse write: `REG_CHN + channel * 4`
- [ ] Frequency validation (50 Hz default for servos)

### 3.2 — Servo Abstraction
- [ ] `hat/servo.rs`: Angle → pulse_us conversion
  - `pulse_us = 500 + (angle / 180) × 2000`
- [ ] `set_servo_pulse_us` IPC method
- [ ] `set_servo_angle` IPC method
- [ ] Channel validation (0–11), pulse range validation (500–2500 µs)
- [ ] Angle range validation (0–180°)

### 3.3 — TTL Lease Watchdog
- [ ] Per-channel lease tracking: `(channel, expires_at)`
- [ ] Background watchdog task (polls every `watchdog_poll_ms`)
- [ ] Expired lease → idle channel (pulse_us = 0)
- [ ] Client disconnect → release all leases for that client
- [ ] Warning log on lease expiry
- [ ] Unit tests for watchdog timing

### Phase 3 Exit Criteria
- Servo commands work via IPC
- TTL watchdog idles channels on expired leases
- Client disconnect cleans up leases
- All tests pass without hardware

---

## Phase 4 — GPIO & MCU Reset (P1)

**Goal**: Named GPIO pin abstraction and MCU reset capability.

### 4.1 — GPIO Pins
- [ ] `hat/gpio.rs`: Named pin enum (D4, D5, MCURST, SW, LED)
- [ ] BCM pin mapping
- [ ] Direction configuration (input/output)
- [ ] Read/write operations

### 4.2 — MCU Reset
- [ ] `reset.rs`: Assert BCM5 low for ≥ 10 ms, release high
- [ ] `reset_mcu` IPC method
- [ ] Response includes `reset_ms` duration
- [ ] Safety: debounce / rate-limit reset requests

### Phase 4 Exit Criteria
- GPIO readable/writable via IPC
- MCU reset works via IPC
- All tests pass without hardware

---

## Phase 5 — Hardening & Deployment

**Goal**: Production-ready daemon with CI, cross-compilation, and deploy tooling.

### 5.1 — CI Pipeline
- [ ] GitHub Actions workflow: test, clippy, fmt, cross-compile
- [ ] Cross-compile for `aarch64-unknown-linux-gnu`
- [ ] Binary artifact uploaded to GitHub Releases

### 5.2 — Deploy Script
- [ ] `scripts/deploy.sh`: Download binary, verify SHA-256, atomic swap, restart service
- [ ] Version pinning in script arguments
- [ ] Rollback support (keep previous binary)

### 5.3 — Integration Testing on Pi
- [ ] End-to-end test: start daemon → connect via socket → verify HAT responses
- [ ] Battery voltage sanity check (voltage in expected range)
- [ ] Servo sweep test (0° → 180° → 0°)
- [ ] MCU reset test

### 5.4 — nomothetic Integration
- [ ] Implement `nomothetic.hat.HatClient` in Python repo
- [ ] Add HAT REST endpoints to `nomothetic.api`
- [ ] End-to-end: REST → HatClient → Unix socket → nomopractic → I2C → HAT
- [ ] Mock-socket tests in nomothetic

### Phase 5 Exit Criteria
- CI green on every push
- Binary deployable to Pi via script
- nomothetic REST endpoints work end-to-end with nomopractic

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
| 2 | I2C & Battery Voltage | 🔲 Not Started | — |
| 3 | PWM & Servo Control | 🔲 Not Started | — |
| 4 | GPIO & MCU Reset | 🔲 Not Started | — |
| 5 | Hardening & Deployment | 🔲 Not Started | — |
