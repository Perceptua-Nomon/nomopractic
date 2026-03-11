// Robot HAT V4 PWM register protocol — prescaler calculation, channel writes.
//
// Timer groups (per SunFounder register map, commit d856cfb):
//   REG_CHN       = 0x20  (channel base; channel N → register 0x20 + N)
//   REG_PSC + ti  = 0x40–0x43  (prescaler for timer index ti = 0–3)
//   REG_ARR + ti  = 0x44–0x47  (auto-reload for timer index ti = 0–3)
//   timer_index   = channel / 4  (channels 0–3 → ti=0, 4–7 → ti=1, …)
//
//   CLOCK_HZ = 72 MHz
//   PERIOD   = 4095

use std::sync::atomic::Ordering;

use crate::hat::i2c::{Hat, HatError, write_register};

/// PWM channel base register.  Channel N uses register `REG_CHN + N`.
const REG_CHN: u8 = 0x20;
/// Prescaler base register for timer groups 0–3 (`REG_PSC + timer_index`).
const REG_PSC: u8 = 0x40;
/// Auto-reload base register for timer groups 0–3 (`REG_ARR + timer_index`).
const REG_ARR: u8 = 0x44;

/// Timer clock frequency (Hz).
const CLOCK_HZ: u64 = 72_000_000;
/// PWM period (auto-reload register value) — 12-bit counter (0–4095).
const PERIOD: u16 = 4095;
/// Default servo PWM frequency (Hz).
pub const SERVO_FREQ: u32 = 50;
/// Maximum servo PWM channel (timers 0–2, channels 0–11).
pub const MAX_CHANNEL: u8 = 11;

/// Default motor PWM frequency (Hz) — TC1508S driver.
pub const MOTOR_FREQ: u32 = 100;
/// First motor PWM channel (timer 3).
pub const MOTOR_MIN_CHANNEL: u8 = 12;
/// Last motor PWM channel (timer 3).
pub const MOTOR_MAX_CHANNEL: u8 = 15;

/// Initialize servo PWM timers 0–2 (channels 0–11) at the given frequency.
///
/// Computes `prescaler = (CLOCK_HZ / (freq_hz × PERIOD)) − 1` and writes it
/// alongside the fixed auto-reload value to each of the three timer groups
/// (`REG_PSC+0..=+2`, `REG_ARR+0..=+2`).
///
/// Also stores `1_000_000 / freq_hz` in `hat.pwm_period_us` so that
/// `set_channel_pulse_us` uses the correct period for duty calculations.
///
/// Must be called once before any `set_channel_pulse_us` calls.
pub async fn init_pwm(hat: &Hat, freq_hz: u32) -> Result<(), HatError> {
    if freq_hz == 0 {
        return Err(HatError::InvalidParam("PWM frequency must be > 0".into()));
    }
    let prescaler = ((CLOCK_HZ / (freq_hz as u64 * PERIOD as u64)).saturating_sub(1)) as u16;
    let arr = PERIOD;
    let psc_bytes = prescaler.to_be_bytes();
    let arr_bytes = arr.to_be_bytes();

    // Store the period in µs so set_channel_pulse_us uses the correct value.
    let period_us = 1_000_000_u32 / freq_hz;
    hat.pwm_period_us.store(period_us, Ordering::Release);

    let mut bus = hat.bus.lock().await;
    // Timers 0–2: channels 0–11 (servo channels).
    for ti in 0u8..3 {
        write_register(&mut **bus, hat.address, REG_PSC + ti, &psc_bytes)?;
        write_register(&mut **bus, hat.address, REG_ARR + ti, &arr_bytes)?;
    }
    Ok(())
}

/// Initialize motor PWM timer 3 (channels 12–15) at the given frequency.
///
/// Motor channels use a separate timer group from the servo channels so they
/// can run at a different frequency (typically 100 Hz vs. 50 Hz for servos).
/// Must be called once before any `set_motor_channel_duty_pct` calls.
pub async fn init_motor_pwm(hat: &Hat, freq_hz: u32) -> Result<(), HatError> {
    if freq_hz == 0 {
        return Err(HatError::InvalidParam(
            "motor PWM frequency must be > 0".into(),
        ));
    }
    let prescaler = ((CLOCK_HZ / (freq_hz as u64 * PERIOD as u64)).saturating_sub(1)) as u16;
    let arr = PERIOD;
    let psc_bytes = prescaler.to_be_bytes();
    let arr_bytes = arr.to_be_bytes();

    let mut bus = hat.bus.lock().await;
    // Timer 3: channels 12–15 (motor channels).
    write_register(&mut **bus, hat.address, REG_PSC + 3, &psc_bytes)?;
    write_register(&mut **bus, hat.address, REG_ARR + 3, &arr_bytes)?;
    Ok(())
}

