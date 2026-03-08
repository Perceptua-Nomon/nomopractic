// Servo abstraction — angle ↔ pulse_us conversion, per-channel TTL leases.
//
// Angle mapping: pulse_us = 500 + (angle / 180) × 2000
//   0°   →  500 µs
//   90°  → 1500 µs
//   180° → 2500 µs
//
// TTL lease: daemon idles channel (pulse_us = 0) if not refreshed within ttl_ms.
// Recommended: client refreshes every 200 ms with 500 ms TTL.
