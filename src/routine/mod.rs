// Autonomous on-robot routine engine.
//
// The `RoutineEngine` spawns a Tokio task for the named routine and tracks
// its lifecycle.  Callers interact via `start`, `stop`, and `status`.
// Only one routine may run at a time.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::calibration::CalibrationStore;
use crate::config::Config;
use crate::hat::gpio::HatGpio;
use crate::hat::i2c::Hat;
use crate::hat::motor;
use crate::hat::servo::LeaseManager;

/// Pseudo-connection ID for routine-owned motor leases (never a real client ID).
pub const ROUTINE_CONN_ID: u64 = 0;

/// Runtime state of the routine engine.
#[derive(Debug, Clone, PartialEq)]
pub enum RoutineState {
    Idle,
    Running,
    Stopping,
}

/// Accumulated statistics from a routine run.
#[derive(Debug, Clone, Default)]
pub struct RoutineStats {
    pub obstacles_avoided: u32,
    pub cliffs_avoided: u32,
}

/// Point-in-time snapshot of routine engine state.
#[derive(Debug)]
pub struct RoutineStatusSnapshot {
    pub running: bool,
    pub name: Option<String>,
    pub elapsed_s: Option<u64>,
    pub obstacles_avoided: Option<u32>,
    pub cliffs_avoided: Option<u32>,
}

/// Stats returned when a routine stops.
#[derive(Debug)]
pub struct RoutineStopResult {
    pub name: String,
    pub ran_for_s: u64,
    pub obstacles_avoided: u32,
    pub cliffs_avoided: u32,
    pub stop_reason: String,
}

struct ActiveRoutine {
    name: String,
    started_at: Instant,
    stop_flag: Arc<AtomicBool>,
    handle: JoinHandle<(RoutineStats, String)>,
}

/// Manages lifecycle of a single autonomous routine task.
pub struct RoutineEngine {
    hat: Arc<Hat>,
    gpio: Arc<HatGpio>,
    config: Arc<Config>,
    calibration: Arc<Mutex<CalibrationStore>>,
    motor_lease_manager: Arc<LeaseManager>,
    active: Option<ActiveRoutine>,
}

impl RoutineEngine {
    /// Create a new idle engine.
    pub fn new(
        hat: Arc<Hat>,
        gpio: Arc<HatGpio>,
        config: Arc<Config>,
        calibration: Arc<Mutex<CalibrationStore>>,
        motor_lease_manager: Arc<LeaseManager>,
    ) -> Self {
        Self {
            hat,
            gpio,
            config,
            calibration,
            motor_lease_manager,
            active: None,
        }
    }

    /// Returns `true` if a routine is currently running.
    pub fn is_running(&self) -> bool {
        self.active.is_some()
    }

    /// Start the named routine, optionally overriding config defaults.
    ///
    /// Returns the Unix epoch seconds at task spawn on success.
    /// Returns `"ALREADY_RUNNING"` if a routine is active.
    /// Returns `"INVALID_PARAMS"` if the name is not recognised.
    pub fn start(
        &mut self,
        name: &str,
        speed_pct: Option<f64>,
        obstacle_threshold_cm: Option<f64>,
        cliff_threshold_normalized: Option<f64>,
        max_duration_s: Option<u64>,
    ) -> Result<u64, &'static str> {
        if self.active.is_some() {
            return Err("ALREADY_RUNNING");
        }
        if name != "explore" {
            return Err("INVALID_PARAMS");
        }

        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_clone = stop_flag.clone();

        let hat = self.hat.clone();
        let gpio = self.gpio.clone();
        let config = self.config.clone();
        let calibration = self.calibration.clone();
        let motor_lease_manager = self.motor_lease_manager.clone();

        let params = crate::routine::explore::ExploreParams {
            speed_pct: speed_pct.unwrap_or(config.routine.explore_speed_pct),
            obstacle_threshold_cm: obstacle_threshold_cm
                .unwrap_or(config.routine.obstacle_threshold_cm),
            cliff_threshold_normalized: cliff_threshold_normalized
                .unwrap_or(config.routine.cliff_threshold_normalized),
            max_duration: Duration::from_secs(
                max_duration_s.unwrap_or(config.routine.max_duration_s),
            ),
            loop_interval: Duration::from_millis(config.routine.loop_interval_ms),
            avoidance_backup: Duration::from_millis(config.routine.avoidance_backup_ms),
            avoidance_turn_angle_deg: config.routine.avoidance_turn_angle_deg,
        };

