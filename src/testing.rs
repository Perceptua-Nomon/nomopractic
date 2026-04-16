// Shared test mocks for I2C, GPIO, and ALSA used across unit and integration tests.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::hat::audio::{AlsaControl, AudioError};
use crate::hat::gpio::{GpioBus, GpioError};
use crate::hat::i2c::{HatError, I2cBus};

/// Configurable I2C mock that returns a fixed 2-byte response for every read.
pub struct MockI2c {
    pub response: [u8; 2],
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

/// GPIO mock with `HashMap` state tracking pin levels.
pub struct MockGpio {
    pub state: HashMap<u8, bool>,
}

impl MockGpio {
    pub fn new() -> Self {
        Self {
            state: HashMap::new(),
        }
    }
}

impl Default for MockGpio {
    fn default() -> Self {
        Self::new()
    }
}

impl GpioBus for MockGpio {
    fn write_pin(&mut self, pin_bcm: u8, high: bool) -> Result<(), GpioError> {
        self.state.insert(pin_bcm, high);
        Ok(())
    }

    fn read_pin(&mut self, pin_bcm: u8) -> Result<bool, GpioError> {
        Ok(self.state.get(&pin_bcm).copied().unwrap_or(false))
    }
}

/// ALSA mock with `AtomicU8` volume/gain and an optional failure flag.
pub struct MockAlsaControl {
    pub volume: AtomicU8,
    pub mic_gain: AtomicU8,
    pub fail: bool,
}

impl MockAlsaControl {
    pub fn new(volume: u8, mic_gain: u8) -> Self {
        Self {
            volume: AtomicU8::new(volume),
            mic_gain: AtomicU8::new(mic_gain),
            fail: false,
        }
    }

    pub fn failing() -> Self {
        Self {
            volume: AtomicU8::new(0),
            mic_gain: AtomicU8::new(0),
            fail: true,
        }
    }
}

impl AlsaControl for MockAlsaControl {
    fn get_volume_pct(&self) -> Result<u8, AudioError> {
        if self.fail {
            return Err(AudioError::Command("mock amixer error".into()));
        }
        Ok(self.volume.load(Ordering::SeqCst))
    }

    fn set_volume_pct(&self, pct: u8) -> Result<(), AudioError> {
        if self.fail {
            return Err(AudioError::Command("mock amixer error".into()));
        }
        self.volume.store(pct, Ordering::SeqCst);
        Ok(())
    }

    fn get_mic_gain_pct(&self) -> Result<u8, AudioError> {
        if self.fail {
            return Err(AudioError::Command("mock amixer error".into()));
        }
        Ok(self.mic_gain.load(Ordering::SeqCst))
    }

    fn set_mic_gain_pct(&self, pct: u8) -> Result<(), AudioError> {
        if self.fail {
            return Err(AudioError::Command("mock amixer error".into()));
        }
        self.mic_gain.store(pct, Ordering::SeqCst);
        Ok(())
    }
}
