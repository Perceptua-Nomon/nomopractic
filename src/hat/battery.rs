// Battery voltage via ADC channel A4 — scaling: battery_v = raw_adc × 3.

use crate::hat::adc::read_adc;
use crate::hat::i2c::{Hat, HatError};

/// ADC channel connected to the battery voltage divider (A4, index 4).
const BATTERY_CHANNEL: u8 = 4;

/// Scaling factor: the HAT firmware divides the measured voltage by 3 before
/// sending, so multiply back by 3 to recover the battery voltage in volts.
const VOLTAGE_SCALE: f64 = 3.0;

/// Read the current battery voltage in volts.
///
/// Reads ADC channel A4 and applies `voltage_v = raw_adc × 3`.
pub async fn get_battery_voltage(hat: &Hat) -> Result<f64, HatError> {
    let raw = read_adc(hat, BATTERY_CHANNEL).await?;
    Ok(raw as f64 * VOLTAGE_SCALE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hat::i2c::{HatError, I2cBus};

    struct MockI2c {
        response: [u8; 2],
    }

    impl I2cBus for MockI2c {
        fn write_bytes(&mut self, _addr: u8, _data: &[u8]) -> Result<(), HatError> {
            Ok(())
        }

        fn read_bytes(&mut self, _addr: u8, buf: &mut [u8]) -> Result<(), HatError> {
            if buf.len() >= 2 {
                buf[0] = self.response[0];
                buf[1] = self.response[1];
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn voltage_calculation_matches_spec() {
        // raw = 0x0A00 = 2560 → voltage = 2560 × 3.0 = 7680.0
        let hat = Hat::new(
            MockI2c {
                response: [0x0A, 0x00],
            },
            0x14,
        );
        let voltage = get_battery_voltage(&hat).await.unwrap();
        assert_eq!(voltage, 7680.0_f64);
    }

    #[tokio::test]
    async fn zero_raw_gives_zero_voltage() {
        let hat = Hat::new(
            MockI2c {
                response: [0x00, 0x00],
            },
            0x14,
        );
        let voltage = get_battery_voltage(&hat).await.unwrap();
        assert_eq!(voltage, 0.0_f64);
    }

    #[tokio::test]
    async fn max_raw_gives_max_voltage() {
        // raw = 0xFFFF = 65535 → voltage = 65535 × 3.0 = 196605.0
        let hat = Hat::new(
            MockI2c {
                response: [0xFF, 0xFF],
            },
            0x14,
        );
        let voltage = get_battery_voltage(&hat).await.unwrap();
        assert_eq!(voltage, 65535.0_f64 * 3.0);
    }
}
