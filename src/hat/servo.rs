// Servo abstraction — angle ↔ pulse_us conversion, per-channel TTL leases.
//
// Angle mapping: pulse_us = 500 + (angle / 180) × 2000
//   0°   →  500 µs
//   90°  → 1500 µs
//   180° → 2500 µs
//
// TTL lease: daemon idles channel (pulse_us = 0) if not refreshed within ttl_ms.
// Recommended: client refreshes every 200 ms with 500 ms TTL.

use std::collections::HashMap;

use tokio::time::{Duration, Instant};

use crate::hat::i2c::{Hat, HatError};
use crate::hat::pwm;
use crate::hat::pwm::MAX_CHANNEL;

const MIN_PULSE_US: u16 = 500;
const MAX_PULSE_US: u16 = 2500;
const MIN_ANGLE: f64 = 0.0;
const MAX_ANGLE: f64 = 180.0;

/// Convert an angle in degrees to a pulse width in microseconds.
///
/// Formula: `pulse_us = 500 + (angle / 180.0) × 2000`
/// Clamps `angle_deg` to [0, 180] before conversion.
pub fn angle_to_pulse_us(angle_deg: f64) -> u16 {
    let angle_deg = angle_deg.clamp(MIN_ANGLE, MAX_ANGLE);
    (500.0 + (angle_deg / 180.0) * 2000.0).round() as u16
}

/// Set a PWM channel to a specific pulse width in microseconds.
///
/// Validates that `channel` is 0–11 and `pulse_us` is 500–2500 before writing
/// to hardware. Returns `INVALID_PARAMS`-category errors if out of range.
pub async fn set_servo_pulse_us(hat: &Hat, channel: u8, pulse_us: u16) -> Result<(), HatError> {
    if channel > MAX_CHANNEL {
        return Err(HatError::InvalidServoChannel(channel));
    }
    if !(MIN_PULSE_US..=MAX_PULSE_US).contains(&pulse_us) {
        return Err(HatError::InvalidPulse(pulse_us));
    }
    pwm::set_channel_pulse_us(hat, channel, pulse_us).await
}

/// Set a servo to an angle in degrees (0–180), returning the computed pulse_us.
///
/// Validates the channel and angle range, then converts to a pulse width and
/// writes to the PWM controller.
pub async fn set_servo_angle(hat: &Hat, channel: u8, angle_deg: f64) -> Result<u16, HatError> {
    if channel > MAX_CHANNEL {
        return Err(HatError::InvalidServoChannel(channel));
    }
    if !(MIN_ANGLE..=MAX_ANGLE).contains(&angle_deg) {
        return Err(HatError::InvalidAngle(angle_deg));
    }
    let pulse_us = angle_to_pulse_us(angle_deg);
    pwm::set_channel_pulse_us(hat, channel, pulse_us).await?;
    Ok(pulse_us)
}

// ---------------------------------------------------------------------------
// TTL Lease Watchdog
// ---------------------------------------------------------------------------

struct LeaseEntry {
    conn_id: u64,
    expires_at: Instant,
}

/// Shared per-channel TTL lease store for all servo commands.
///
/// Every `set_servo_pulse_us` / `set_servo_angle` registers a lease on the
/// targeted channel. The background watchdog calls `poll_expired` periodically
/// and idles any channels whose lease has elapsed. Client disconnect calls
/// `release_connection` to immediately idle all leased channels for that client.
pub struct LeaseManager {
    leases: tokio::sync::Mutex<HashMap<u8, LeaseEntry>>,
}

