// ALSA mixer control for output volume (HifiBerry DAC) and input gain
// (USB microphone PCM2902).
//
// Production implementation uses the ALSA `amixer` command-line utility.
// Tests substitute a `MockAlsaControl` that records calls without hitting
// the system mixer.

use std::process::Command;

use thiserror::Error;

/// Errors from ALSA mixer operations.
#[derive(Debug, Error)]
pub enum AudioError {
    #[error("amixer returned an error: {0}")]
    Command(String),
    #[error("failed to parse amixer output: {0}")]
    Parse(String),
    #[error("I/O error invoking amixer: {0}")]
    Io(#[from] std::io::Error),
}

/// Abstraction over ALSA mixer control.
///
/// Sync (blocking) so it can be used from `tokio::task::spawn_blocking`.
/// Test code supplies a `MockAlsaControl`; production code uses
/// `AmixerControl`.
pub trait AlsaControl: Send + Sync {
    /// Return the current output volume as a percentage (0–100).
    fn get_volume_pct(&self) -> Result<u8, AudioError>;
    /// Set the output volume to `pct` percent (0–100).
    fn set_volume_pct(&self, pct: u8) -> Result<(), AudioError>;
    /// Return the current microphone capture gain as a percentage (0–100).
    fn get_mic_gain_pct(&self) -> Result<u8, AudioError>;
    /// Set the microphone capture gain to `pct` percent (0–100).
    fn set_mic_gain_pct(&self, pct: u8) -> Result<(), AudioError>;
}

/// Production `AlsaControl` backed by `amixer`.
///
/// `amixer -c <card_index> sset "<control>" <pct>%` — works on any ALSA
/// system with the `alsa-utils` package installed (standard on Raspberry Pi OS).
#[derive(Debug)]
pub struct AmixerControl {
    /// ALSA card index for audio output (HifiBerry DAC), e.g. 1.
    pub(crate) output_card_index: u8,
    /// ALSA mixer control name for output volume, e.g. `"Digital"`.
    pub(crate) output_control: String,
    /// ALSA card index for audio input (USB mic PCM2902), e.g. 2.
    pub(crate) input_card_index: u8,
    /// ALSA mixer control name for microphone capture gain, e.g. `"Mic Capture"`.
    pub(crate) input_control: String,
}

impl AmixerControl {
    /// List the simple-mixer control names available on `card_index`.
    ///
    /// Runs `amixer -c N scontrols` and returns the raw output trimmed to a
    /// single line summary.  Used to enrich error messages when a configured
    /// control name is not found, so the caller can immediately see what names
    /// are valid on their hardware.
    fn available_controls(card_index: u8) -> String {
        match Command::new("amixer")
            .args(["-c", &card_index.to_string(), "scontrols"])
            .output()
        {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                let names: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
                if names.is_empty() {
                    format!("(no controls found on card {card_index})")
                } else {
                    names.join(", ")
                }
            }
            Err(e) => format!("(could not list controls: {e})"),
        }
    }

    /// Parse the current percentage from `amixer get` output.
    ///
    /// Looks for the first `[N%]` token in the output — produced by lines
    /// like `  Front Left: Playback 84 [84%] ...`.
    fn parse_pct(output: &str) -> Result<u8, AudioError> {
        for line in output.lines() {
            if let Some(start) = line.find('[')
                && let Some(end) = line[start..].find('%')
            {
                let pct_str = line[start + 1..start + end].trim();
                return pct_str.parse::<u8>().map_err(|_| {
                    AudioError::Parse(format!(
                        "could not parse percentage '{pct_str}' from amixer output"
                    ))
                });
            }
        }
        Err(AudioError::Parse(format!(
            "no percentage found in amixer output: {output}"
        )))
    }
}

impl AlsaControl for AmixerControl {
    fn get_volume_pct(&self) -> Result<u8, AudioError> {
        let out = Command::new("amixer")
            .args([
                "-c",
                &self.output_card_index.to_string(),
                "get",
                &self.output_control,
            ])
            .output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            let available = Self::available_controls(self.output_card_index);
            return Err(AudioError::Command(format!(
                "{stderr}Available controls on card {}: {available}",
                self.output_card_index
            )));
        }
        Self::parse_pct(&String::from_utf8_lossy(&out.stdout))
    }

    fn set_volume_pct(&self, pct: u8) -> Result<(), AudioError> {
        let pct_arg = format!("{pct}%");
        let out = Command::new("amixer")
            .args([
                "-c",
                &self.output_card_index.to_string(),
                "sset",
                &self.output_control,
                &pct_arg,
            ])
            .output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            let available = Self::available_controls(self.output_card_index);
            return Err(AudioError::Command(format!(
                "{stderr}Available controls on card {}: {available}",
                self.output_card_index
            )));
        }
        Ok(())
    }

    fn get_mic_gain_pct(&self) -> Result<u8, AudioError> {
        let out = Command::new("amixer")
            .args([
                "-c",
                &self.input_card_index.to_string(),
                "get",
                &self.input_control,
            ])
            .output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            let available = Self::available_controls(self.input_card_index);
            return Err(AudioError::Command(format!(
                "{stderr}Available controls on card {}: {available}",
                self.input_card_index
            )));
        }
        Self::parse_pct(&String::from_utf8_lossy(&out.stdout))
    }

    fn set_mic_gain_pct(&self, pct: u8) -> Result<(), AudioError> {
        let pct_arg = format!("{pct}%");
        let out = Command::new("amixer")
            .args([
                "-c",
                &self.input_card_index.to_string(),
                "sset",
                &self.input_control,
                &pct_arg,
            ])
            .output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            let available = Self::available_controls(self.input_card_index);
            return Err(AudioError::Command(format!(
                "{stderr}Available controls on card {}: {available}",
                self.input_card_index
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pct_extracts_first_percentage() {
        let output = "Simple mixer control 'Digital',0\n  \
            Limits: Playback 0 - 255\n  \
            Mono: Playback 214 [84%] [-12.28dB]\n";
        let pct = AmixerControl::parse_pct(output).unwrap();
        assert_eq!(pct, 84);
    }

    #[test]
    fn parse_pct_handles_zero() {
        let output = "  Front Left: Playback 0 [0%] [-inf]\n";
        let pct = AmixerControl::parse_pct(output).unwrap();
        assert_eq!(pct, 0);
    }

    #[test]
    fn parse_pct_handles_100() {
        let output = "  Mono: Playback 255 [100%] [0.00dB]\n";
        let pct = AmixerControl::parse_pct(output).unwrap();
        assert_eq!(pct, 100);
    }

    #[test]
    fn parse_pct_error_on_no_percentage() {
        let output = "No controls found.\n";
        let err = AmixerControl::parse_pct(output).unwrap_err();
        assert!(matches!(err, AudioError::Parse(_)));
    }
}
