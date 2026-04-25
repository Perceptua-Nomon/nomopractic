# ADR-004: BLE Simplification — Native OS Pairing + JSON Relay

## Status

Accepted

## Date

2026-04-17

## Context

The Phase 13 BLE implementation (ADR-001, ADR-002, ADR-003) works but carries
significant complexity:

| Concern | Current (Phase 13) | Proposed |
|---------|---------------------|----------|
| Pairing | Custom secret exchange via GATT characteristic write + HKDF key derivation | OS-level Bluetooth passkey pairing (BlueZ agent on Pi, native iOS/Android dialog) |
| Encryption | Application-layer AES-128-CCM with counter-based replay protection | BLE link-layer encryption (provided free by OS bonding) |
| Protocol | Compact binary protocol with 10 opcodes, fixed-point encoding (ADR-002) | NDJSON — same format as Unix socket IPC |
| GATT structure | 4 services, 10+ characteristics | 1 service, 2 characteristics |
| WiFi provisioning | Custom binary encoding on separate BLE service | Standard IPC methods (`wifi_scan`, `wifi_connect`, `wifi_status`) |
| JWT bootstrap | Embedded in custom pairing ceremony (salt + JWT concatenated on Auth Token characteristic) | Dedicated `authenticate` IPC method called after bonding |
| Code | ~2,900 lines Rust + ~1,400 lines TypeScript + ~200 lines tests | Estimated ~600 lines Rust + ~400 lines TypeScript |
| Crypto dependencies | `aes`, `ccm`, `hkdf`, `sha2`, `subtle`, `rand`, `@noble/ciphers`, `@noble/hashes` | None (OS provides link-layer crypto) |

The custom binary protocol (ADR-002) was designed for bandwidth efficiency —
87-byte JSON motor commands compressed to 8-byte binary frames. In practice:

1. **MTU negotiation works.** BLE 4.2 on Pi Zero 2W consistently negotiates
   244-byte MTU. Most IPC responses fit in a single BLE notification even as
   JSON.
2. **Motor command rate is modest.** The app sends 2–5 commands/sec during
   active driving, not the 10+ Hz assumed in ADR-002. NDJSON throughput is
   sufficient.
3. **Developer friction is real.** Binary codec bugs are harder to diagnose
   than plain JSON. Every new IPC method requires a new opcode, encoder,
   decoder, and test on both sides.
4. **The crypto layer duplicates what the OS provides for free.** Bonded BLE
   connections already have AES-CCM link-layer encryption. Our application
   layer adds complexity without meaningful security improvement.

### Why not just remove the binary codec and keep the custom pairing?

The custom pairing ceremony (write secret → derive key → issue JWT) is tightly
coupled to the AES-128-CCM encryption layer. The session key is derived
specifically for encrypting subsequent command frames. Removing the encryption
but keeping custom pairing would leave an authenticated-but-unencrypted
channel — strictly worse than OS-level bonding which provides both.

### Passkey Entry vs Just Works

BLE 4.2 "Just Works" pairing (the default when no agent is registered)
provides encrypted transport but **no authentication** — any nearby device
can pair. "Passkey Entry" pairing requires the user to enter a 6-digit
numeric code displayed (or known) on the peripheral. This:

- Authenticates the user (they must know the passkey)
- Triggers bonding (the OS remembers the device)
- Enables link-layer AES-CCM encryption
- Works natively on iOS, Android, and Linux (no app-layer crypto needed)

The nomon's passkey is read from `/var/lib/nomon/pairing_secret` — the same
file currently used for the custom pairing secret. The operator sets the
passkey during device setup; it is displayed in startup logs.

## Decision

**Replace the custom BLE binary protocol, application-layer encryption, and
pairing ceremony with OS-level Bluetooth passkey pairing and plain NDJSON
relay over a single GATT service.**

### New Architecture

