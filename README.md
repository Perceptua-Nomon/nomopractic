# nomopractic

Low-latency HAT hardware daemon for the nomon fleet.

## What It Does

nomopractic is a Rust daemon that drives the SunFounder Robot HAT V4 on
Raspberry Pi Zero 2W microcontrollers. It exposes hardware controls over a Unix
domain socket using NDJSON framing, consumed by the Python
[nomothetic](https://github.com/Perceptua/nomothetic) package via its
`HatClient`.

**All hardware register logic lives here.** The Python side contains zero
hardware knowledge — it only sends/receives IPC messages.

## Capabilities

| Feature | Details |
|---------|---------|
| Battery voltage | ADC channel A4, scaled reading |
| Servo control | 12 PWM channels, angle or pulse-width, TTL safety lease |
| DC motor control | 4 channels, speed percentage, TTL safety lease |
| Named vehicle methods | `drive`, `steer`, `pan_camera`, `tilt_camera` |
| Grayscale sensors | 3 ADC channels, raw + normalized readings, calibration |
| Ultrasonic sensor | Distance measurement in cm |
| Audio | Speaker enable/disable, volume and mic gain via ALSA |
| Calibration | Per-motor, per-servo, and grayscale calibration; persist to TOML |
| Routines | Built-in obstacle-avoidance and line-following autonomous routines |
| MCU reset | Assert/release BCM5 GPIO |
| Named GPIO | D4, D5, MCURST, SW, LED — read/write via IPC |
| WiFi | Scan networks, connect, status |
| BLE | GATT server (ADR-004): NDJSON relay over Command Write + Response Notify characteristics |
| IPC | Unix socket + NDJSON at `/run/nomopractic/nomopractic.sock` |
| Config | TOML file + `NOMON_HAT_*` env var overrides |

## Quick Start

```bash
# Build
cargo build

# Run tests (no hardware required)
cargo test

# Lint
cargo clippy -- -D warnings

# Cross-compile for Pi
cross build --target aarch64-unknown-linux-gnu --release
```

## Deployment

```bash
# On the Pi:
sudo cp target/aarch64-unknown-linux-gnu/release/nomopractic /usr/local/bin/
sudo mkdir -p /etc/nomopractic
sudo cp config.toml /etc/nomopractic/config.toml
sudo cp systemd/nomopractic.service /etc/systemd/system/
sudo systemctl enable --now nomopractic
```

Verify:

```bash
echo '{"id":"1","method":"health","params":{}}' | \
  socat - UNIX-CONNECT:/run/nomopractic/nomopractic.sock
```

## Project Structure

```
src/
├── main.rs          Binary entry point (CLI, config, runtime)
├── lib.rs           Library root
├── config.rs        TOML + env configuration
├── ipc/
│   ├── mod.rs       Unix socket listener
│   ├── schema.rs    NDJSON request/response types
│   ├── handler.rs   Method dispatch → HAT drivers
│   └── params.rs    Typed IPC parameter extraction
├── hat/
│   ├── mod.rs       HAT abstraction
│   ├── i2c.rs       I2C read/write helpers
│   ├── pwm.rs       PWM register protocol
│   ├── adc.rs       ADC reads
│   ├── servo.rs     Servo control + TTL watchdog
│   ├── motor.rs     DC motor speed/direction control
│   ├── battery.rs   Battery voltage
│   ├── gpio.rs      Named GPIO pins
│   ├── ultrasonic.rs HC-SR04 distance sensor
│   └── audio.rs     ALSA volume/gain control
├── calibration.rs   Motor/servo/grayscale calibration store
├── routine/
│   ├── mod.rs       Routine engine (start/stop/status)
│   └── explore.rs   Autonomous explore routine
├── ble/             BLE GATT server (behind `ble` feature flag)
│   ├── mod.rs       GATT server lifecycle, BlueZ passkey agent, advertising
│   ├── services.rs  Single GATT service + 2 characteristics (Command Write, Response Notify)
│   └── bridge.rs    NDJSON relay: accumulate chunks → dispatch → chunk response
├── wifi.rs          WiFi control: nmcli scan/connect/status (WifiControl trait)
├── reset.rs         MCU reset (BCM5)
└── testing.rs       Shared test mocks (MockI2c, MockGpio, MockAlsaControl)
```

## BLE Pairing Setup

nomopractic uses OS-level Bluetooth passkey pairing (ADR-004) and an NDJSON
relay over a single GATT service. The daemon reads a 6-digit numeric
passkey from `pairing_secret_path` (default `/var/lib/nomon/pairing_secret`) at
startup.

Behavior added in this branch:
- Deploy installs a `systemd-tmpfiles` entry to ensure `/var/lib/nomon` exists
  with owner `root:nomon` and mode `0750`.
- `nomopractic` will attempt to create and seed `/var/lib/nomon/pairing_secret`
  with a random 6-digit passkey (mode `0640`) if the file is missing so the
  daemon can run standalone for developer testing.

Manual creation (optional):

```bash
sudo mkdir -p /var/lib/nomon
echo "123456" | sudo tee /var/lib/nomon/pairing_secret > /dev/null
sudo chmod 640 /var/lib/nomon/pairing_secret
sudo chown root:nomon /var/lib/nomon/pairing_secret
```

Enable BLE in `config.toml`:

```toml
[ble]
enabled = true
device_name = "nomon"
pairing_secret_path = "/var/lib/nomon/pairing_secret"
```

On mobile: scan for the device named as configured and use an LE-capable
client (native OS pairing, nRF Connect, LightBlue) to initiate an LE GATT
connection — when the BlueZ agent is invoked the passkey returned will match
the file contents and the app can call `authenticate` to receive a device-scoped JWT.

## Documentation

- [Architecture](docs/architecture.md) — system design, data flows, concurrency model
- [Roadmap](docs/roadmap.md) — development phases and progress tracking
- [Hardware Reference](docs/hardware_reference.md) — Robot HAT V4 register map
- [Contributing](CONTRIBUTING.md) — development setup and code guidelines

## Related

- [nomothetic](https://github.com/Perceptua/nomothetic) — Python package (camera, API, telemetry, HAT IPC client)
- [IPC Schema](https://github.com/Perceptua/nomothetic/blob/main/docs/hat_ipc_schema.md) — full protocol specification

## License

MIT
