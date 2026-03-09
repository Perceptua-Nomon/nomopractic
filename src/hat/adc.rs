// ADC read — write channel command byte, read 2-byte result.

use tokio::time::{Duration, sleep};

use crate::hat::i2c::{Hat, HatError, write_register};

/// Base command byte; add channel index (0–7) to select the channel.
const ADC_CMD_BASE: u8 = 0x10;
/// Delay between write and read, per hardware specification.
const ADC_DELAY_MS: u64 = 10;
const ADC_MAX_CHANNEL: u8 = 7;

/// Read a raw ADC value from the given channel (A0–A7).
///
/// Sends the command byte `0x10 + channel`, waits ~10 ms, then reads the
/// 2-byte big-endian result from the HAT.
pub async fn read_adc(hat: &Hat, channel: u8) -> Result<u16, HatError> {
    if channel > ADC_MAX_CHANNEL {
        return Err(HatError::InvalidChannel(channel));
    }

    let cmd = ADC_CMD_BASE + channel;

    {
        let mut bus = hat.bus.lock().await;
        write_register(&mut **bus, hat.address, cmd, &[])?;
    }

    sleep(Duration::from_millis(ADC_DELAY_MS)).await;

    let mut raw = [0u8; 2];
    {
        let mut bus = hat.bus.lock().await;
        bus.read_bytes(hat.address, &mut raw)?;
    }

    Ok(u16::from_be_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hat::i2c::{HatError, I2cBus};

    struct MockI2c {
        adc_response: [u8; 2],
        last_write: Option<Vec<u8>>,
    }

    impl MockI2c {
        fn new(hi: u8, lo: u8) -> Self {
            Self {
                adc_response: [hi, lo],
                last_write: None,
            }
        }
    }

    impl I2cBus for MockI2c {
        fn write_bytes(&mut self, _addr: u8, data: &[u8]) -> Result<(), HatError> {
            self.last_write = Some(data.to_vec());
            Ok(())
        }

        fn read_bytes(&mut self, _addr: u8, buf: &mut [u8]) -> Result<(), HatError> {
            if buf.len() >= 2 {
                buf[0] = self.adc_response[0];
                buf[1] = self.adc_response[1];
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn read_adc_returns_big_endian_u16() {
        // 0x0BB8 = 3000
        let hat = Hat::new(MockI2c::new(0x0B, 0xB8), 0x14);
        let val = read_adc(&hat, 4).await.unwrap();
        assert_eq!(val, 3000u16);
    }

    #[tokio::test]
    async fn read_adc_rejects_channel_above_7() {
        let hat = Hat::new(MockI2c::new(0, 0), 0x14);
        let err = read_adc(&hat, 8).await.unwrap_err();
        assert!(matches!(err, HatError::InvalidChannel(8)));
    }

    #[tokio::test]
    async fn read_adc_accepts_boundary_channel_0() {
        let hat = Hat::new(MockI2c::new(0x00, 0x01), 0x14);
        let val = read_adc(&hat, 0).await.unwrap();
        assert_eq!(val, 1u16);
    }

    #[tokio::test]
    async fn read_adc_accepts_boundary_channel_7() {
        let hat = Hat::new(MockI2c::new(0xFF, 0xFF), 0x14);
        let val = read_adc(&hat, 7).await.unwrap();
        assert_eq!(val, u16::MAX);
    }
}
