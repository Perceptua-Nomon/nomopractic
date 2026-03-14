// Explore routine — drive forward, avoid obstacles and cliffs.
//
// The routine loops at `loop_interval` polling the ultrasonic sensor
// (obstacle) and grayscale ADC channels (cliff).  On detection it
// reverses, then steers away before resuming forward motion.
//
// The task returns `(RoutineStats, stop_reason)` where `stop_reason`
// is one of `"commanded"` (external stop), `"timeout"` (max_duration
// reached), or `"error"`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::calibration::CalibrationStore;
use crate::config::Config;
use crate::hat::gpio::HatGpio;
use crate::hat::i2c::Hat;
use crate::hat::{adc, motor, servo, ultrasonic};

use super::RoutineStats;

/// Per-run parameters for `explore_task`.
#[derive(Clone)]
pub struct ExploreParams {
    /// Forward (and backward) speed percentage.
    pub speed_pct: f64,
    /// Stop-and-avoid threshold for ultrasonic sensor (cm).
    pub obstacle_threshold_cm: f64,
    /// Cliff detection threshold (normalised 0.0–1.0).
    pub cliff_threshold_normalized: f64,
    /// Maximum total run duration.
    pub max_duration: Duration,
    /// Sensor-poll loop interval.
    pub loop_interval: Duration,
    /// Reverse duration during avoidance.
    pub avoidance_backup: Duration,
    /// Degrees added to 90° for the avoidance turn.
    pub avoidance_turn_angle_deg: f64,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Main explore task body.  Returns `(stats, stop_reason)`.
pub async fn explore_task(
    hat: Arc<Hat>,
    gpio: Arc<HatGpio>,
    config: Arc<Config>,
    calibration: Arc<Mutex<CalibrationStore>>,
    params: ExploreParams,
    stop_flag: Arc<AtomicBool>,
) -> (RoutineStats, String) {
    let mut stats = RoutineStats::default();
    let started = Instant::now();

    info!(
        speed_pct = params.speed_pct,
        cliff_threshold = params.cliff_threshold_normalized,
        obstacle_cm = params.obstacle_threshold_cm,
        "explore_task: started"
    );

    loop {
        // 1. Check stop flag.
        if stop_flag.load(Ordering::Relaxed) {
            info!("explore_task: stop commanded");
            stop_motors(&hat, &config).await;
            return (stats, "commanded".to_string());
        }

        // 2. Check max duration.
        if started.elapsed() >= params.max_duration {
            info!("explore_task: max_duration reached");
            stop_motors(&hat, &config).await;
            return (stats, "timeout".to_string());
        }

        // 3. Read ultrasonic (distance OK = no obstacle within threshold).
        let distance_ok = read_ultrasonic(&gpio, &config, &params).await;

        // 4. Read normalised grayscale for cliff detection.
        let cliff_detected = read_normalized_cliff(&hat, &config, &calibration, &params).await;

        // 5. Cliff avoidance takes priority.
        if cliff_detected {
            stats.cliffs_avoided += 1;
            warn!("explore_task: cliff detected — avoiding");
            stop_motors(&hat, &config).await;
            drive_all(&hat, &gpio, &config, &calibration, -params.speed_pct).await;
            tokio::time::sleep(params.avoidance_backup).await;
            stop_motors(&hat, &config).await;
            steer_channel(&hat, &config, 90.0 + params.avoidance_turn_angle_deg).await;
            tokio::time::sleep(Duration::from_millis(400)).await;
            steer_channel(&hat, &config, 90.0).await;
            continue;
        }

        // 6. Obstacle avoidance.
        if !distance_ok {
            stats.obstacles_avoided += 1;
            warn!("explore_task: obstacle detected — avoiding");
            stop_motors(&hat, &config).await;
            drive_all(&hat, &gpio, &config, &calibration, -params.speed_pct).await;
            tokio::time::sleep(params.avoidance_backup).await;
            stop_motors(&hat, &config).await;
            steer_channel(&hat, &config, 90.0 + params.avoidance_turn_angle_deg).await;
            tokio::time::sleep(Duration::from_millis(400)).await;
            steer_channel(&hat, &config, 90.0).await;
            continue;
        }

        // 7. Clear — drive forward.
        drive_all(&hat, &gpio, &config, &calibration, params.speed_pct).await;
        steer_channel(&hat, &config, 90.0).await;

        tokio::time::sleep(params.loop_interval).await;
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Stop all configured motors (best-effort; errors logged but not fatal).
async fn stop_motors(hat: &Hat, config: &Config) {
    for cfg in &config.motors {
        if let Err(e) = motor::idle_motor(hat, cfg.pwm_channel).await {
            warn!(error = %e, pwm_channel = cfg.pwm_channel, "explore: stop_motors failed");
        }
    }
}

/// Command all motors to `speed_pct`, applying motor calibration.
async fn drive_all(
    hat: &Hat,
    gpio: &HatGpio,
    config: &Config,
    calibration: &Mutex<CalibrationStore>,
    speed_pct: f64,
) {
    // Snapshot calibration for all channels; drop lock before hardware calls.
    let cal_entries: Vec<(f64, bool)> = {
        let guard = calibration.lock().await;
        config
            .motors
            .iter()
            .enumerate()
            .map(|(i, cfg)| {
                let cal = &guard.motors[i];
                let scaled = (speed_pct * cal.speed_scale).clamp(-100.0, 100.0);
                let effective = if scaled.abs() < cal.deadband_pct {
                    0.0
                } else {
                    scaled
                };
                let reversed = cal.reversed ^ cfg.reversed;
                (effective, reversed)
            })
            .collect()
    };

    for (i, cfg) in config.motors.iter().enumerate() {
        let (effective, reversed) = cal_entries[i];
        if let Err(e) = motor::set_motor_speed(
            hat,
            gpio,
            cfg.pwm_channel,
            cfg.dir_pin_bcm,
            reversed,
            effective,
        )
        .await
        {
            warn!(error = %e, channel = i, "explore: drive_all set_motor_speed failed");
        }
    }
}

/// Set the steering servo to `angle_deg` if configured; errors are logged.
async fn steer_channel(hat: &Hat, config: &Config, angle_deg: f64) {
    let ch = match config.servos.steering {
        Some(ch) => ch,
        None => return, // steering not configured
    };
    let angle_clamped = angle_deg.clamp(0.0, 180.0);
    if let Err(e) = servo::set_servo_angle(hat, ch, angle_clamped).await {
        warn!(error = %e, angle_deg, "explore: steer_channel failed");
    }
}

/// Read the ultrasonic distance and return `true` if the path is clear.
///
/// Returns `true` (clear) on any measurement error so the robot does not
/// get stuck if the sensor is temporarily unavailable.
async fn read_ultrasonic(gpio: &HatGpio, config: &Config, params: &ExploreParams) -> bool {
    let cfg = &config.ultrasonic;
    match ultrasonic::read_distance_cm(gpio, cfg.trig_pin_bcm, cfg.echo_pin_bcm, cfg.timeout_ms)
        .await
    {
        Ok(dist) => dist >= params.obstacle_threshold_cm,
        Err(e) => {
            warn!(error = %e, "explore: ultrasonic read failed — treating as clear");
            true
        }
    }
}

/// Read all three grayscale channels and return `true` if a cliff is detected.
///
/// Normalisation: `(raw - white_raw) / (black_raw - white_raw)`, clamped 0–1.
/// Returns `false` (no cliff) on ADC errors or invalid calibration.
async fn read_normalized_cliff(
    hat: &Hat,
    config: &Config,
    calibration: &Mutex<CalibrationStore>,
    params: &ExploreParams,
) -> bool {
    // Copy calibration before any hardware calls.
    let cal_gs = {
        let guard = calibration.lock().await;
        guard.grayscale.clone()
    };

    let [ch0, ch1, ch2] = config.sensors.grayscale;

    for (ch, gs) in [ch0, ch1, ch2].iter().zip(cal_gs.iter()) {
        // Sanity-check calibration invariant.
        if gs.white_raw >= gs.black_raw {
            continue; // Skip this channel; treat as not-cliff.
        }

        let raw = match adc::read_adc(hat, *ch).await {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, channel = ch, "explore: grayscale ADC read failed — skipping channel");
                continue;
            }
        };

        let white = gs.white_raw as f64;
        let black = gs.black_raw as f64;
        let normalized = ((raw as f64 - white) / (black - white)).clamp(0.0, 1.0);

        if normalized >= params.cliff_threshold_normalized {
            return true;
        }
    }

    false
}
