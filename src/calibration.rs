// Calibration store — runtime-adjustable motor, servo, and grayscale sensor
// calibration values, separate from the static Config.
//
// Values are persisted to a TOML file (`calibration_path` in Config) and loaded
// at daemon startup.  Changes take effect immediately without a daemon restart.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Per-channel calibration types
// ---------------------------------------------------------------------------

/// Runtime calibration for a single DC motor channel.
///
/// `speed_scale` multiplies `speed_pct` before the PWM write; `deadband_pct`
/// is the minimum magnitude below which the motor stays stopped; `reversed`
/// provides a runtime direction flip independent of `MotorConfig.reversed`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MotorCalibration {
    /// Multiplier on `speed_pct` before PWM write. Range: 0.5–2.0.
    pub speed_scale: f64,
    /// Minimum `speed_pct` magnitude below which motor stays stopped. Range: 0.0–20.0.
    /// The boundary is exclusive: `|speed_pct| == deadband_pct` does not stop the motor.
    pub deadband_pct: f64,
    /// Runtime direction flip (XOR with `MotorConfig.reversed`).
    #[serde(default)]
    pub reversed: bool,
}

impl Default for MotorCalibration {
    fn default() -> Self {
        Self {
            speed_scale: 1.0,
            deadband_pct: 0.0,
            reversed: false,
        }
    }
}

/// Raw ADC surface references for one grayscale sensor position.
///
/// `white_raw` is the ADC reading on a white/reflective surface;
/// `black_raw` is the reading on a black/non-reflective surface.
/// Invariant: `white_raw < black_raw`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GrayscaleCalibration {
    /// ADC reading from a white/reflective surface (lower bound). Default: 100.
    pub white_raw: u16,
    /// ADC reading from a black/non-reflective surface (upper bound). Default: 3000.
    pub black_raw: u16,
}

impl Default for GrayscaleCalibration {
    fn default() -> Self {
        Self {
            white_raw: 100,
            black_raw: 3000,
        }
    }
}

/// Trim offset for a named servo channel.
///
/// `trim_us` is added to the computed pulse width before the 500–2500 µs clamp.
/// Range: −500–+500 µs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ServoCalibration {
    /// Signed trim in microseconds. Added to computed pulse before clamping.
    pub trim_us: i16,
}

// ---------------------------------------------------------------------------
// CalibrationStore
// ---------------------------------------------------------------------------

/// Live-mutable calibration store held behind `Arc<tokio::sync::Mutex<_>>` in
/// the IPC handler.
///
/// Loaded from `calibration.toml` at startup (file absence is not an error)
/// and written back via `save_calibration`.  All fields take effect immediately
/// when written.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CalibrationStore {
    /// Per-motor calibration entries. Length matches `config.motors`.
    pub motors: Vec<MotorCalibration>,
    /// Fixed [left, center, right] grayscale calibration entries.
    pub grayscale: [GrayscaleCalibration; 3],
    /// Per-servo trim offsets keyed by logical name.
    pub servos: HashMap<String, ServoCalibration>,
}

impl CalibrationStore {
    /// Build a default store for `n_motors` motor channels.
    ///
    /// Creates `n_motors` default `MotorCalibration` entries and pre-populates
    /// the three well-known servo keys (`"steering"`, `"camera_pan"`,
    /// `"camera_tilt"`).
    pub fn default_for(n_motors: usize) -> Self {
        let motors = vec![MotorCalibration::default(); n_motors];
        let grayscale = [
            GrayscaleCalibration::default(),
            GrayscaleCalibration::default(),
            GrayscaleCalibration::default(),
        ];
        let mut servos = HashMap::new();
        servos.insert("steering".to_string(), ServoCalibration::default());
        servos.insert("camera_pan".to_string(), ServoCalibration::default());
        servos.insert("camera_tilt".to_string(), ServoCalibration::default());
        Self {
            motors,
            grayscale,
            servos,
        }
    }

