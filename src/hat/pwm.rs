// Robot HAT V4 PWM register protocol — prescaler calculation, channel writes.
//
// Constants from SunFounder register map:
//   REG_CHN  = 0x20  (PWM channel base)
//   REG_PSC  = 0x40  (prescaler group 1)
//   REG_ARR  = 0x44  (auto-reload group 1)
//   REG_PSC2 = 0x50  (prescaler group 2)
//   REG_ARR2 = 0x54  (auto-reload group 2)
//   CLOCK_HZ = 72 MHz
//   PERIOD   = 4095

use std::sync::atomic::Ordering;

use crate::hat::i2c::{Hat, HatError, write_register};

const REG_CHN: u8 = 0x20;
const REG_PSC: u8 = 0x40;
const REG_ARR: u8 = 0x44;
const REG_PSC2: u8 = 0x50;
const REG_ARR2: u8 = 0x54;

/// Timer clock frequency (Hz).
const CLOCK_HZ: u64 = 72_000_000;
/// PWM period (auto-reload register value) — 12-bit counter (0–4095).
const PERIOD: u16 = 4095;
/// Default servo PWM frequency (Hz).
pub const SERVO_FREQ: u32 = 50;

pub const MAX_CHANNEL: u8 = 11;

/// Initialize the PWM timer at the given frequency (Hz).
///
/// Computes the prescaler as `(CLOCK_HZ / (freq_hz × PERIOD)) − 1` and writes
/// it alongside the auto-reload value to both register groups:
/// - Group 1 (`REG_PSC` / `REG_ARR`): channels 0–7
/// - Group 2 (`REG_PSC2` / `REG_ARR2`): channels 8–11
///
/// Also stores the period in µs (`1_000_000 / freq_hz`) in the `Hat` context
/// so that `set_channel_pulse_us` uses the correct period for duty calculations.
///
/// Must be called once before any `set_channel_pulse_us` calls.
pub async fn init_pwm(hat: &Hat, freq_hz: u32) -> Result<(), HatError> {
    if freq_hz == 0 {
        return Err(HatError::I2c("PWM frequency must be > 0".into()));
    }
    let prescaler = ((CLOCK_HZ / (freq_hz as u64 * PERIOD as u64)).saturating_sub(1)) as u16;
    let arr = PERIOD;
    let psc_bytes = prescaler.to_be_bytes();
    let arr_bytes = arr.to_be_bytes();

    // Store the period in µs so set_channel_pulse_us uses the correct value.
    let period_us = 1_000_000_u32 / freq_hz;
    hat.pwm_period_us.store(period_us, Ordering::Release);

    let mut bus = hat.bus.lock().await;
    // Group 1: channels 0–7
    write_register(&mut **bus, hat.address, REG_PSC, &psc_bytes)?;
    write_register(&mut **bus, hat.address, REG_ARR, &arr_bytes)?;
    // Group 2: channels 8–11
    write_register(&mut **bus, hat.address, REG_PSC2, &psc_bytes)?;
    write_register(&mut **bus, hat.address, REG_ARR2, &arr_bytes)?;
    Ok(())
}

