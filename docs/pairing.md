# Developer pairing guide

This short guide helps developers make a nomon device discoverable, pair
from a phone, and verify the `authenticate` NDJSON flow.

Prerequisites
- A nomon device with the `nomopractic` service installed and running
- `bluetoothctl` available on the device (BlueZ)
- A phone or laptop with BLE capability (iOS/Android recommended)

Quick steps
1. Ensure the device is running the `nomopractic` service (the daemon will
   attempt to create and seed the pairing secret if it is missing):

   sudo systemctl restart nomopractic

2. Make the controller discoverable and pairable for 5 minutes (optional —
   deploy and the systemd unit already set discoverable + pairable on
   startup):

   sudo /usr/bin/bluetoothctl discoverable-timeout 300
   sudo /usr/bin/bluetoothctl pairable on
   sudo /usr/bin/bluetoothctl discoverable on

3. On your phone or laptop (LightBlue / nRF Connect), open BLE scan and
   connect to the device named according to `config.toml` (default `nomon`).
   - Use an LE-capable client to ensure an LE GATT connection — OS pairing
     dialogs that use BR/EDR may show unrelated passkeys.
   - When prompted for the passkey, enter the 6-digit code present in
     `/var/lib/nomon/pairing_secret` (nomopractic uses this file at startup).

4. Verify `authenticate` via your app or a GATT client that writes the
   NDJSON request to the Command Write characteristic and observes the
   Response Notify characteristic. Example request (NDJSON line):

   {"id":"1","method":"authenticate","params":{}}

   Expect `ok:true` and a `result.jwt` field in the JSON response.

Troubleshooting
- If you do not see a passkey prompt on the phone:
   - Check `sudo journalctl -u nomopractic -n 200 --no-pager` for agent registration logs.
   - Confirm `/var/lib/nomon/pairing_secret` contains exactly 6 digits and permissions are `0640` (owner `root`, group `nomon`).
   - Ensure BlueZ is running: `systemctl status bluetooth`.
- If BR/EDR appears enabled despite attempts to disable it: some Bluetooth
   controllers / BlueZ builds reject disabling BR/EDR at runtime. In this
   case use an explicit LE GATT client (LightBlue / nRF Connect or the
   platform app) to initiate an LE connection which reliably triggers the
   BlueZ passkey agent.

Advanced debug commands
- Show controller state: `bluetoothctl show`
- List devices / bonds: `bluetoothctl devices` / `bluetoothctl info <MAC>`
- List BlueZ managed objects: `busctl --user call org.bluez / org.freedesktop.DBus.ObjectManager GetManagedObjects` (advanced)

Notes
- `nomopractic` will attempt to create `/var/lib/nomon/pairing_secret` with mode `0640` if the file is missing so the daemon can run standalone for local testing. Production setups should still create the directory with the correct owner/group (root:nomon) via tmpfiles or package install.
- For lab automation prefer a dedicated test harness that runs on a managed BLE gateway and triggers pairing via an LE GATT connection.
