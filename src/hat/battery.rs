// Battery voltage via ADC channel A4.
// Scaling: battery_v = (raw / ADC_MAX) × ADC_VREF × VOLTAGE_DIVIDER

use crate::hat::adc::read_adc;
use crate::hat::i2c::{Hat, HatError};

/// ADC channel connected to the battery voltage divider (A4, index 4).
const BATTERY_CHANNEL: u8 = 4;

/// Full-scale raw count for the 12-bit ADC (0–4095 = 0–3.3 V).
const ADC_MAX: f64 = 4095.0;

/// ADC reference voltage (3.3 V rail on the HAT).
const ADC_VREF: f64 = 3.3;

/// Voltage divider factor on the battery rail (3:1 resistor network).
const VOLTAGE_DIVIDER: f64 = 3.0;

/// Read the current battery voltage in volts.
///
/// Reads ADC channel A4 and applies:
///   `battery_v = (raw / 4095) × 3.3 × 3.0`
pub async fn get_battery_voltage(hat: &Hat) -> Result<f64, HatError> {
    let raw = read_adc(hat, BATTERY_CHANNEL).await?;
    Ok(raw as f64 / ADC_MAX * ADC_VREF * VOLTAGE_DIVIDER)
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
        // raw = 0x0A00 = 2560 → voltage = 2560 / 4095 × 3.3 × 3.0 ≈ 6.190 V
        let hat = Hat::new(
            MockI2c {
                response: [0x0A, 0x00],
            },
            0x14,
        );
        let voltage = get_battery_voltage(&hat).await.unwrap();
        let expected = 2560.0_f64 / 4095.0_f64 * 3.3_f64 * 3.0_f64;
        assert_eq!(voltage, expected);
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
        // raw = 0x0FFF = 4095 (12-bit max) → voltage = 3.3 × 3.0 = 9.9 V
        let hat = Hat::new(
            MockI2c {
                response: [0x0F, 0xFF],
            },
            0x14,
        );
        let voltage = get_battery_voltage(&hat).await.unwrap();
        let expected = 4095.0_f64 / 4095.0_f64 * 3.3_f64 * 3.0_f64;
        assert_eq!(voltage, expected);
    }
}