/// Write a pulse width in microseconds to a servo PWM channel (0–11).
///
/// `pulse_us = 0` disables the channel (used by the watchdog to idle servos).
/// The duty register is `REG_CHN + channel`, written as big-endian u16.
/// The period in µs is read from `hat.pwm_period_us`, which is set by `init_pwm`.
pub async fn set_channel_pulse_us(hat: &Hat, channel: u8, pulse_us: u16) -> Result<(), HatError> {
    if channel > MAX_CHANNEL {
        return Err(HatError::InvalidServoChannel(channel));
    }
    let period_us = hat.pwm_period_us.load(Ordering::Acquire);
    let duty = (pulse_us as u32 * PERIOD as u32 / period_us) as u16;
    let reg = REG_CHN + channel;

    let mut bus = hat.bus.lock().await;
    write_register(&mut **bus, hat.address, reg, &duty.to_be_bytes())
}

/// Write a duty cycle percentage (0.0–100.0) to a motor PWM channel (12–15).
///
/// Motor channels use percentage-based control rather than the pulse-width
/// path used by servos; they bypass `hat.pwm_period_us` entirely.
/// `duty_pct = 0.0` stops the motor (zero torque).
pub async fn set_motor_channel_duty_pct(
    hat: &Hat,
    channel: u8,
    duty_pct: f64,
) -> Result<(), HatError> {
    if !(MOTOR_MIN_CHANNEL..=MOTOR_MAX_CHANNEL).contains(&channel) {
        return Err(HatError::InvalidMotorChannel(channel));
    }
    let duty = (duty_pct.clamp(0.0, 100.0) * PERIOD as f64 / 100.0).round() as u16;
    let reg = REG_CHN + channel;

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

    // ------------------------------------------------------------------
    // init_pwm
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn init_pwm_writes_prescaler_and_arr_to_timers_0_1_2() {
        // prescaler = floor(72_000_000 / (50 * 4095)) - 1 = 351 - 1 = 350 = 0x015E
        // arr       = 4095 = 0x0FFF
        let (mock, log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        init_pwm(&hat, SERVO_FREQ).await.unwrap();

        let writes = log.lock().unwrap();
        assert_eq!(writes.len(), 6);
        // Timer 0 prescaler (0x40) + ARR (0x44)
        assert_eq!(writes[0], (0x14, vec![REG_PSC, 0x01, 0x5E]));
        assert_eq!(writes[1], (0x14, vec![REG_ARR, 0x0F, 0xFF]));
        // Timer 1 prescaler (0x41) + ARR (0x45)
        assert_eq!(writes[2], (0x14, vec![REG_PSC + 1, 0x01, 0x5E]));
        assert_eq!(writes[3], (0x14, vec![REG_ARR + 1, 0x0F, 0xFF]));
        // Timer 2 prescaler (0x42) + ARR (0x46)
        assert_eq!(writes[4], (0x14, vec![REG_PSC + 2, 0x01, 0x5E]));
        assert_eq!(writes[5], (0x14, vec![REG_ARR + 2, 0x0F, 0xFF]));
    }

    // ------------------------------------------------------------------
    // init_motor_pwm
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn init_motor_pwm_writes_prescaler_and_arr_to_timer_3() {
        // prescaler at 100 Hz = floor(72_000_000 / (100 * 4095)) - 1
        //   = floor(175.82) - 1 = 175 - 1 = 174 = 0x00AE
        // arr = 4095 = 0x0FFF
        let (mock, log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        init_motor_pwm(&hat, MOTOR_FREQ).await.unwrap();

        let writes = log.lock().unwrap();
        assert_eq!(writes.len(), 2);
        // Timer 3 prescaler (0x43) + ARR (0x47)
        assert_eq!(writes[0], (0x14, vec![REG_PSC + 3, 0x00, 0xAE]));
        assert_eq!(writes[1], (0x14, vec![REG_ARR + 3, 0x0F, 0xFF]));
    }

    #[tokio::test]
    async fn init_motor_pwm_zero_freq_returns_invalid_param() {
        let (mock, _log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        let err = init_motor_pwm(&hat, 0).await.unwrap_err();
        assert!(matches!(err, HatError::InvalidParam(_)));
    }

    #[tokio::test]
    async fn init_pwm_zero_freq_returns_invalid_param() {
        let (mock, _log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        let err = init_pwm(&hat, 0).await.unwrap_err();
        assert!(matches!(err, HatError::InvalidParam(_)));
    }

    // ------------------------------------------------------------------
    // set_channel_pulse_us
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn set_channel_pulse_us_writes_correct_duty_for_midpoint() {
        // pulse_us=1500, duty = 1500 * 4095 / 20000 = 307 = 0x0133
        // channel=0 → reg = REG_CHN + 0 = 0x20
        let (mock, log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        set_channel_pulse_us(&hat, 0, 1500).await.unwrap();

        let writes = log.lock().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0], (0x14, vec![0x20, 0x01, 0x33]));
    }

    #[tokio::test]
    async fn set_channel_pulse_us_idle_writes_zero() {
        // pulse_us=0 → duty=0; channel=3 → reg = REG_CHN + 3 = 0x23
        let (mock, log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        set_channel_pulse_us(&hat, 3, 0).await.unwrap();

        let writes = log.lock().unwrap();
        assert_eq!(writes[0], (0x14, vec![0x23, 0x00, 0x00]));
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
        // channel=11 → reg = REG_CHN + 11 = 0x2B
        // pulse_us=2500 → duty = 2500 * 4095 / 20000 = 511 = 0x01FF
        let (mock, log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        set_channel_pulse_us(&hat, 11, 2500).await.unwrap();

        let writes = log.lock().unwrap();
        assert_eq!(writes[0], (0x14, vec![0x2B, 0x01, 0xFF]));
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

    // ------------------------------------------------------------------
    // set_motor_channel_duty_pct
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn set_motor_channel_duty_pct_full_forward() {
        // channel=12, duty_pct=100.0 → duty = 4095 = 0x0FFF
        // reg = REG_CHN + 12 = 0x2C
        let (mock, log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        set_motor_channel_duty_pct(&hat, 12, 100.0).await.unwrap();

        let writes = log.lock().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0], (0x14, vec![0x2C, 0x0F, 0xFF]));
    }

    #[tokio::test]
    async fn set_motor_channel_duty_pct_half_speed() {
        // duty_pct=50.0 → duty = (50.0 * 4095 / 100.0).round() = 2048 = 0x0800
        // channel=12 → reg = 0x2C
        let (mock, log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        set_motor_channel_duty_pct(&hat, 12, 50.0).await.unwrap();

        let writes = log.lock().unwrap();
        assert_eq!(writes[0].0, 0x14);
        assert_eq!(writes[0].1[0], 0x2C); // reg
        // duty = round(50.0 * 4095 / 100.0) = round(2047.5) = 2048 = 0x0800
        assert_eq!(writes[0].1[1..], [0x08, 0x00]);
    }

    #[tokio::test]
    async fn set_motor_channel_duty_pct_stop() {
        // duty_pct=0.0 → duty=0; channel=13 → reg = 0x2D
        let (mock, log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        set_motor_channel_duty_pct(&hat, 13, 0.0).await.unwrap();

        let writes = log.lock().unwrap();
        assert_eq!(writes[0], (0x14, vec![0x2D, 0x00, 0x00]));
    }

    #[tokio::test]
    async fn set_motor_channel_duty_pct_rejects_servo_channel() {
        let (mock, _log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        let err = set_motor_channel_duty_pct(&hat, 11, 50.0)
            .await
            .unwrap_err();
        assert!(matches!(err, HatError::InvalidMotorChannel(11)));
    }

    #[tokio::test]
    async fn set_motor_channel_duty_pct_rejects_out_of_range_channel() {
        let (mock, _log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        let err = set_motor_channel_duty_pct(&hat, 16, 50.0)
            .await
            .unwrap_err();
        assert!(matches!(err, HatError::InvalidMotorChannel(16)));
    }

    #[tokio::test]
    async fn set_motor_channel_duty_pct_clamps_over_100() {
        // duty_pct=150.0 → clamped to 100.0 → duty = 4095 = 0x0FFF
        let (mock, log) = MockI2c::new();
        let hat = Hat::new(mock, 0x14);
        set_motor_channel_duty_pct(&hat, 12, 150.0).await.unwrap();

        let writes = log.lock().unwrap();
        assert_eq!(writes[0], (0x14, vec![0x2C, 0x0F, 0xFF]));
    }
}
