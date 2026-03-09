// Robot HAT V4 PWM register protocol — prescaler calculation, channel writes.
//
// Constants from SunFounder register map:
//   REG_CHN  = 0x20  (PWM channel base)
//   REG_PSC  = 0x40  (prescaler group 1)
//   REG_ARR  = 0x44  (auto-reload group 1)
//   REG_PSC2 = 0x50  (prescaler group 2)
//   REG_ARR2 = 0x54  (auto-reload group 2)
//   CLOCK_HZ = 72 MHz
//   PERIOD   = 4095
