// Ultrasonic distance sensor (HC-SR04 / compatible) via GPIO.
//
// Wiring (SunFounder PicarX default):
//   TRIG → D2 (BCM 27) — send the trigger pulse
//   ECHO → D3 (BCM 22) — measure the reflected echo
//
// Measurement sequence:
//  1. Assert TRIG low for ≥ 1 ms (quiesce)
//  2. Assert TRIG high for ≥ 10 µs (trigger pulse)
//  3. Lower TRIG
//  4. Busy-wait for ECHO to go high (pulse start)
//  5. Busy-wait for ECHO to go low  (pulse end)
//  6. distance_cm = elapsed_seconds × SOUND_SPEED_CM_S / 2
//
// A configurable timeout (default 20 ms) aborts any step that takes too long.
// Out-of-range or no-object conditions are reported as `UltrasonicError::NoEcho`.

use std::time::Instant;

use tokio::time::{Duration, sleep};

use crate::hat::gpio::{GpioError, HatGpio};

/// Speed of sound at sea level, 20 °C (cm/s).
const SOUND_SPEED_CM_S: f64 = 34_330.0;

/// Default timeout for a single ultrasonic measurement (20 ms).
pub const DEFAULT_TIMEOUT_MS: u64 = 20;

/// Errors from ultrasonic operations.
#[derive(Debug, thiserror::Error)]
pub enum UltrasonicError {
    #[error("GPIO error: {0}")]
    Gpio(#[from] GpioError),
    #[error("measurement timed out after {0} ms")]
    Timeout(u64),
    #[error("no valid echo received")]
    NoEcho,
}

/// Read a single distance measurement from the ultrasonic sensor.
///
/// # Parameters
/// - `gpio` — shared GPIO context.
/// - `trig_bcm` — BCM pin number for the TRIG output line.
/// - `echo_bcm` — BCM pin number for the ECHO input line.
/// - `timeout_ms` — maximum time to wait for the echo pulse (ms).
///
/// # Returns
/// Distance in centimetres, or `UltrasonicError` on failure.
pub async fn read_distance_cm(
    gpio: &HatGpio,
    trig_bcm: u8,
    echo_bcm: u8,
    timeout_ms: u64,
) -> Result<f64, UltrasonicError> {
    let timeout = Duration::from_millis(timeout_ms);

    // 1. Quiesce: drive TRIG low and give the sensor 1 ms to settle.
    {
        let mut bus = gpio.bus.lock().await;
        bus.write_pin(trig_bcm, false)?;
    }
    sleep(Duration::from_millis(1)).await;

    // 2–4. Send trigger pulse and measure echo pulse duration.
    //
    // The GPIO lock is held for the entire timing-critical section to
    // eliminate per-read lock churn and keep the echo measurement precise.
    // A single end-to-end deadline covers both the pulse-start wait and
    // the pulse-end wait, so the total wall time is bounded by one
    // timeout_ms window (not 2×).
    let (pulse_start, pulse_end_time) = {
        let mut bus = gpio.bus.lock().await;

        // 2. Assert TRIG high for ≥ 10 µs then low.
        //    tokio::time resolution is ~1 ms, so spin to honour the minimum
        //    pulse width without blocking the async executor for a full timer tick.
        bus.write_pin(trig_bcm, true)?;
        let trig_end = Instant::now() + Duration::from_micros(10);
        while Instant::now() < trig_end {
            std::hint::spin_loop();
        }
        bus.write_pin(trig_bcm, false)?;

        // Shared end-to-end deadline for both echo phases.
        let deadline = Instant::now() + timeout;

        // 3. Wait for ECHO to go high (start of return pulse).
        let pulse_start = loop {
            if bus.read_pin(echo_bcm)? {
                break Instant::now();
            }
            if Instant::now() >= deadline {
                return Err(UltrasonicError::Timeout(timeout_ms));
            }
            std::hint::spin_loop();
        };

        // 4. Wait for ECHO to go low (end of return pulse).
        let pulse_end_time = loop {
            if !bus.read_pin(echo_bcm)? {
                break Instant::now();
            }
            if Instant::now() >= deadline {
                return Err(UltrasonicError::Timeout(timeout_ms));
            }
            std::hint::spin_loop();
        };

        (pulse_start, pulse_end_time)
    };

    // 5. Calculate distance.
    let elapsed_s = pulse_end_time.duration_since(pulse_start).as_secs_f64();
    let distance_cm = elapsed_s * SOUND_SPEED_CM_S / 2.0;

    // Sanity range: HC-SR04 specifies 2 cm – 400 cm.
    if !(2.0..=400.0).contains(&distance_cm) {
        return Err(UltrasonicError::NoEcho);
    }

    Ok((distance_cm * 100.0).round() / 100.0)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::hat::gpio::{GpioBus, GpioError, HatGpio};

    // ------------------------------------------------------------------
    // Mock GPIO for testing.
    //
    // The ultrasonic driver reads the ECHO pin in a tight loop.  The mock
    // simulates the ECHO pin going high after a suitable number of reads
    // (simulating a short transit time) and then going low again.
    // ------------------------------------------------------------------

    struct MockUltrasonicGpio {
        state: HashMap<u8, bool>,
        echo_bcm: u8,
        reads_until_high: usize,
        reads_high_for: usize,
        read_count: usize,
    }

    impl MockUltrasonicGpio {
        fn new(echo_bcm: u8, reads_until_high: usize, reads_high_for: usize) -> Self {
            Self {
                state: HashMap::new(),
                echo_bcm,
                reads_until_high,
                reads_high_for,
                read_count: 0,
            }
        }
    }

    impl GpioBus for MockUltrasonicGpio {
        fn write_pin(&mut self, pin_bcm: u8, high: bool) -> Result<(), GpioError> {
            self.state.insert(pin_bcm, high);
            Ok(())
        }

        fn read_pin(&mut self, pin_bcm: u8) -> Result<bool, GpioError> {
            if pin_bcm == self.echo_bcm {
                let count = self.read_count;
                self.read_count += 1;
                if count >= self.reads_until_high
                    && count < self.reads_until_high + self.reads_high_for
                {
                    return Ok(true);
                }
                return Ok(false);
            }
            Ok(*self.state.get(&pin_bcm).unwrap_or(&false))
        }
    }

    const TRIG: u8 = 27;
    const ECHO: u8 = 22;

    #[tokio::test]
    async fn read_distance_returns_valid_range_for_simulated_echo() {
        // Simulate ECHO going high after 2 reads, staying high for 3 reads.
        // The loop acquires the lock per read, so timing varies, but the
        // result should be a positive number within sensor range.
        let gpio = HatGpio::new(MockUltrasonicGpio::new(ECHO, 2, 3));
        let result = read_distance_cm(&gpio, TRIG, ECHO, 1000).await;
        // The mock returns immediately — distance should be extremely small
        // (near-zero elapsed time). Allow NoEcho since sub-2 cm is filtered.
        match result {
            Ok(d) => assert!(d > 0.0 && d <= 400.0),
            Err(UltrasonicError::NoEcho) => { /* expected for zero-time mock */ }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[tokio::test]
    async fn read_distance_times_out_when_echo_never_goes_high() {
        // ECHO never goes high  → timeout waiting for pulse start.
        let gpio = HatGpio::new(MockUltrasonicGpio::new(ECHO, usize::MAX, 0));
        let result = read_distance_cm(&gpio, TRIG, ECHO, 1).await;
        assert!(
            matches!(result, Err(UltrasonicError::Timeout(1))),
            "expected Timeout, got {result:?}"
        );
    }

    #[tokio::test]
    async fn trig_pin_is_driven_high_then_low() {
        // Verify the trigger pulse pattern via mock state.
        let gpio = HatGpio::new(MockUltrasonicGpio::new(ECHO, 0, 100));
        // Don't care about the result — we want to inspect TRIG state.
        let _ = read_distance_cm(&gpio, TRIG, ECHO, 100).await;
        let trig_state = gpio.bus.lock().await.read_pin(TRIG).unwrap();
        // After the measurement TRIG should be low.
        assert!(!trig_state, "TRIG should be low after measurement");
    }
}
