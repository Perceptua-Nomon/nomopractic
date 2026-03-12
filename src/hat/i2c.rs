// Low-level I2C helpers — read/write registers on the HAT (bus 1, address 0x14).

use std::sync::atomic::AtomicU32;

use thiserror::Error;

/// Errors from HAT hardware operations.
#[derive(Debug, Error)]
pub enum HatError {
    #[error("I2C error: {0}")]
    I2c(String),
    #[error("invalid ADC channel {0}: must be 0–7")]
    InvalidChannel(u8),
    #[error("invalid servo channel {0}: must be 0–11")]
    InvalidServoChannel(u8),
    #[error("invalid motor channel {0}: must be 12–15")]
    InvalidMotorChannel(u8),
    #[error("invalid pulse width {0} µs: must be 500–2500")]
    InvalidPulse(u16),
    #[error("invalid angle {0}°: must be 0.0–180.0")]
    InvalidAngle(f64),
    #[error("invalid parameter: {0}")]
    InvalidParam(String),
}

/// Abstraction over raw I2C bus operations, enabling mock injection in tests.
pub trait I2cBus: Send {
    /// Write bytes to the device at `addr`.
    fn write_bytes(&mut self, addr: u8, data: &[u8]) -> Result<(), HatError>;
    /// Read bytes from the device at `addr`.
    fn read_bytes(&mut self, addr: u8, buf: &mut [u8]) -> Result<(), HatError>;
}

/// Write `reg` followed by `data` to the device at `addr`.
pub fn write_register(
    bus: &mut dyn I2cBus,
    addr: u8,
    reg: u8,
    data: &[u8],
) -> Result<(), HatError> {
    let mut buf = Vec::with_capacity(1 + data.len());
    buf.push(reg);
    buf.extend_from_slice(data);
    bus.write_bytes(addr, &buf)
}

/// Write `reg` to `addr`, then read bytes into `buf` (combined write-then-read).
pub fn read_register(
    bus: &mut dyn I2cBus,
    addr: u8,
    reg: u8,
    buf: &mut [u8],
) -> Result<(), HatError> {
    bus.write_bytes(addr, &[reg])?;
    bus.read_bytes(addr, buf)
}

/// rppal-backed I2C bus implementation (target hardware only).
pub struct RppalI2c {
    inner: rppal::i2c::I2c,
}

impl RppalI2c {
    /// Open the I2C bus at the given bus number (typically 1).
    pub fn open(bus: u8) -> Result<Self, HatError> {
        rppal::i2c::I2c::with_bus(bus)
            .map(|inner| Self { inner })
            .map_err(|e| HatError::I2c(e.to_string()))
    }
}

impl I2cBus for RppalI2c {
    fn write_bytes(&mut self, addr: u8, data: &[u8]) -> Result<(), HatError> {
        self.inner
            .set_slave_address(addr as u16)
            .map_err(|e| HatError::I2c(e.to_string()))?;
        self.inner
            .write(data)
            .map_err(|e| HatError::I2c(e.to_string()))?;
        Ok(())
    }

    fn read_bytes(&mut self, addr: u8, buf: &mut [u8]) -> Result<(), HatError> {
        self.inner
            .set_slave_address(addr as u16)
            .map_err(|e| HatError::I2c(e.to_string()))?;
        self.inner
            .read(buf)
            .map_err(|e| HatError::I2c(e.to_string()))?;
        Ok(())
    }
}

/// Thread-safe HAT context holding the I2C bus behind an async mutex.
pub struct Hat {
    pub bus: tokio::sync::Mutex<Box<dyn I2cBus>>,
    pub address: u8,
    /// Period in microseconds corresponding to the frequency set by `init_pwm`.
    /// Defaults to 20 000 µs (50 Hz). Updated atomically by `init_pwm`.
    pub pwm_period_us: AtomicU32,
}

impl Hat {
    /// Create a new Hat with the given I2C bus implementation and HAT address.
    pub fn new(bus: impl I2cBus + 'static, address: u8) -> Self {
        Self {
            bus: tokio::sync::Mutex::new(Box::new(bus)),
            address,
            pwm_period_us: AtomicU32::new(20_000), // 50 Hz default
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockI2c {
        write_log: Vec<(u8, Vec<u8>)>,
        read_response: Vec<u8>,
    }

    impl MockI2c {
        fn new(read_response: Vec<u8>) -> Self {
            Self {
                write_log: Vec::new(),
                read_response,
            }
        }
    }

    impl I2cBus for MockI2c {
        fn write_bytes(&mut self, addr: u8, data: &[u8]) -> Result<(), HatError> {
            self.write_log.push((addr, data.to_vec()));
            Ok(())
        }

        fn read_bytes(&mut self, _addr: u8, buf: &mut [u8]) -> Result<(), HatError> {
            let len = buf.len().min(self.read_response.len());
            buf[..len].copy_from_slice(&self.read_response[..len]);
            Ok(())
        }
    }

    #[test]
    fn write_register_prepends_reg_byte() {
        let mut mock = MockI2c::new(vec![]);
        write_register(&mut mock, 0x14, 0x20, &[0x01, 0x5E]).unwrap();
        assert_eq!(mock.write_log, vec![(0x14, vec![0x20, 0x01, 0x5E])]);
    }

    #[test]
    fn write_register_command_only() {
        let mut mock = MockI2c::new(vec![]);
        write_register(&mut mock, 0x14, 0x14, &[]).unwrap();
        assert_eq!(mock.write_log, vec![(0x14, vec![0x14])]);
    }

    #[test]
    fn read_register_writes_reg_then_reads() {
        let mut mock = MockI2c::new(vec![0x0A, 0xBC]);
        let mut buf = [0u8; 2];
        read_register(&mut mock, 0x14, 0x13, &mut buf).unwrap();
        assert_eq!(mock.write_log, vec![(0x14, vec![0x13])]);
        assert_eq!(buf, [0x0A, 0xBC]);
    }
}