impl LeaseManager {
    pub fn new() -> Self {
        Self {
            leases: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Register or refresh the lease for `channel`, owned by `conn_id`, expiring after `ttl_ms`.
    pub async fn set_lease(&self, channel: u8, conn_id: u64, ttl_ms: u64) {
        let expires_at = Instant::now() + Duration::from_millis(ttl_ms);
        self.leases.lock().await.insert(
            channel,
            LeaseEntry {
                conn_id,
                expires_at,
            },
        );
    }

    /// Remove all leases belonging to `conn_id` and return the channel numbers to idle.
    pub async fn release_connection(&self, conn_id: u64) -> Vec<u8> {
        let mut map = self.leases.lock().await;
        let channels: Vec<u8> = map
            .iter()
            .filter(|(_, e)| e.conn_id == conn_id)
            .map(|(&ch, _)| ch)
            .collect();
        for ch in &channels {
            map.remove(ch);
        }
        channels
    }

    /// Remove all expired leases and return the channel numbers to idle.
    pub async fn poll_expired(&self) -> Vec<u8> {
        let now = Instant::now();
        let mut map = self.leases.lock().await;
        let expired: Vec<u8> = map
            .iter()
            .filter(|(_, e)| e.expires_at <= now)
            .map(|(&ch, _)| ch)
            .collect();
        for ch in &expired {
            map.remove(ch);
        }
        expired
    }

    /// Returns `(channel, ttl_remaining_ms, conn_id)` for every lease that
    /// has not yet expired.
    pub async fn get_active_leases(&self) -> Vec<(u8, u64, u64)> {
        let now = Instant::now();
        self.leases
            .lock()
            .await
            .iter()
            .filter_map(|(&ch, entry)| {
                entry
                    .expires_at
                    .checked_duration_since(now)
                    .map(|d| (ch, d.as_millis() as u64, entry.conn_id))
            })
            .collect()
    }
}

impl Default for LeaseManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hat::i2c::{HatError, I2cBus};

    // ------------------------------------------------------------------
    // Angle conversion
    // ------------------------------------------------------------------

    #[test]
    fn angle_to_pulse_us_at_zero_degrees() {
        assert_eq!(angle_to_pulse_us(0.0), 500);
    }

    #[test]
    fn angle_to_pulse_us_at_ninety_degrees() {
        assert_eq!(angle_to_pulse_us(90.0), 1500);
    }

    #[test]
    fn angle_to_pulse_us_at_one_eighty_degrees() {
        assert_eq!(angle_to_pulse_us(180.0), 2500);
    }

    #[test]
    fn angle_to_pulse_us_at_forty_five_degrees() {
        // 500 + (45/180)*2000 = 500 + 500 = 1000
        assert_eq!(angle_to_pulse_us(45.0), 1000);
    }

    #[test]
    fn angle_to_pulse_us_clamps_below_zero() {
        assert_eq!(angle_to_pulse_us(-45.0), 500);
    }

    #[test]
    fn angle_to_pulse_us_clamps_above_180() {
        assert_eq!(angle_to_pulse_us(200.0), 2500);
    }

    // ------------------------------------------------------------------
    // set_servo_pulse_us validation
    // ------------------------------------------------------------------

    struct MockI2c;

    impl I2cBus for MockI2c {
        fn write_bytes(&mut self, _addr: u8, _data: &[u8]) -> Result<(), HatError> {
            Ok(())
        }
        fn read_bytes(&mut self, _addr: u8, _buf: &mut [u8]) -> Result<(), HatError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn set_servo_pulse_us_rejects_invalid_channel() {
        let hat = Hat::new(MockI2c, 0x14);
        let err = set_servo_pulse_us(&hat, 12, 1500).await.unwrap_err();
        assert!(matches!(err, HatError::InvalidServoChannel(12)));
    }

    #[tokio::test]
    async fn set_servo_pulse_us_rejects_pulse_below_min() {
        let hat = Hat::new(MockI2c, 0x14);
        let err = set_servo_pulse_us(&hat, 0, 499).await.unwrap_err();
        assert!(matches!(err, HatError::InvalidPulse(499)));
    }

    #[tokio::test]
    async fn set_servo_pulse_us_rejects_pulse_above_max() {
        let hat = Hat::new(MockI2c, 0x14);
        let err = set_servo_pulse_us(&hat, 0, 2501).await.unwrap_err();
        assert!(matches!(err, HatError::InvalidPulse(2501)));
    }

    #[tokio::test]
    async fn set_servo_angle_rejects_angle_out_of_range() {
        let hat = Hat::new(MockI2c, 0x14);
        let err = set_servo_angle(&hat, 0, 181.0).await.unwrap_err();
        assert!(matches!(err, HatError::InvalidAngle(_)));
    }

    #[tokio::test]
    async fn set_servo_angle_returns_computed_pulse_us() {
        let hat = Hat::new(MockI2c, 0x14);
        let pulse = set_servo_angle(&hat, 0, 90.0).await.unwrap();
        assert_eq!(pulse, 1500);
    }

    // ------------------------------------------------------------------
    // LeaseManager
    // ------------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn poll_expired_returns_channel_after_ttl_elapsed() {
        let manager = LeaseManager::new();
        manager.set_lease(5, 1, 100).await;

        // Not yet expired
        assert!(manager.poll_expired().await.is_empty());

        // Advance past TTL
        tokio::time::advance(Duration::from_millis(101)).await;

        let expired = manager.poll_expired().await;
        assert_eq!(expired, vec![5]);
    }

    #[tokio::test(start_paused = true)]
    async fn poll_expired_does_not_return_unexpired_lease() {
        let manager = LeaseManager::new();
        manager.set_lease(2, 1, 500).await;
        tokio::time::advance(Duration::from_millis(200)).await;

        assert!(manager.poll_expired().await.is_empty());
    }

    #[tokio::test]
    async fn release_connection_returns_only_matching_channels() {
        let manager = LeaseManager::new();
        manager.set_lease(0, 42, 5000).await;
        manager.set_lease(1, 99, 5000).await;
        manager.set_lease(2, 42, 5000).await;

        let released = manager.release_connection(42).await;
        let mut released_sorted = released.clone();
        released_sorted.sort_unstable();
        assert_eq!(released_sorted, vec![0, 2]);

        // conn 99's lease should still be present
        assert!(manager.release_connection(99).await.contains(&1));
    }

    #[tokio::test]
    async fn set_lease_refreshes_existing_channel() {
        let manager = LeaseManager::new();
        // Two clients claim the same channel — last write wins
        manager.set_lease(7, 1, 5000).await;
        manager.set_lease(7, 2, 5000).await;

        // conn 1 no longer owns channel 7
        let released = manager.release_connection(1).await;
        assert!(!released.contains(&7));

        // conn 2 does
        let released2 = manager.release_connection(2).await;
        assert!(released2.contains(&7));
    }
}
