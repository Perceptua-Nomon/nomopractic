# nomopractic — Hardware Reference

Quick reference for the SunFounder Robot HAT V4 register map as discovered on
the Raspberry Pi Zero 2W. Source: `nomothetic/docs/microcontroller_setup.md`.

## I2C

| Parameter | Value |
|-----------|-------|
| Bus | 1 |
| HAT address | `0x14` |
| Scan command | `sudo i2cdetect -y 1` |

## PWM Registers

| Constant | Address | Purpose |
|----------|---------|---------|
| `REG_CHN` | `0x20` | PWM channel base register |
| `REG_PSC` | `0x40` | Prescaler (group 1, channels 0–7) |
| `REG_ARR` | `0x44` | Auto-reload / period (group 1) |
| `REG_PSC2` | `0x50` | Prescaler (group 2, channels 8–11) |
| `REG_ARR2` | `0x54` | Auto-reload (group 2) |

**Channel register**: `REG_CHN + channel × 4` (2 bytes: high, low)

**Clock**: 72 MHz
**Period**: 4095 (12-bit PWM)
**Servo frequency**: 50 Hz

### Prescaler Calculation

```
prescaler = CLOCK_HZ / (SERVO_FREQ × PERIOD) - 1
          = 72_000_000 / (50 × 4095) - 1
          ≈ 350
```

### Servo Pulse Mapping

| Angle | Pulse (µs) | Formula |
|-------|-----------|---------|
| 0° | 500 | `pulse = 500 + (angle / 180) × 2000` |
| 90° | 1500 | |
| 180° | 2500 | |

**PWM duty value**: `duty = pulse_us / (1_000_000 / SERVO_FREQ) × PERIOD`

## ADC

| Parameter | Value |
|-----------|-------|
| Battery channel | A4 |
| Command byte | `0x13` |
| Read size | 2 bytes (big-endian) |
| Scaling | `voltage = raw × 3` |

### ADC Protocol

1. Write command byte (`0x10 + channel`) to HAT address
2. Short delay (~10 ms)
3. Read 2 bytes from HAT address → big-endian u16

## GPIO Pins

| HAT Name | BCM Pin | Direction | Purpose |
|----------|---------|-----------|---------|
| `D4` | 23 | Output | General purpose |
| `D5` | 24 | Output | General purpose |
| `MCURST` | 5 | Output | MCU reset (active low) |
| `SW` | 19 | Input | Switch / button |
| `LED` | 26 | Output | Status LED |

### MCU Reset Procedure

1. Set BCM5 to output mode
2. Drive LOW
3. Hold for ≥ 10 ms
4. Drive HIGH (release)
5. Wait for MCU to reinitialize (~100 ms recommended)

## I2C Addresses to Avoid

| Address | Device | Notes |
|---------|--------|-------|
| `0x36` | OV5647 camera | Buses 10/11 (muxed) — do not access |