```
┌──────────────┐         BLE (bonded, link-encrypted)        ┌──────────────┐
│  nomotactic  │                                              │ nomopractic  │
│  (app)       │                                              │ (GATT server)│
└──────┬───────┘                                              └──────┬───────┘
       │                                                             │
       │ 1. OS passkey pairing (6-digit code)                        │
       │    iOS/Android shows native dialog                          │
       │    BlueZ passkey agent returns code from file               │
       │                                                             │
       │ 2. NDJSON over single GATT service:                         │
       │    Command Write (e3a12001):                                │
       │      {"id":"1","method":"authenticate","params":{}}         │
       │    Response Notify (e3a12002):                              │
       │      {"id":"1","ok":true,"result":{"jwt":"eyJ..."}}        │
       │                                                             │
       │ 3. JWT stored → subsequent HTTPS requests use it            │
       │                                                             │
       │ 4. WiFi provisioning (same NDJSON):                         │
       │    {"id":"2","method":"wifi_scan","params":{}}              │
       │    {"id":"2","ok":true,"result":{"networks":[...]}}        │
       │                                                             │
       │ 5. Any IPC method works over BLE (same handler):            │
       │    {"id":"3","method":"drive","params":{"speed_pct":30}}    │
       │    {"id":"3","ok":true,"result":{"speed_pct":30,"motors":2}}│
```

### GATT Service Structure

Single service replacing the previous 4:

**nomon Service** (`e3a10001-7b2a-4b9c-8f5a-2b7d6e4f1a3c`)

| Characteristic | UUID | Properties | Description |
|----------------|------|------------|-------------|
| Command Write | `e3a12001-7b2a-4b9c-8f5a-2b7d6e4f1a3c` | Write | Client writes NDJSON request (chunked at MTU boundary) |
| Response Notify | `e3a12002-7b2a-4b9c-8f5a-2b7d6e4f1a3c` | Notify | Server sends NDJSON response (chunked at MTU boundary) |

### NDJSON Chunking Over BLE

BLE ATT payload is limited to (MTU − 3) bytes per write/notification.
Most NDJSON messages fit in a single ATT payload at the negotiated 244-byte
MTU. For messages exceeding the MTU:

1. **Sender** splits the NDJSON line (including trailing `\n`) into chunks
   of at most (MTU − 3) bytes.
2. Each chunk is sent as a separate GATT write or notification.
3. **Receiver** accumulates bytes in a buffer until `\n` is found, then
   parses and dispatches the complete JSON line.

No binary length-prefix or framing header is needed — NDJSON's `\n`
delimiter serves as the message boundary. This reuses the exact same
framing semantics as the Unix socket IPC path.

### New IPC Methods

Three WiFi methods move from the BLE-specific binary encoding to standard
IPC methods callable from both Unix socket and BLE:

- `wifi_scan` — scan for available WiFi networks via `nmcli`
- `wifi_connect` — connect to a network with SSID and password
- `wifi_status` — query current WiFi connection state

One new method for JWT bootstrapping:

- `authenticate` — generates and returns a JWT; **transport-agnostic** —
  callable from both BLE and Unix socket connections (explicit team decision:
  Unix socket callers such as test tooling and nomothetic service accounts
  need device-scoped JWTs without a BLE connection present)

### Passkey Agent

nomopractic registers a BlueZ passkey agent via `bluer::agent::Agent`.
When a device attempts to pair, the agent reads the numeric passkey from
`pairing_secret_path` and returns it to BlueZ. The file must contain a
6-digit numeric string (000000–999999).

## Alternatives Considered

### 1. Keep binary protocol, remove only crypto

- **Pro**: Preserves bandwidth efficiency.
- **Con**: Still requires maintaining parallel codec on both sides. Every new
  IPC method needs a new opcode. The binary codec is the primary source of
  developer friction. Bandwidth savings are negligible at actual usage rates.
- **Rejected.**

### 2. Use CBOR instead of NDJSON

