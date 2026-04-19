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
| MCU reset | Assert/release BCM5 GPIO |
| Named GPIO | D4, D5, MCURST, SW, LED |
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

The BLE GATT server uses OS-level Bluetooth passkey pairing (ADR-004). A 6-digit numeric
passkey is read from the filesystem at startup.

```bash
# On the Pi, create the passkey file:
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

On mobile: scan for the device named "nomon", enter the 6-digit passkey when the OS prompts,
then the app calls `authenticate` to receive a device-scoped JWT.

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
