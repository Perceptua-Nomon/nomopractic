// Named GPIO pins for Robot HAT V4.
//
// | HAT Name | BCM | Direction |
// |----------|-----|-----------|
// | D4       |  23 | Output    |
// | D5       |  24 | Output    |
// | MCURST   |   5 | Output    |
// | SW       |  19 | Input     |
// | LED      |  26 | Output    |

use std::collections::HashMap;

use thiserror::Error;
use tokio::sync::Mutex;

/// Errors from GPIO operations.
#[derive(Debug, Error)]
pub enum GpioError {
    #[error("GPIO error: {0}")]
    Gpio(String),
    #[error("pin '{0}' is an input and cannot be written")]
    ReadOnly(&'static str),
}

/// Named GPIO pins on the Robot HAT V4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioPin {
    D4,
    D5,
    McuRst,
    Sw,
    Led,
}

impl GpioPin {
    /// BCM GPIO pin number.
    pub fn bcm(self) -> u8 {
        match self {
            Self::D4 => 23,
            Self::D5 => 24,
            Self::McuRst => 5,
            Self::Sw => 19,
            Self::Led => 26,
        }
    }

    /// HAT label for this pin.
    pub fn name(self) -> &'static str {
        match self {
            Self::D4 => "D4",
            Self::D5 => "D5",
            Self::McuRst => "MCURST",
            Self::Sw => "SW",
            Self::Led => "LED",
        }
    }

    /// Returns `true` if this pin is an output (can be driven high/low).
    pub fn is_output(self) -> bool {
        !matches!(self, Self::Sw)
    }

    /// Look up a pin by its HAT label (case-sensitive).
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "D4" => Some(Self::D4),
            "D5" => Some(Self::D5),
            "MCURST" => Some(Self::McuRst),
            "SW" => Some(Self::Sw),
            "LED" => Some(Self::Led),
            _ => None,
        }
    }
}

/// Abstraction over GPIO bus operations — enables mock injection in tests.
pub trait GpioBus: Send {
    /// Drive a pin high (`true`) or low (`false`).
    fn write_pin(&mut self, pin_bcm: u8, high: bool) -> Result<(), GpioError>;
    /// Read the current logical level of a pin.
    fn read_pin(&mut self, pin_bcm: u8) -> Result<bool, GpioError>;
}

/// rppal-backed GPIO implementation for target hardware.
pub struct RppalGpio {
    gpio: rppal::gpio::Gpio,
    /// Cached output pin handles, keyed by BCM number.
    ///
    /// Retaining the handle keeps the pin in output mode; dropping an
    /// `OutputPin` causes rppal to reset the line back to input (floating),
    /// which would truncate any active drive (e.g. the MCU reset pulse).
    output_pins: HashMap<u8, rppal::gpio::OutputPin>,
}

impl RppalGpio {
    /// Open the Raspberry Pi GPIO controller.
    pub fn open() -> Result<Self, GpioError> {
        rppal::gpio::Gpio::new()
            .map(|gpio| Self {
                gpio,
                output_pins: HashMap::new(),
            })
            .map_err(|e| GpioError::Gpio(e.to_string()))
    }
}

impl GpioBus for RppalGpio {
    fn write_pin(&mut self, pin_bcm: u8, high: bool) -> Result<(), GpioError> {
        // Acquire or reuse a cached output handle so the pin stays in output
        // mode between calls (e.g. across the sleep in reset_mcu).
        if !self.output_pins.contains_key(&pin_bcm) {
            let output = self
                .gpio
                .get(pin_bcm)
                .map_err(|e| GpioError::Gpio(e.to_string()))?
                .into_output();
            self.output_pins.insert(pin_bcm, output);
        }
        let pin = self
            .output_pins
            .get_mut(&pin_bcm)
            .expect("pin must exist in output_pins after insert");
        if high {
            pin.set_high();
        } else {
            pin.set_low();
        }
        Ok(())
    }

    fn read_pin(&mut self, pin_bcm: u8) -> Result<bool, GpioError> {
        // If the pin is already held as an output, read its current level
        // from the cached handle rather than acquiring a conflicting input.
        if let Some(pin) = self.output_pins.get(&pin_bcm) {
            return Ok(pin.is_set_high());
        }
        let pin = self
            .gpio
            .get(pin_bcm)
            .map_err(|e| GpioError::Gpio(e.to_string()))?
            .into_input();
        Ok(pin.is_high())
    }
}

