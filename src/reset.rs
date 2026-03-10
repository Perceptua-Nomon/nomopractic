// MCU reset procedure — assert BCM5 low for ≥ 10 ms, then release high.

use tokio::time::{Duration, sleep};

use crate::hat::gpio::{GpioError, GpioPin, HatGpio};

/// Minimum hold time in milliseconds for the MCU reset pulse.
pub const RESET_HOLD_MS: u64 = 10;

/// Result of a successful MCU reset operation.
pub struct ResetResult {
    pub reset_ms: u64,
}

/// Perform an MCU reset: drive MCURST (BCM5) low for `RESET_HOLD_MS` ms, then high.
///
/// The GPIO mutex is released during the sleep so other operations are not blocked
/// during the hold period.
pub async fn reset_mcu(gpio: &HatGpio) -> Result<ResetResult, GpioError> {
    let bcm = GpioPin::McuRst.bcm();
    gpio.bus.lock().await.write_pin(bcm, false)?;
    sleep(Duration::from_millis(RESET_HOLD_MS)).await;
    gpio.bus.lock().await.write_pin(bcm, true)?;
    Ok(ResetResult {
        reset_ms: RESET_HOLD_MS,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::hat::gpio::{GpioBus, GpioPin, HatGpio};

    struct RecordingGpio {
        log: Arc<Mutex<Vec<(u8, bool)>>>,
    }

    impl GpioBus for RecordingGpio {
        fn write_pin(&mut self, pin_bcm: u8, high: bool) -> Result<(), GpioError> {
            self.log.lock().unwrap().push((pin_bcm, high));
            Ok(())
        }

        fn read_pin(&mut self, _pin_bcm: u8) -> Result<bool, GpioError> {
            Ok(false)
        }
    }

    #[tokio::test]
    async fn reset_mcu_drives_low_then_high() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let gpio = HatGpio::new(RecordingGpio {
            log: Arc::clone(&log),
        });
        let result = reset_mcu(&gpio).await.unwrap();
        assert_eq!(result.reset_ms, RESET_HOLD_MS);
        let writes = log.lock().unwrap().clone();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0], (GpioPin::McuRst.bcm(), false)); // assert low
        assert_eq!(writes[1], (GpioPin::McuRst.bcm(), true)); // release high
    }
}
