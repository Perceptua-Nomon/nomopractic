# ADR-005: Wi-Fi Soft AP as Proximity Pairing Channel

## Status

Accepted

## Date

2026-05-15

## Supersedes

- ADR-001: BLE GATT Server in nomopractic
- ADR-002: Binary Protocol for BLE GATT
- ADR-003: BLE Security Model
- ADR-004: BLE Simplification — Native OS Pairing + JSON Relay

---

## Context

The BLE GATT server (ADR-001 through ADR-004) was implemented to solve a
single bootstrap problem: *before* the nomon device joins a known Wi-Fi
network, the mobile app has no IP-level path to reach the device. BLE
provided the out-of-box discovery and provisioning channel.

After operating the BLE stack across nomopractic, nomothetic, and nomotactic,
several problems emerged that make BLE untenable as a production pairing
channel:

### 1. OS bonding instability

BlueZ passkey pairing works in controlled environments but is brittle in
practice. OS-level bonding state drifts: re-installing the app on mobile, OS
Bluetooth resets, or rebooting the Pi without clearing BlueZ bond entries leaves
devices in an unpairable state requiring manual `bluetoothctl remove` intervention.
There is no user-facing recovery path that does not require SSH to the Pi.

### 2. Platform fragmentation

- **Web Bluetooth** is available only in Chrome/Edge with an origin-trial flag;
  absent in Firefox, Safari, and all iOS browsers. The nomotactic web app cannot
  reliably provide BLE pairing to the majority of users.
- **react-native-ble-plx** requires linking native modules and Expo prebuild;
  it prevents use of the Expo Go development client and inflates the app bundle.
- The native module dependency makes CI on non-macOS agents harder and
  eliminates the ability to run a production web-only deployment.

### 3. Shared antenna contention

The BCM43436s on Pi Zero 2W shares a single antenna between Wi-Fi and BLE.
Running BLE advertising while simultaneously maintaining a Wi-Fi station link
degrades Wi-Fi throughput and introduces latency spikes on the motor/servo
control path.

### 4. Protocol duplication

The BLE bridge (ADR-004) forwarded raw NDJSON to the IPC handler, adding a
second transport layer that was semantically identical to the Unix socket. The
`authenticate`, `wifi_scan`, `wifi_connect`, and `wifi_status` IPC methods
existed exclusively to serve BLE clients — nomothetic never called them over
the Unix socket. Removing BLE removes four dead IPC methods, their tests, and
their Cargo dependencies (`bluer`, `tokio-stream`, `jsonwebtoken`, `chrono`).

### 5. Simpler alternative exists

The nomon device already runs the nomothetic HTTPS stack on port 8443. Any
HTTP client — including any browser on any platform — can reach it directly
if the client is on the same network as the device. A Wi-Fi Soft AP satisfies
the bootstrap requirement with zero new protocol, zero native modules, and full
cross-platform compatibility.

---

## Decision

**Remove all BLE code. Use a Wi-Fi Soft AP as the sole proximity pairing
channel.**

### Soft AP behaviour

When the nomon device cannot associate with any known Wi-Fi network
(NetworkManager is in a disconnected or "limited" state), it automatically
starts a WPA2 protected hotspot with:

| Property | Value |
|----------|-------|
| SSID | `nomon-<last4-hex-of-MAC>` (e.g. `nomon-3a2f`) |
| Password | Contents of `/var/lib/nomon/pairing_secret` (the same 6-digit secret already displayed at nomothetic startup) |
| Pi IP on AP | `192.168.4.1` |
| mDNS hostname | `nomon.local` (Avahi on the same interface) |
| Service | nomothetic HTTPS on `192.168.4.1:8443` |

The AP is managed by NetworkManager's built-in hotspot mode, triggered by a
systemd service (`nomon-softap.service`) that:

1. Queries `nmcli general status` on a polling interval (or via NetworkManager
   D-Bus events).
2. When no uplink is present for > 30 s, runs:
   ```
   nmcli con add type wifi ifname wlan0 con-name nomon-ap \
       ssid "nomon-XXXX" mode ap \
       wifi-sec.key-mgmt wpa-psk \
       wifi-sec.psk "$(cat /var/lib/nomon/pairing_secret)" \
       ipv4.method shared ipv4.addresses 192.168.4.1/24
   nmcli con up nomon-ap
   ```
3. When a known uplink re-appears, brings down `nomon-ap` and removes the
   connection profile.

A shell script `scripts/ap-mode.sh` encapsulates the state machine and is
invoked by the systemd unit. The script accepts `up` / `down` / `status`
subcommands for manual use during development and deployment.

### Pairing UX

1. User powers on their nomon device.
2. If the device has not yet been provisioned with Wi-Fi credentials:
   - The Soft AP appears within ~30 s.
   - The user connects their phone or laptop to the `nomon-XXXX` Wi-Fi network.
   - The password is the 6-digit secret printed to the nomothetic startup log
     (same secret already used for HTTP pairing in Phase 17).
3. User opens `https://192.168.4.1:8443` (or `https://nomon.local:8443`) in any
   browser, **or** opens the nomotactic app which already targets `DEVICE_API_URL`.
4. The existing `HttpPairingForm` component submits the secret to
   `POST /api/device/auth/pair` — no new screen, no new endpoint.