/// Thread-safe GPIO context shared between the IPC handler and reset module.
pub struct HatGpio {
    pub bus: Mutex<Box<dyn GpioBus>>,
}

impl HatGpio {
    pub fn new(bus: impl GpioBus + 'static) -> Self {
        Self {
            bus: Mutex::new(Box::new(bus)),
        }
    }
}

/// Write a named output pin high (`true`) or low (`false`).
///
/// Returns `GpioError::ReadOnly` if `pin` is an input-only pin (SW).
pub async fn write_gpio_pin(gpio: &HatGpio, pin: GpioPin, high: bool) -> Result<(), GpioError> {
    if !pin.is_output() {
        return Err(GpioError::ReadOnly(pin.name()));
    }
    gpio.bus.lock().await.write_pin(pin.bcm(), high)
}

/// Read the current level of any named pin.
pub async fn read_gpio_pin(gpio: &HatGpio, pin: GpioPin) -> Result<bool, GpioError> {
    gpio.bus.lock().await.read_pin(pin.bcm())
}

/// Drive a GPIO output pin by raw BCM number.
///
/// Used by the motor driver for config-specified direction pins that may not
/// correspond to a named `GpioPin` variant. No read-only check is performed —
/// callers are responsible for ensuring `bcm` refers to an output-capable pin.
pub async fn write_gpio_bcm(gpio: &HatGpio, bcm: u8, high: bool) -> Result<(), GpioError> {
    gpio.bus.lock().await.write_pin(bcm, high)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct MockGpio {
        state: HashMap<u8, bool>,
    }

    impl MockGpio {
        fn new() -> Self {
            Self {
                state: HashMap::new(),
            }
        }
    }

    impl GpioBus for MockGpio {
        fn write_pin(&mut self, pin_bcm: u8, high: bool) -> Result<(), GpioError> {
            self.state.insert(pin_bcm, high);
            Ok(())
        }

        fn read_pin(&mut self, pin_bcm: u8) -> Result<bool, GpioError> {
            Ok(*self.state.get(&pin_bcm).unwrap_or(&false))
        }
    }

    #[test]
    fn pin_bcm_mappings() {
        assert_eq!(GpioPin::D4.bcm(), 23);
        assert_eq!(GpioPin::D5.bcm(), 24);
        assert_eq!(GpioPin::McuRst.bcm(), 5);
        assert_eq!(GpioPin::Sw.bcm(), 19);
        assert_eq!(GpioPin::Led.bcm(), 26);
    }

    #[test]
    fn pin_direction() {
        assert!(GpioPin::D4.is_output());
        assert!(GpioPin::D5.is_output());
        assert!(GpioPin::McuRst.is_output());
        assert!(!GpioPin::Sw.is_output());
        assert!(GpioPin::Led.is_output());
    }

    #[test]
    fn pin_from_name_round_trip() {
        assert_eq!(GpioPin::from_name("D4"), Some(GpioPin::D4));
        assert_eq!(GpioPin::from_name("D5"), Some(GpioPin::D5));
        assert_eq!(GpioPin::from_name("MCURST"), Some(GpioPin::McuRst));
        assert_eq!(GpioPin::from_name("SW"), Some(GpioPin::Sw));
        assert_eq!(GpioPin::from_name("LED"), Some(GpioPin::Led));
        assert_eq!(GpioPin::from_name("INVALID"), None);
    }

    #[tokio::test]
    async fn write_output_pin_succeeds() {
        let gpio = HatGpio::new(MockGpio::new());
        assert!(write_gpio_pin(&gpio, GpioPin::D4, true).await.is_ok());
        assert!(write_gpio_pin(&gpio, GpioPin::Led, false).await.is_ok());
    }

    #[tokio::test]
    async fn write_input_pin_returns_readonly_error() {
        let gpio = HatGpio::new(MockGpio::new());
        let err = write_gpio_pin(&gpio, GpioPin::Sw, true).await.unwrap_err();
        assert!(matches!(err, GpioError::ReadOnly("SW")));
    }

    #[tokio::test]
    async fn read_reflects_written_state() {
        let gpio = HatGpio::new(MockGpio::new());
        write_gpio_pin(&gpio, GpioPin::D4, true).await.unwrap();
        assert!(read_gpio_pin(&gpio, GpioPin::D4).await.unwrap());
        write_gpio_pin(&gpio, GpioPin::D4, false).await.unwrap();
        assert!(!read_gpio_pin(&gpio, GpioPin::D4).await.unwrap());
    }
}