        let handle = tokio::spawn(async move {
            crate::routine::explore::explore_task(
                hat,
                gpio,
                config,
                calibration,
                motor_lease_manager,
                params,
                stop_flag_clone,
            )
            .await
        });

        let started_at = Instant::now();
        let started_at_unix_s = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.active = Some(ActiveRoutine {
            name: name.to_string(),
            started_at,
            stop_flag,
            handle,
        });

        Ok(started_at_unix_s)
    }

    /// Extract the active routine for stopping outside the engine lock.
    ///
    /// Returns `None` if no routine is running.  The caller is responsible for
    /// signalling `stop_flag`, waiting for the handle, and idling motors on
    /// timeout/abort.
    pub fn take_active(
        &mut self,
    ) -> Option<(String, Instant, Arc<AtomicBool>, JoinHandle<(RoutineStats, String)>)> {
        self.active
            .take()
            .map(|a| (a.name, a.started_at, a.stop_flag, a.handle))
    }

    /// Stop the active routine and wait for it to finish (up to 2 s).
    ///
    /// On timeout the task is aborted and motors are idled as a safety
    /// backstop.  Returns `"INVALID_PARAMS"` if no routine is running.
    pub async fn stop(&mut self) -> Result<RoutineStopResult, &'static str> {
        let (name, started_at, stop_flag, mut handle) =
            self.take_active().ok_or("INVALID_PARAMS")?;
        stop_flag.store(true, Ordering::Relaxed);
        let ran_for_s = started_at.elapsed().as_secs();

        let (stats, stop_reason) =
            match tokio::time::timeout(Duration::from_secs(2), &mut handle).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => (RoutineStats::default(), "error".to_string()),
                Err(_timeout) => {
                    // Timeout — abort the task so it cannot keep running.
                    handle.abort();
                    let _ = handle.await;
                    // Best-effort motor idle as safety backstop.
                    for cfg in &self.config.motors {
                        if let Err(e) = motor::idle_motor(&self.hat, cfg.pwm_channel).await {
                            tracing::warn!(
                                error = %e,
                                pwm_channel = cfg.pwm_channel,
                                "routine stop: SAFETY: failed to idle motor after task abort"
                            );
                        }
                    }
                    self.motor_lease_manager
                        .release_connection(ROUTINE_CONN_ID)
                        .await;
                    (RoutineStats::default(), "timeout_abort".to_string())
                }
            };

        Ok(RoutineStopResult {
            name,
            ran_for_s,
            obstacles_avoided: stats.obstacles_avoided,
            cliffs_avoided: stats.cliffs_avoided,
            stop_reason,
        })
    }

    /// Return a point-in-time snapshot of engine state without affecting it.
    pub fn status(&self) -> RoutineStatusSnapshot {
        match &self.active {
            None => RoutineStatusSnapshot {
                running: false,
                name: None,
                elapsed_s: None,
                obstacles_avoided: None,
                cliffs_avoided: None,
            },
            Some(active) => RoutineStatusSnapshot {
                running: true,
                name: Some(active.name.clone()),
                elapsed_s: Some(active.started_at.elapsed().as_secs()),
                // Live stats require a separate channel (Phase 12 enhancement).
                obstacles_avoided: None,
                cliffs_avoided: None,
            },
        }
    }
}

pub mod explore;

#[cfg(test)]
mod tests {
    #[test]
    fn routine_config_defaults_are_valid() {
        let cfg = crate::config::Config::default();
        // The default RoutineConfig must be in-range (matches validation rules).
        assert!((1.0..=100.0).contains(&cfg.routine.explore_speed_pct));
        assert!(cfg.routine.obstacle_threshold_cm > 0.0);
        assert!((0.0..=1.0).contains(&cfg.routine.cliff_threshold_normalized));
        assert!(cfg.routine.loop_interval_ms >= 50);
    }

    #[test]
    fn config_with_cliff_threshold_out_of_range_is_rejected() {
        use std::io::Write;
        // Write a complete [routine] section so serde can parse it; only
        // cliff_threshold_normalized is out of range to trigger validation.
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            "[routine]\n\
             explore_speed_pct = 30.0\n\
             obstacle_threshold_cm = 25.0\n\
             cliff_threshold_normalized = 1.5\n\
             loop_interval_ms = 100\n\
             avoidance_backup_ms = 500\n\
             avoidance_turn_angle_deg = 60.0\n\
             max_duration_s = 300\n"
        )
        .unwrap();
        let err = crate::config::Config::load(f.path()).unwrap_err();
        assert!(
            err.to_string().contains("cliff_threshold_normalized"),
            "expected error to mention field, got: {err}"
        );
    }
}
