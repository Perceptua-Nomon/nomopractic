# ADR-001: BLE GATT Server in nomopractic

## Status

Superseded by [ADR-005](005-wifi-soft-ap.md)

## Date

2026-04-15

## Context

nomon robots need Bluetooth Low Energy (BLE) support for two scenarios:

1. **Initial device discovery and pairing** — before WiFi is configured, the
   mobile app has no IP-level connectivity to the Pi. BLE provides a
   discovery and bootstrap channel.
2. **Fallback control** — when WiFi is unavailable or unreliable, basic motor,
   servo, battery, and sensor commands can be sent over BLE.

The Raspberry Pi Zero 2W's BCM43436s chip supports BLE 4.2 via the BlueZ
Linux stack. The question is which process hosts the BLE GATT server.

## Decision

The BLE GATT server is implemented in **nomopractic** (Rust) using the
`bluer` crate (BlueZ D-Bus bindings for async Rust).

BLE commands received via GATT characteristic writes are relayed as NDJSON
to the existing `ipc/handler.rs` method handler — the same code path that
serves Unix socket IPC requests. (The earlier binary command protocol described
in ADR-002 is superseded by ADR-004.)

## Alternatives Considered

### 1. BLE server in nomothetic (Python)

- **Pro**: Python has `bleak` for BLE, closer to the REST API layer.
- **Con**: Introduces a second process doing hardware-adjacent work. nomothetic
  would need to forward BLE commands to nomopractic via IPC anyway — adding a
  network hop. Python's GIL and asyncio model add latency to real-time motor
  control. Breaks the architecture rule that all hardware-facing code lives in
  nomopractic.
- **Rejected.**

### 2. Standalone BLE daemon (third process)

- **Pro**: Clean separation of concerns.
- **Con**: Adds a third systemd service, another IPC boundary, more deployment
  complexity, and more failure modes. The Pi Zero 2W has only 512 MB RAM;
  minimizing process count matters. The BLE command set is a strict subset of
  the existing IPC methods — a new daemon would duplicate dispatch logic.
- **Rejected.**

### 3. BLE server in nomopractic (chosen)

- **Pro**: All Pi hardware access stays in one Rust daemon. BLE commands reuse
  the existing handler (zero code duplication). `bluer` provides async D-Bus
  integration that fits naturally into the tokio runtime. Single deployment
  artifact. Minimal latency from BLE characteristic write to motor register
  write.
- **Con**: Adds BlueZ D-Bus dependency to nomopractic. Increases binary size
  (~200 KB for `bluer` + `zbus`). `bluer` requires the BlueZ daemon to be
  running on the Pi (already present on Raspberry Pi OS).
- **Accepted.**

## Consequences

- `bluer` and its transitive dependencies (`zbus`, `futures-core`) are added
  to `Cargo.toml`.
- The Pi must have BlueZ installed and `bluetooth.service` enabled (already
  the default on Raspberry Pi OS).
- nomopractic's `config.toml` gains a `[ble]` section for enabling/disabling
  BLE, setting the device advertising name, and referencing the
  `pairing_secret_path` — a file containing the 6-digit numeric passkey read
  by the BlueZ passkey agent.
- The `bluer` GATT server runs as a Tokio task within the existing runtime;
  no new threads or processes.
- BLE support is compile-time optional via a Cargo feature flag (`ble`) to
  keep the binary small for non-BLE deployments and to avoid `bluer`
  dependencies in CI on x86_64 where BlueZ is not available.
- Unit tests for the BLE NDJSON relay bridge logic do not require BlueZ
  and run on any platform. Integration tests for the full GATT stack require
  a BlueZ-capable environment.

## Subsequent Decisions

- **[ADR-004](004-ble-simplification.md)** — BLE Simplification: Native OS Pairing + NDJSON Relay.
  Replaces the binary command protocol (ADR-002) and application-layer encryption (ADR-003).
  This ADR (001) remains accepted — the decision to host the GATT server in nomopractic is preserved.
  ADR-002 and ADR-003 are superseded.

## References

- `bluer` crate: https://crates.io/crates/bluer
- BlueZ GATT server API: https://git.kernel.org/pub/scm/bluetooth/bluez.git/tree/doc/gatt-api.txt
- BCM43436s BLE 4.2 specification (Pi Zero 2W datasheet)
- nomopractic architecture: `docs/architecture.md`