- **Pro**: More compact than JSON, self-describing, no text overhead.
- **Con**: Adds a new dependency (`ciborium` in Rust, `cbor-x` in
  TypeScript). JSON is already the IPC lingua franca. Developer debugging is
  harder (not human-readable). Bandwidth savings don't justify the new
  dependency.
- **Rejected.**

### 3. Keep custom pairing, drop only encryption

- **Pro**: Maintains explicit authentication step in app.
- **Con**: Authenticated-but-unencrypted channel is worse than OS bonding.
  Custom pairing ceremony UX is worse than native OS passkey dialog. Still
  requires maintaining `session.rs`, `ble-session.ts`.
- **Rejected.**

## Consequences

### Positive

- ~3,500 fewer lines of custom code to maintain across two repos.
- New IPC methods automatically available over BLE — no per-method codec work.
- WiFi provisioning works identically over BLE and Unix socket.
- Native OS pairing dialog is a better UX than typing a secret into a text field.
- Removing 6 crypto crates from `nomopractic` reduces binary size and audit surface.
- Debugging BLE issues becomes trivial — NDJSON is human-readable.

### Negative

- BLE command frames are larger (JSON vs binary). A `drive` command goes from
  7 bytes (binary) to ~65 bytes (JSON). At 2–5 commands/sec, this is well
  within BLE 4.2 throughput (~30 KB/s usable).
- OS passkey pairing requires the user to know the device's passkey before
  connecting. This is acceptable for a personal robot fleet (operator sets up
  the device and knows the passkey).
- Bonding is persistent — the device remembers paired phones. A future
  `reset_pairing` feature (physical button to clear bonds and regenerate
  passkey) is needed for device transfer. **Explicitly out of scope for this
  phase.**

### Migration

- ADR-002 (Binary Protocol) is **superseded** by this ADR.
- ADR-003 (BLE Security Model) is **superseded** by this ADR.
- ADR-001 (BLE GATT Server in nomopractic) remains valid — the GATT server
  stays in nomopractic; only the protocol and security layers change.

## References

- nomopractic ADR-001: BLE GATT Server in nomopractic
- nomopractic ADR-002: Binary Protocol for BLE GATT (superseded)
- nomopractic ADR-003: BLE Security Model (superseded)
- BlueZ agent API: https://git.kernel.org/pub/scm/bluetooth/bluez.git/tree/doc/agent-api.txt
- `bluer` agent module: https://docs.rs/bluer/latest/bluer/agent/index.html
- BLE 4.2 Security Manager spec (Passkey Entry pairing)

## Implementation Status & Verification

- **Runtime implementation**: `nomopractic` contains the simplified BLE server: `src/ble/mod.rs` (agent registration + lifecycle), `src/ble/services.rs` (single service with Command Write + Response Notify), and `src/ble/bridge.rs` (NDJSON relay and chunking).
- **Tests added**: unit tests for `read_passkey()` and a gated integration test scaffold `tests/ble_pairing_integration.rs` (runs only with `--features ble` and `NOMON_RUN_BLE_INTEGRATION=1`).
- **Docs updated**: ADR-002 and ADR-003 are superseded; this ADR documents the current choice and migration.
- **Operational notes discovered during verification**:
  - Some Bluetooth controllers / BlueZ configurations do **not** permit disabling BR/EDR at runtime (`btmgmt bredr off` may be rejected). As a result, BR/EDR may remain available even when LE is used. Design the pairing UX and operator guidance accordingly.
  - The systemd unit is updated to require and start after `bluetooth.service` so that `bluetoothctl discoverable` commands (used at startup/deploy) reliably apply.
  - Logging hardened: the numeric passkey is no longer emitted as a structured log field.

-- **Remaining work**: finalise CI gating for BLE integration tests (hardware-gated or lab runner), sync nomotactic UI expectations with ADR-004, and implement an explicit `reset_pairing` operator action for device transfer scenarios. The previously-provisioned developer smoke scripts and lab-only Python client were removed from the repository; prefer a managed BLE gateway harness for automated lab verification.