5. On success, the app receives device JWT tokens and proceeds to the device
   detail page exactly as before.
6. The user then navigates to the Wi-Fi settings in the app (or via
   `GET /api/system/wifi/status` + the forthcoming Wi-Fi provisioning endpoint)
   to join their home network. Once joined, the Soft AP shuts down and the
   device switches to normal station mode.

### What is deleted

| Artifact | Location | Reason |
|----------|----------|--------|
| `src/ble/` directory | nomopractic | Entire BLE GATT server module |
| `src/wifi.rs` | nomopractic | Used exclusively by BLE WiFi provisioning IPC |
| `src/config.rs` `BleConfig` struct + `ble` field | nomopractic | No BLE server to configure |
| `src/main.rs` BLE spawn block (`#[cfg(feature = "ble")]`) | nomopractic | No BLE server |
| `src/ipc/handler.rs` `handle_wifi_scan/connect/status/authenticate` | nomopractic | Dead IPC methods (BLE-only callers) |
| `Cargo.toml` `bluer`, `tokio-stream`, `jsonwebtoken`, `chrono` | nomopractic | No longer used |
| `Cargo.toml` `[features] ble` | nomopractic | Feature flag removed |
| `docs/pairing.md` | nomopractic | BLE developer guide, superseded by AP setup |
| `lib/ble.ts` | nomotactic | BLE abstraction layer |
| `lib/transport.tsx` BLE mode | nomotactic | `TransportMode`, `connectViaBle`, `activateSession` (BLE) |
| `lib/local-devices.ts` `bleDeviceId` field | nomotactic | BLE device identity no longer stored |
| `components/BlePairingFlow.tsx` | nomotactic | BLE paired-device list |
| `components/AddDeviceSection.tsx` | nomotactic | BLE scan + connect |
| `app/(app)/index.tsx` BLE component imports | nomotactic | Replace with `HttpPairingForm` |
| `app/(app)/device/[id].tsx` `activateSession` BLE call | nomotactic | No BLE session to resume |
| `app/(app)/register-device.tsx` BLE framing copy | nomotactic | Screen now entered directly via Soft AP |
| `react-native-ble-plx` | nomotactic `package.json` | Native BLE module removed |
| `BLE_QUICK_START.md` | nomotactic | Replaced by AP onboarding guide |
| `docs/ble-testing-guide.md` | nomotactic | Superseded |
| `pairing.py` BLE docstring reference | nomothetic | Docstring update only |
| `docs/hat_ipc_schema.md` BLE transport note + wifi/authenticate sections | nomothetic | Methods deleted |

### What is NOT changed

- `nomothetic.pairing` module — `_write_shared_secret` continues to write
  `/var/lib/nomon/pairing_secret`. The Soft AP uses the same file as its WPA2
  password, so the shared-secret lifecycle is unchanged.
- `POST /api/device/auth/pair` REST endpoint — identical; Soft AP users reach
  it over HTTPS on `192.168.4.1:8443`.
- `HttpPairingForm` component — already exists; now promoted to the primary
  (and only) pairing UI in `app/(app)/index.tsx`.
- nomographic — no schema changes. Device registration writes the same VIN and
  metadata regardless of how the user reached the pairing endpoint.
- IPC Unix socket — all non-BLE IPC methods unchanged.

---

## Consequences

### Positive

- **Platform parity.** Pairing works identically in any browser, any OS, any
  device — including iOS Safari and Firefox, where Web Bluetooth was absent.
- **Simpler dependency tree.** Removes `bluer`, `tokio-stream`,
  `react-native-ble-plx`, and their transitive deps; Expo Go works again.
- **Smaller nomopractic binary.** Eliminating the BLE feature flag and four
  dead IPC methods reduces compile time and binary size.
- **Antenna relief.** Wi-Fi throughput is no longer degraded by simultaneous
  BLE advertising; motor/servo latency improves slightly in constrained
  RF environments.
- **No OS bonding debt.** No BlueZ bond state to drift; factory-reset is a
  single `nmcli con delete nomon-ap`.
- **Dead code removed.** `wifi_scan`, `wifi_connect`, `wifi_status`,
  `authenticate` IPC methods, and `lib/wifi.rs` were maintained solely for BLE
  callers. Their removal leaves the IPC surface honest.

### Negative / Regressions

- **No offline fallback control.** BLE allowed command relay when Wi-Fi was
  absent. With Soft AP, the Pi is the AP — the app can reach it, but the Pi
  has no internet. This is acceptable for proximity control (the usual
  use-case) but not for remote control.
- **AP transition latency.** The 30 s timeout before AP mode starts means
  out-of-box unboxing has a short wait. A progress indicator in the app
  partially mitigates this.
- **WPA2 password = pairing secret.** The Soft AP password is the same 6-digit
  numeric secret shown at startup. This is intentional (single secret, dual
  use) but means any device on the AP can attempt HTTPS requests. The
  `POST /api/device/auth/pair` rate limit (Phase 17) prevents brute-force
  without knowledge of the secret.
- **Requires NetworkManager.** The AP mode script depends on `nmcli` ≥ 1.x
  being present and managing `wlan0`. Raspbian Bookworm (default since 2023)
  satisfies this; older dhcpcd-only setups do not.
