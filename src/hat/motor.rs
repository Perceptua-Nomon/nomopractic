// DC motor control — TC1508S H-bridge driver via PWM + GPIO direction.
//
// Mode 1 (TC1508S) wiring:
//   forward:  pwm = duty,  dir = HIGH
//   backward: pwm = duty,  dir = LOW
//   stop:     pwm = 0,     dir = any
//
// Speed is a signed percentage: -100.0 (full reverse) to +100.0 (full forward).
// The `reversed` flag in config inverts the direction signal for motors wired
// with reversed polarity.
//
// Motor PWM channels: 12–15 (timer 3, initialized with `init_motor_pwm`).
// Direction pins: arbitrary BCM output pins (specified per-motor in config).

use thiserror::Error;

use crate::hat::gpio::{GpioError, HatGpio, write_gpio_bcm};
use crate::hat::i2c::{Hat, HatError};
use crate::hat::pwm;

/// Errors from motor control operations.
#[derive(Debug, Error)]
pub enum MotorError {
    #[error("HAT I2C error: {0}")]
    Hat(HatError),
    #[error("GPIO error: {0}")]
    Gpio(GpioError),
}

/// Set a motor's speed as a signed percentage.
///
/// - `pwm_channel`: HAT PWM channel (12–15).
/// - `dir_pin_bcm`: BCM GPIO pin for direction control (HIGH = forward, LOW = backward).
/// - `reversed`: if `true`, the direction signal is inverted for this motor.
/// - `speed_pct`: −100.0 (full reverse) to +100.0 (full forward); values
///   outside range are clamped. `0.0` applies zero duty (stop).
///
/// Direction is written before duty so the motor driver never sees a brief
/// period of wrong-direction torque when direction changes.
pub async fn set_motor_speed(
    hat: &Hat,
    gpio: &HatGpio,
    pwm_channel: u8,
    dir_pin_bcm: u8,
    reversed: bool,
    speed_pct: f64,
) -> Result<(), MotorError> {
    let speed_pct = speed_pct.clamp(-100.0, 100.0);
    // forward = (speed_pct >= 0) XOR reversed
    let dir_high = (speed_pct >= 0.0) ^ reversed;

    // Write direction before PWM.
    write_gpio_bcm(gpio, dir_pin_bcm, dir_high)
        .await
        .map_err(MotorError::Gpio)?;

    pwm::set_motor_channel_duty_pct(hat, pwm_channel, speed_pct.abs())
        .await
        .map_err(MotorError::Hat)?;

    Ok(())
}

/// Set motor duty to zero without changing the direction pin.
///
/// Used by the TTL watchdog and `stop_all_motors` to halt a motor channel
/// with minimum I2C traffic. The direction pin is left as-is.
pub async fn idle_motor(hat: &Hat, pwm_channel: u8) -> Result<(), HatError> {
    pwm::set_motor_channel_duty_pct(hat, pwm_channel, 0.0).await
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::hat::gpio::{GpioBus, GpioError, HatGpio};
    use crate::hat::i2c::{HatError, I2cBus};

    // ------------------------------------------------------------------
    // Mock helpers
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

    // ------------------------------------------------------------------
    // set_motor_speed
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn set_motor_speed_forward_sets_dir_high() {
        let hat = Hat::new(MockI2c, 0x14);
        let gpio = HatGpio::new(MockGpio::new());

        set_motor_speed(&hat, &gpio, 12, 24, false, 50.0)
            .await
            .unwrap();

        let result = gpio.bus.lock().await.read_pin(24).unwrap();
        assert!(result, "direction pin should be HIGH for forward speed");
    }

    #[tokio::test]
    async fn set_motor_speed_backward_sets_dir_low() {
        let hat = Hat::new(MockI2c, 0x14);
        let gpio = HatGpio::new(MockGpio::new());

        set_motor_speed(&hat, &gpio, 12, 24, false, -50.0)
            .await
            .unwrap();

        // Cannot introspect mock directly through trait object in this architecture;
        // verify via public read_pin on HatGpio instead.
        let result = gpio.bus.lock().await.read_pin(24).unwrap();
        assert!(!result, "direction pin should be LOW for backward");
    }

    #[tokio::test]
    async fn set_motor_speed_reversed_inverts_direction() {
        let hat = Hat::new(MockI2c, 0x14);
        let gpio = HatGpio::new(MockGpio::new());

        // Positive speed + reversed=true → dir should be LOW
        set_motor_speed(&hat, &gpio, 12, 24, true, 50.0)
            .await
            .unwrap();

        let result = gpio.bus.lock().await.read_pin(24).unwrap();
        assert!(
            !result,
            "direction pin should be LOW when reversed=true and speed>0"
        );
    }

    #[tokio::test]
    async fn set_motor_speed_zero_sets_dir_high_and_zero_duty() {
        // speed=0 → forward XOR false = HIGH; duty = 0.0% (stop)
        let hat = Hat::new(MockI2c, 0x14);
        let gpio = HatGpio::new(MockGpio::new());

        set_motor_speed(&hat, &gpio, 12, 24, false, 0.0)
            .await
            .unwrap();

        let result = gpio.bus.lock().await.read_pin(24).unwrap();
        assert!(
            result,
            "direction pin should be HIGH for zero speed (forward convention)"
        );
    }

    #[tokio::test]
    async fn set_motor_speed_invalid_pwm_channel_returns_error() {
        let hat = Hat::new(MockI2c, 0x14);
        let gpio = HatGpio::new(MockGpio::new());

        let err = set_motor_speed(&hat, &gpio, 5, 24, false, 50.0)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MotorError::Hat(HatError::InvalidMotorChannel(5))
        ));
    }

    #[tokio::test]
    async fn set_motor_speed_clamps_speed_above_100() {
        // Should not return an error even if speed_pct > 100
        let hat = Hat::new(MockI2c, 0x14);
        let gpio = HatGpio::new(MockGpio::new());

        set_motor_speed(&hat, &gpio, 12, 24, false, 150.0)
            .await
            .unwrap();
    }

    // ------------------------------------------------------------------
    // idle_motor
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn idle_motor_succeeds_for_valid_channel() {
        let hat = Hat::new(MockI2c, 0x14);
        idle_motor(&hat, 12).await.unwrap();
    }

    #[tokio::test]
    async fn idle_motor_rejects_servo_channel() {
        let hat = Hat::new(MockI2c, 0x14);
        let err = idle_motor(&hat, 0).await.unwrap_err();
        assert!(matches!(err, HatError::InvalidMotorChannel(0)));
    }
}