/// Write a pulse width in microseconds to a PWM channel (0–11).
///
/// `pulse_us = 0` disables the channel (used by the watchdog to idle servos).
/// The duty register is `REG_CHN + channel × 4`, written as big-endian u16.
/// The period in µs is read from `hat.pwm_period_us`, which is set by `init_pwm`.
pub async fn set_channel_pulse_us(hat: &Hat, channel: u8, pulse_us: u16) -> Result<(), HatError> {
    if channel > MAX_CHANNEL {
        return Err(HatError::InvalidServoChannel(channel));
    }
    let period_us = hat.pwm_period_us.load(Ordering::Acquire);
    let duty = (pulse_us as u32 * PERIOD as u32 / period_us) as u16;
    let reg = REG_CHN + channel * 4;

    let mut bus = hat.bus.lock().await;
    write_register(&mut **bus, hat.address, reg, &duty.to_be_bytes())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::hat::i2c::{HatError, I2cBus};

    type WriteLog = Arc<Mutex<Vec<(u8, Vec<u8>)>>>;

    /// Observable mock that records (addr, bytes) pairs for each `write_bytes` call.
    struct MockI2c {
        writes: WriteLog,
    }

    impl MockI2c {
        fn new() -> (Self, WriteLog) {
            let log = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    writes: Arc::clone(&log),
                },
                log,
            )
        }
    }

    impl I2cBus for MockI2c {
        fn write_bytes(&mut self, addr: u8, data: &[u8]) -> Result<(), HatError> {
            self.writes.lock().unwrap().push((addr, data.to_vec()));
            Ok(())
        }
        fn read_bytes(&mut self, _addr: u8, _buf: &mut [u8]) -> Result<(), HatError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn init_pwm_writes_prescaler_and_arr_to_both_groups() {
        // prescaler = floor(72_000_000 / (50 * 4095)) - 1 = 351 - 1 = 350 = 0x015E
        // arr       = 4095 = 0x0FFF
        let (mock, log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        init_pwm(&hat, SERVO_FREQ).await.unwrap();

        let writes = log.lock().unwrap();
        assert_eq!(writes.len(), 4);
        // Group 1 prescaler
        assert_eq!(writes[0], (0x14, vec![REG_PSC, 0x01, 0x5E]));
        // Group 1 ARR
        assert_eq!(writes[1], (0x14, vec![REG_ARR, 0x0F, 0xFF]));
        // Group 2 prescaler
        assert_eq!(writes[2], (0x14, vec![REG_PSC2, 0x01, 0x5E]));
        // Group 2 ARR
        assert_eq!(writes[3], (0x14, vec![REG_ARR2, 0x0F, 0xFF]));
    }

    #[tokio::test]
    async fn set_channel_pulse_us_writes_correct_duty_for_midpoint() {
        // pulse_us=1500, duty = 1500 * 4095 / 20000 = 307 = 0x0133
        // channel=0 → reg = REG_CHN + 0*4 = 0x20
        let (mock, log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        set_channel_pulse_us(&hat, 0, 1500).await.unwrap();

        let writes = log.lock().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0], (0x14, vec![0x20, 0x01, 0x33]));
    }

    #[tokio::test]
    async fn set_channel_pulse_us_idle_writes_zero() {
        // pulse_us=0 → duty=0; channel=3 → reg = 0x20 + 3*4 = 0x2C
        let (mock, log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        set_channel_pulse_us(&hat, 3, 0).await.unwrap();

        let writes = log.lock().unwrap();
        assert_eq!(writes[0], (0x14, vec![0x2C, 0x00, 0x00]));
    }

    #[tokio::test]
    async fn set_channel_pulse_us_rejects_invalid_channel() {
        let (mock, _log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        let err = set_channel_pulse_us(&hat, 12, 1500).await.unwrap_err();
        assert!(matches!(err, HatError::InvalidServoChannel(12)));
    }

    #[tokio::test]
    async fn set_channel_pulse_us_uses_correct_register_for_channel_11() {
        // channel=11 → reg = 0x20 + 11*4 = 0x20 + 44 = 0x4C
        // pulse_us=2500 → duty = 2500 * 4095 / 20000 = 511 = 0x01FF
        let (mock, log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        set_channel_pulse_us(&hat, 11, 2500).await.unwrap();

        let writes = log.lock().unwrap();
        assert_eq!(writes[0], (0x14, vec![0x4C, 0x01, 0xFF]));
    }

    #[tokio::test]
    async fn set_channel_pulse_us_uses_period_from_init_pwm() {
        // After init at 100 Hz, period_us = 1_000_000 / 100 = 10_000
        // duty = 1500 * 4095 / 10_000 = 614 = 0x0266
        let (mock, log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        init_pwm(&hat, 100).await.unwrap();

        log.lock().unwrap().clear(); // discard init writes

        set_channel_pulse_us(&hat, 0, 1500).await.unwrap();

        let writes = log.lock().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0], (0x14, vec![0x20, 0x02, 0x66]));
    }
}