    /// Load calibration from a TOML file, falling back to defaults on any error.
    ///
    /// File absence is logged at DEBUG and is not treated as an error.
    /// Parse errors are logged at WARN. In both cases the caller receives
    /// `default_for(n_motors)`.
    pub fn load_or_default(path: &Path, n_motors: usize) -> Self {
        match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!(
                    path = %path.display(),
                    "calibration file not found, using defaults"
                );
                Self::default_for(n_motors)
            }
            Err(e) => {
                warn!(
                    error = %e,
                    path = %path.display(),
                    "failed to read calibration file, using defaults"
                );
                Self::default_for(n_motors)
            }
            Ok(contents) => match toml::from_str::<CalibrationStore>(&contents) {
                Ok(store) => store.ensure_compat(n_motors),
                Err(e) => {
                    warn!(
                        error = %e,
                        path = %path.display(),
                        "failed to parse calibration file, using defaults"
                    );
                    Self::default_for(n_motors)
                }
            },
        }
    }

    /// Serialize this store to TOML and write to `path`.
    ///
    /// Creates parent directories if they do not exist.
    pub fn save(&self, path: &Path) -> Result<(), std::io::Error> {
        let contents = toml::to_string(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)
    }

    // -----------------------------------------------------------------------
    // Validation helpers
    // -----------------------------------------------------------------------

    /// Return `true` if `speed_scale` is within [0.5, 2.0].
    pub fn valid_speed_scale(speed_scale: f64) -> bool {
        (0.5..=2.0).contains(&speed_scale)
    }

    /// Return `true` if `deadband_pct` is within [0.0, 20.0].
    pub fn valid_deadband_pct(deadband_pct: f64) -> bool {
        (0.0..=20.0).contains(&deadband_pct)
    }

    /// Return `true` if `|trim_us|` ≤ 500.
    pub fn valid_trim_us(trim_us: i16) -> bool {
        trim_us.abs() <= 500
    }

    /// Return `true` if `white_raw < black_raw`.
    pub fn valid_grayscale(white_raw: u16, black_raw: u16) -> bool {
        white_raw < black_raw
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Ensure the store is compatible with the current config:
    /// - pads or truncates the `motors` Vec to `n_motors` entries
    /// - inserts any missing well-known servo keys
    /// - resets any grayscale entry that violates `white_raw < black_raw`
    fn ensure_compat(mut self, n_motors: usize) -> Self {
        while self.motors.len() < n_motors {
            self.motors.push(MotorCalibration::default());
        }
        self.motors.truncate(n_motors);
        for key in ["steering", "camera_pan", "camera_tilt"] {
            self.servos.entry(key.to_string()).or_default();
        }
        // Validate grayscale invariant: white_raw < black_raw.
        for (i, entry) in self.grayscale.iter_mut().enumerate() {
            if entry.white_raw >= entry.black_raw {
                warn!(
                    index = i,
                    white_raw = entry.white_raw,
                    black_raw = entry.black_raw,
                    "invalid grayscale calibration (white_raw >= black_raw); resetting to defaults"
                );
                *entry = GrayscaleCalibration::default();
            }
        }
        self
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_for_two_motors_has_correct_field_counts() {
        let store = CalibrationStore::default_for(2);
        assert_eq!(store.motors.len(), 2);
        assert_eq!(store.grayscale.len(), 3);
        assert!(store.servos.contains_key("steering"));
        assert!(store.servos.contains_key("camera_pan"));
        assert!(store.servos.contains_key("camera_tilt"));
    }

    #[test]
    fn default_motor_calibration_values_are_correct() {
        let store = CalibrationStore::default_for(2);
        for motor in &store.motors {
            assert_eq!(motor.speed_scale, 1.0);
            assert_eq!(motor.deadband_pct, 0.0);
            assert!(!motor.reversed);
        }
    }

    #[test]
    fn default_grayscale_calibration_values_are_correct() {
        let store = CalibrationStore::default_for(2);
        for gs in &store.grayscale {
            assert_eq!(gs.white_raw, 100);
            assert_eq!(gs.black_raw, 3000);
        }
    }

    #[test]
    fn default_servo_trim_is_zero() {
        let store = CalibrationStore::default_for(2);
        for (_, s) in &store.servos {
            assert_eq!(s.trim_us, 0);
        }
    }

    #[test]
    fn load_or_default_falls_back_on_missing_file() {
        let store = CalibrationStore::load_or_default(
            std::path::Path::new("/tmp/this_file_should_not_exist_abc123.toml"),
            2,
        );
        // Should silently fall back — no panic.
        assert_eq!(store.motors.len(), 2);
        assert_eq!(store.motors[0].speed_scale, 1.0);
    }

    #[test]
    fn load_or_default_round_trips_via_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("calibration.toml");

        let mut original = CalibrationStore::default_for(2);
        original.motors[0].speed_scale = 1.4;
        original.motors[0].deadband_pct = 3.0;
        original.motors[1].reversed = true;
        original.grayscale[0].white_raw = 150;
        original.grayscale[1].black_raw = 2800;
        original.servos.get_mut("steering").unwrap().trim_us = -30;

        original.save(&path).expect("save must succeed");
        let reloaded = CalibrationStore::load_or_default(&path, 2);

        assert_eq!(original, reloaded);
    }

    #[test]
    fn validation_rejects_speed_scale_out_of_range() {
        assert!(!CalibrationStore::valid_speed_scale(0.1));
        assert!(!CalibrationStore::valid_speed_scale(2.1));
        assert!(CalibrationStore::valid_speed_scale(0.5));
        assert!(CalibrationStore::valid_speed_scale(2.0));
        assert!(CalibrationStore::valid_speed_scale(1.0));
    }

    #[test]
    fn validation_rejects_deadband_pct_out_of_range() {
        assert!(!CalibrationStore::valid_deadband_pct(-1.0));
        assert!(!CalibrationStore::valid_deadband_pct(20.1));
        assert!(CalibrationStore::valid_deadband_pct(0.0));
        assert!(CalibrationStore::valid_deadband_pct(20.0));
    }

    #[test]
    fn validation_rejects_white_raw_ge_black_raw() {
        assert!(!CalibrationStore::valid_grayscale(3000, 100));
        assert!(!CalibrationStore::valid_grayscale(500, 500));
        assert!(CalibrationStore::valid_grayscale(100, 3000));
    }

    #[test]
    fn validation_rejects_trim_us_out_of_range() {
        assert!(!CalibrationStore::valid_trim_us(501));
        assert!(!CalibrationStore::valid_trim_us(-501));
        assert!(CalibrationStore::valid_trim_us(500));
        assert!(CalibrationStore::valid_trim_us(-500));
        assert!(CalibrationStore::valid_trim_us(0));
    }

    #[test]
    fn partial_motor_update_preserves_unchanged_fields() {
        let mut store = CalibrationStore::default_for(2);
        store.motors[0].speed_scale = 1.5;
        store.motors[0].deadband_pct = 5.0;
        store.motors[0].reversed = true;

        // Simulate a partial update — only change speed_scale.
        store.motors[0].speed_scale = 1.2;

        assert_eq!(store.motors[0].speed_scale, 1.2);
        // Other fields unchanged.
        assert_eq!(store.motors[0].deadband_pct, 5.0);
        assert!(store.motors[0].reversed);
        // Motor 1 untouched.
        assert_eq!(store.motors[1].speed_scale, 1.0);
    }

    #[test]
    fn reset_to_defaults_clears_modified_store() {
        let mut store = CalibrationStore::default_for(2);
        store.motors[0].speed_scale = 1.8;
        store.motors[1].deadband_pct = 10.0;
        store.grayscale[0].white_raw = 200;
        store.servos.get_mut("steering").unwrap().trim_us = 100;

        // Reset.
        store = CalibrationStore::default_for(2);

        assert_eq!(store.motors[0].speed_scale, 1.0);
        assert_eq!(store.motors[1].deadband_pct, 0.0);
        assert_eq!(store.grayscale[0].white_raw, 100);
        assert_eq!(store.servos["steering"].trim_us, 0);
    }

    #[test]
    fn load_or_default_falls_back_on_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("calibration.toml");
        std::fs::write(&path, b"not valid toml {{{{").unwrap();

        let store = CalibrationStore::load_or_default(&path, 2);
        // Should fall back to defaults — no panic.
        assert_eq!(store.motors.len(), 2);
        assert_eq!(store.motors[0].speed_scale, 1.0);
    }
}
