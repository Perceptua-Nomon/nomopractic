// Request dispatch — routes method names to HAT driver functions.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tracing::{debug, error, warn};

use super::schema::{Request, Response};
use crate::config::Config;
use crate::hat::adc;
use crate::hat::battery;
use crate::hat::gpio::{self, GpioError, HatGpio};
use crate::hat::i2c::{Hat, HatError};
use crate::hat::motor::{self, MotorError};
use crate::hat::servo::LeaseManager;
use crate::hat::ultrasonic;
use crate::hat::{pwm, servo};
use crate::reset;

/// Minimum interval between consecutive MCU reset requests (ms).
const RESET_MIN_INTERVAL_MS: u64 = 1000;

/// Snapshot of MCU reset state, kept under a single mutex for consistency.
struct McuState {
    reset_count: u64,
    last_reset_at: Option<Instant>,
}

/// Classify a `HatError` into an IPC error code string.
fn hat_error_code(e: &HatError) -> &'static str {
    match e {
        HatError::I2c(_) => "HARDWARE_ERROR",
        HatError::InvalidChannel(_)
        | HatError::InvalidServoChannel(_)
        | HatError::InvalidMotorChannel(_)
        | HatError::InvalidPulse(_)
        | HatError::InvalidAngle(_)
        | HatError::InvalidParam(_) => "INVALID_PARAMS",
    }
}

/// Classify a `MotorError` into an IPC error code string.
fn motor_error_code(e: &MotorError) -> &'static str {
    match e {
        MotorError::Hat(he) => hat_error_code(he),
        MotorError::Gpio(ge) => gpio_error_code(ge),
    }
}

/// Classify a `GpioError` into an IPC error code string.
fn gpio_error_code(e: &GpioError) -> &'static str {
    match e {
        GpioError::Gpio(_) => "HARDWARE_ERROR",
        GpioError::ReadOnly(_) => "INVALID_PARAMS",
    }
}

/// Processes incoming IPC requests and returns serialized JSON responses.
pub struct Handler {
    config: Arc<Config>,
    start_time: Instant,
    hat: Arc<Hat>,
    lease_manager: Arc<LeaseManager>,
    motor_lease_manager: Arc<LeaseManager>,
    gpio: Arc<HatGpio>,
    mcu_state: tokio::sync::Mutex<McuState>,
}

impl Handler {
    pub fn new(config: Arc<Config>, hat: Arc<Hat>, gpio: Arc<HatGpio>) -> Self {
        Self {
            config,
            start_time: Instant::now(),
            hat,
            lease_manager: Arc::new(LeaseManager::new()),
            motor_lease_manager: Arc::new(LeaseManager::new()),
            gpio,
            mcu_state: tokio::sync::Mutex::new(McuState {
                reset_count: 0,
                last_reset_at: None,
            }),
        }
    }

    pub fn lease_manager(&self) -> &Arc<LeaseManager> {
        &self.lease_manager
    }

    pub fn motor_lease_manager(&self) -> &Arc<LeaseManager> {
        &self.motor_lease_manager
    }

    pub fn hat(&self) -> &Arc<Hat> {
        &self.hat
    }

    pub fn config(&self) -> &Arc<Config> {
        &self.config
    }

    /// Release all servo and motor leases for a disconnected connection and idle
    /// any active channels.
    pub async fn on_client_disconnect(&self, conn_id: u64) {
        // Idle servo channels.
        let servo_channels = self.lease_manager.release_connection(conn_id).await;
        for ch in servo_channels {
            warn!(
                channel = ch,
                conn_id, "client disconnected; idling leased servo channel"
            );
            if let Err(e) = pwm::set_channel_pulse_us(&self.hat, ch, 0).await {
                error!(error = %e, channel = ch, "failed to idle servo channel on client disconnect");
            }
        }
        // Idle motor channels.
        let motor_channels = self.motor_lease_manager.release_connection(conn_id).await;
        for ipc_ch in motor_channels {
            warn!(
                channel = ipc_ch,
                conn_id, "client disconnected; stopping leased motor"
            );
            if let Some(cfg) = self.config.motors.get(ipc_ch as usize)
                && let Err(e) = motor::idle_motor(&self.hat, cfg.pwm_channel).await
            {
                error!(error = %e, channel = ipc_ch, "failed to stop motor on client disconnect");
            }
        }
    }

    /// Parse a raw JSON line, dispatch the method, and return a JSON response string.
    ///
    /// `conn_id` identifies the originating connection for servo lease tracking.
    pub async fn dispatch(&self, raw: &str, conn_id: u64) -> String {
        let request: Request = match serde_json::from_str(raw) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "malformed request");
                let resp = Response::err(
                    String::new(),
                    "INVALID_PARAMS",
                    format!("malformed JSON: {e}"),
                );
                return serde_json::to_string(&resp)
                    .unwrap_or_else(|_| r#"{"id":"","ok":false,"error":{"code":"INTERNAL_ERROR","message":"serialization failed"}}"#.into());
            }
        };

        debug!(id = %request.id, method = %request.method, "dispatching");

        let response = match request.method.as_str() {
            "health" => self.handle_health(&request),
            "get_battery_voltage" => self.handle_get_battery_voltage(&request).await,
            "read_adc" => self.handle_read_adc(&request).await,
            "set_servo_pulse_us" => self.handle_set_servo_pulse_us(&request, conn_id).await,
            "set_servo_angle" => self.handle_set_servo_angle(&request, conn_id).await,
            "read_gpio" => self.handle_read_gpio(&request).await,
            "write_gpio" => self.handle_write_gpio(&request).await,
            "reset_mcu" => self.handle_reset_mcu(&request).await,
            "set_motor_speed" => self.handle_set_motor_speed(&request, conn_id).await,
            "stop_all_motors" => self.handle_stop_all_motors(&request).await,
            "get_motor_status" => self.handle_get_motor_status(&request).await,
            "get_servo_status" => self.handle_get_servo_status(&request).await,
            "get_mcu_status" => self.handle_get_mcu_status(&request).await,
            // Convenience / coordinated methods.
            "drive" => self.handle_drive(&request, conn_id).await,
            "steer" => self.handle_steer(&request, conn_id).await,
            "pan_camera" => self.handle_pan_camera(&request, conn_id).await,
            "tilt_camera" => self.handle_tilt_camera(&request, conn_id).await,
            "read_grayscale" => self.handle_read_grayscale(&request).await,
            "read_ultrasonic" => self.handle_read_ultrasonic(&request).await,
            "enable_speaker" => self.handle_enable_speaker(&request).await,
            "disable_speaker" => self.handle_disable_speaker(&request).await,
            other => {
                warn!(method = other, "unknown method");
                Response::err(
                    request.id,
                    "UNKNOWN_METHOD",
                    format!("method '{}' not recognized", other),
                )
            }
        };

        serde_json::to_string(&response)
            .unwrap_or_else(|_| r#"{"id":"","ok":false,"error":{"code":"INTERNAL_ERROR","message":"serialization failed"}}"#.into())
    }

    fn handle_health(&self, request: &Request) -> Response {
        let uptime = self.start_time.elapsed();
        Response::ok(
            request.id.clone(),
            json!({
                "schema_version": "1.0.0",
                "status": "ok",
                "version": env!("CARGO_PKG_VERSION"),
                "hat_address": format!("0x{:02x}", self.config.hat_address),
                "i2c_bus": self.config.i2c_bus,
                "uptime_s": uptime.as_secs(),
            }),
        )
    }

    async fn handle_get_battery_voltage(&self, request: &Request) -> Response {
        match battery::get_battery_voltage(&self.hat).await {
            Ok(voltage) => Response::ok(request.id.clone(), json!({ "voltage_v": voltage })),
            Err(e) => {
                warn!(error = %e, "battery read failed");
                Response::err(request.id.clone(), "HARDWARE_ERROR", e.to_string())
            }
        }
    }

    async fn handle_read_adc(&self, request: &Request) -> Response {
        let channel = match request.params.get("channel").and_then(|v| v.as_u64()) {
            Some(ch) if ch <= 7 => ch as u8,
            Some(_) | None => {
                return Response::err(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    "channel is required and must be 0–7",
                );
            }
        };
        match adc::read_adc(&self.hat, channel).await {
            Ok(raw) => Response::ok(
                request.id.clone(),
                json!({ "channel": channel, "raw_value": raw }),
            ),
            Err(e) => {
                warn!(error = %e, "read_adc failed");
                Response::err(request.id.clone(), hat_error_code(&e), e.to_string())
            }
        }
    }

    async fn handle_set_servo_pulse_us(&self, request: &Request, conn_id: u64) -> Response {
        let channel = match extract_channel(request) {
            Ok(ch) => ch,
            Err(resp) => return resp,
        };
        let pulse_us = match request.params.get("pulse_us").and_then(|v| v.as_u64()) {
            Some(p) if (500..=2500).contains(&p) => p as u16,
            Some(_) | None => {
                return Response::err(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    "pulse_us is required and must be 500–2500",
                );
            }
        };
        let ttl_ms = extract_ttl(request, self.config.servo_default_ttl_ms);
        let ttl_ms = match ttl_ms {
            Ok(ms) => ms,
            Err(resp) => return resp,
        };

        match servo::set_servo_pulse_us(&self.hat, channel, pulse_us).await {
            Ok(()) => {
                self.lease_manager.set_lease(channel, conn_id, ttl_ms).await;
                Response::ok(
                    request.id.clone(),
                    json!({ "channel": channel, "pulse_us": pulse_us }),
                )
            }
            Err(e) => {
                warn!(error = %e, "set_servo_pulse_us failed");
                Response::err(request.id.clone(), hat_error_code(&e), e.to_string())
            }
        }
    }

    async fn handle_set_servo_angle(&self, request: &Request, conn_id: u64) -> Response {
        let channel = match extract_channel(request) {
            Ok(ch) => ch,
            Err(resp) => return resp,
        };
        let angle_deg = match request.params.get("angle_deg").and_then(|v| v.as_f64()) {
            Some(a) if (0.0..=180.0).contains(&a) => a,
            Some(_) | None => {
                return Response::err(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    "angle_deg is required and must be 0.0–180.0",
                );
            }
        };
        let ttl_ms = extract_ttl(request, self.config.servo_default_ttl_ms);
        let ttl_ms = match ttl_ms {
            Ok(ms) => ms,
            Err(resp) => return resp,
        };

        match servo::set_servo_angle(&self.hat, channel, angle_deg).await {
            Ok(pulse_us) => {
                self.lease_manager.set_lease(channel, conn_id, ttl_ms).await;
                Response::ok(
                    request.id.clone(),
                    json!({
                        "channel": channel,
                        "angle_deg": angle_deg,
                        "pulse_us": pulse_us,
                    }),
                )
            }
            Err(e) => {
                warn!(error = %e, "set_servo_angle failed");
                Response::err(request.id.clone(), hat_error_code(&e), e.to_string())
            }
        }
    }

    async fn handle_read_gpio(&self, request: &Request) -> Response {
        let pin_name = match request.params.get("pin").and_then(|v| v.as_str()) {
            Some(s) => s.to_owned(),
            None => return Response::err(request.id.clone(), "INVALID_PARAMS", "pin is required"),
        };
        let pin = match gpio::GpioPin::from_name(&pin_name) {
            Some(p) => p,
            None => {
                return Response::err(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    format!("unknown pin '{pin_name}'; valid: D4, D5, MCURST, SW, LED"),
                );
            }
        };
        match gpio::read_gpio_pin(&self.gpio, pin).await {
            Ok(high) => Response::ok(
                request.id.clone(),
                json!({ "pin": pin.name(), "high": high }),
            ),
            Err(e) => {
                warn!(error = %e, "read_gpio failed");
                Response::err(request.id.clone(), gpio_error_code(&e), e.to_string())
            }
        }
    }

    async fn handle_write_gpio(&self, request: &Request) -> Response {
        let pin_name = match request.params.get("pin").and_then(|v| v.as_str()) {
            Some(s) => s.to_owned(),
            None => return Response::err(request.id.clone(), "INVALID_PARAMS", "pin is required"),
        };
        let pin = match gpio::GpioPin::from_name(&pin_name) {
            Some(p) => p,
            None => {
                return Response::err(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    format!("unknown pin '{pin_name}'; valid output pins: D4, D5, MCURST, LED"),
                );
            }
        };
        let high = match request.params.get("high").and_then(|v| v.as_bool()) {
            Some(h) => h,
            None => {
                return Response::err(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    "high is required and must be a boolean",
                );
            }
        };
        match gpio::write_gpio_pin(&self.gpio, pin, high).await {
            Ok(()) => Response::ok(
                request.id.clone(),
                json!({ "pin": pin.name(), "high": high }),
            ),
            Err(e) => {
                warn!(error = %e, "write_gpio failed");
                Response::err(request.id.clone(), gpio_error_code(&e), e.to_string())
            }
        }
    }

    async fn handle_reset_mcu(&self, request: &Request) -> Response {
        let now = Instant::now();
        {
            // Check the rate-limit and release the lock before the reset so
            // that `reset_mcu` (which acquires `gpio.bus`) is not executed
            // under `mcu_state` — this avoids holding the lock across an
            // async sleep.  The lock is re-acquired below only on success;
            // a failed attempt therefore does not block a retry.
            let guard = self.mcu_state.lock().await;
            if let Some(last) = guard.last_reset_at {
                let elapsed_ms = now
                    .checked_duration_since(last)
                    .unwrap_or(Duration::ZERO)
                    .as_millis() as u64;
                if elapsed_ms < RESET_MIN_INTERVAL_MS {
                    let remaining = RESET_MIN_INTERVAL_MS - elapsed_ms;
                    return Response::err(
                        request.id.clone(),
                        "HARDWARE_ERROR",
                        format!("MCU reset rate-limited; retry after {remaining} ms"),
                    );
                }
            }
        }
        match reset::reset_mcu(&self.gpio).await {
            Ok(result) => {
                let mut guard = self.mcu_state.lock().await;
                guard.last_reset_at = Some(now);
                guard.reset_count += 1;
                Response::ok(request.id.clone(), json!({ "reset_ms": result.reset_ms }))
            }
            Err(e) => {
                warn!(error = %e, "reset_mcu failed");
                Response::err(request.id.clone(), "HARDWARE_ERROR", e.to_string())
            }
        }
    }
    async fn handle_get_servo_status(&self, request: &Request) -> Response {
        let leases = self.lease_manager.get_active_leases().await;
        let active: Vec<_> = leases
            .iter()
            .map(|(ch, ttl_remaining_ms, conn_id)| {
                json!({
                    "channel": ch,
                    "ttl_remaining_ms": ttl_remaining_ms,
                    "conn_id": conn_id,
                })
            })
            .collect();
        Response::ok(request.id.clone(), json!({ "active_leases": active }))
    }

    async fn handle_get_mcu_status(&self, request: &Request) -> Response {
        let guard = self.mcu_state.lock().await;
        let resets = guard.reset_count;
        let last_reset_s_ago = guard.last_reset_at.map(|t| t.elapsed().as_secs());
        Response::ok(
            request.id.clone(),
            json!({
                "resets_since_start": resets,
                "last_reset_s_ago": last_reset_s_ago,
            }),
        )
    }

    async fn handle_set_motor_speed(&self, request: &Request, conn_id: u64) -> Response {
        let num_motors = self.config.motors.len();
        let ipc_channel = match request.params.get("channel").and_then(|v| v.as_u64()) {
            Some(ch) if (ch as usize) < num_motors => ch as u8,
            Some(ch) => {
                return Response::err(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    format!("channel {ch} is not configured; {num_motors} motor(s) available"),
                );
            }
            None => {
                return Response::err(request.id.clone(), "INVALID_PARAMS", "channel is required");
            }
        };
        let speed_pct = match request.params.get("speed_pct").and_then(|v| v.as_f64()) {
            Some(s) if (-100.0..=100.0).contains(&s) => s,
            Some(s) => {
                return Response::err(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    format!("speed_pct {s} is out of range -100.0..=100.0"),
                );
            }
            None => {
                return Response::err(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    "speed_pct is required",
                );
            }
        };
        let ttl_ms = match extract_ttl(request, self.config.motor_default_ttl_ms) {
            Ok(ms) => ms,
            Err(resp) => return resp,
        };

        let cfg = &self.config.motors[ipc_channel as usize];
        match motor::set_motor_speed(
            &self.hat,
            &self.gpio,
            cfg.pwm_channel,
            cfg.dir_pin_bcm,
            cfg.reversed,
            speed_pct,
        )
        .await
        {
            Ok(()) => {
                self.motor_lease_manager
                    .set_lease(ipc_channel, conn_id, ttl_ms)
                    .await;
                Response::ok(
                    request.id.clone(),
                    json!({ "channel": ipc_channel, "speed_pct": speed_pct }),
                )
            }
            Err(e) => {
                warn!(error = %e, "set_motor_speed failed");
                Response::err(request.id.clone(), motor_error_code(&e), e.to_string())
            }
        }
    }

    async fn handle_stop_all_motors(&self, request: &Request) -> Response {
        let mut errors: Vec<String> = Vec::new();
        for (ipc_ch, cfg) in self.config.motors.iter().enumerate() {
            if let Err(e) = motor::idle_motor(&self.hat, cfg.pwm_channel).await {
                error!(error = %e, channel = ipc_ch, "failed to stop motor");
                errors.push(format!("channel {ipc_ch}: {e}"));
            } else {
                self.motor_lease_manager.revoke_channel(ipc_ch as u8).await;
            }
        }
        if errors.is_empty() {
            Response::ok(
                request.id.clone(),
                json!({ "stopped": self.config.motors.len() }),
            )
        } else {
            Response::err(request.id.clone(), "HARDWARE_ERROR", errors.join("; "))
        }
    }

    async fn handle_get_motor_status(&self, request: &Request) -> Response {
        let leases = self.motor_lease_manager.get_active_leases().await;
        let active: Vec<_> = leases
            .iter()
            .map(|(ch, ttl_remaining_ms, conn_id)| {
                json!({
                    "channel": ch,
                    "ttl_remaining_ms": ttl_remaining_ms,
                    "conn_id": conn_id,
                })
            })
            .collect();
        Response::ok(request.id.clone(), json!({ "active_leases": active }))
    }

    // -----------------------------------------------------------------------
    // Convenience / coordinated methods
    // -----------------------------------------------------------------------

    /// `drive { speed_pct, ttl_ms? }` — set both configured motors to the
    /// same speed simultaneously.  All configured motors are commanded in a
    /// single Rust call to avoid the per-motor latency and race conditions that
    /// would occur when issuing two separate `set_motor_speed` IPC requests.
    ///
    /// Failure-atomic: leases are only committed after **all** motors have been
    /// successfully commanded.  If any motor fails, already-set motors are
    /// rolled back (idled) and no leases are committed.
    async fn handle_drive(&self, request: &Request, conn_id: u64) -> Response {
        let speed_pct = match request.params.get("speed_pct").and_then(|v| v.as_f64()) {
            Some(s) if (-100.0..=100.0).contains(&s) => s,
            Some(s) => {
                return Response::err(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    format!("speed_pct {s} is out of range -100.0..=100.0"),
                );
            }
            None => {
                return Response::err(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    "speed_pct is required",
                );
            }
        };
        let ttl_ms = match extract_ttl(request, self.config.motor_default_ttl_ms) {
            Ok(ms) => ms,
            Err(resp) => return resp,
        };

        if self.config.motors.is_empty() {
            return Response::err(request.id.clone(), "INVALID_PARAMS", "no motors configured");
        }

        // Phase 1: command all motors.  Track which channels succeeded so we
        // can roll back on partial failure.
        let mut succeeded: Vec<u8> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        for (ipc_ch, cfg) in self.config.motors.iter().enumerate() {
            match motor::set_motor_speed(
                &self.hat,
                &self.gpio,
                cfg.pwm_channel,
                cfg.dir_pin_bcm,
                cfg.reversed,
                speed_pct,
            )
            .await
            {
                Ok(()) => succeeded.push(ipc_ch as u8),
                Err(e) => {
                    warn!(error = %e, channel = ipc_ch, "drive: set_motor_speed failed");
                    errors.push(format!("channel {ipc_ch}: {e}"));
                }
            }
        }

        if errors.is_empty() {
            // Phase 2: all succeeded — commit leases now.
            for ipc_ch in &succeeded {
                self.motor_lease_manager
                    .set_lease(*ipc_ch, conn_id, ttl_ms)
                    .await;
            }
            Response::ok(
                request.id.clone(),
                json!({
                    "speed_pct": speed_pct,
                    "motors": self.config.motors.len(),
                }),
            )
        } else {
            // Phase 2: partial failure — roll back any motors that did start.
            let mut rollback_errors: Vec<String> = Vec::new();
            for ipc_ch in &succeeded {
                let cfg = &self.config.motors[*ipc_ch as usize];
                if let Err(e) = motor::idle_motor(&self.hat, cfg.pwm_channel).await {
                    error!(error = %e, channel = ipc_ch, "drive: rollback idle failed");
                    rollback_errors.push(format!("channel {ipc_ch}: rollback failed: {e}"));
                }
            }
            let mut all_errors = errors;
            all_errors.extend(rollback_errors);
            Response::err(request.id.clone(), "HARDWARE_ERROR", all_errors.join("; "))
        }
    }

    /// `steer { angle_deg, ttl_ms? }` — set the steering servo by name.
    async fn handle_steer(&self, request: &Request, conn_id: u64) -> Response {
        let ch = match self.config.servos.steering {
            Some(ch) => ch,
            None => {
                return Response::err(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    "steering servo is not configured",
                );
            }
        };
        self.set_named_servo(request, conn_id, ch, "steering").await
    }

    /// `pan_camera { angle_deg, ttl_ms? }` — set the camera pan servo by name.
    async fn handle_pan_camera(&self, request: &Request, conn_id: u64) -> Response {
        let ch = match self.config.servos.camera_pan {
            Some(ch) => ch,
            None => {
                return Response::err(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    "camera_pan servo is not configured",
                );
            }
        };
        self.set_named_servo(request, conn_id, ch, "camera_pan")
            .await
    }

    /// `tilt_camera { angle_deg, ttl_ms? }` — set the camera tilt servo by name.
    async fn handle_tilt_camera(&self, request: &Request, conn_id: u64) -> Response {
        let ch = match self.config.servos.camera_tilt {
            Some(ch) => ch,
            None => {
                return Response::err(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    "camera_tilt servo is not configured",
                );
            }
        };
        self.set_named_servo(request, conn_id, ch, "camera_tilt")
            .await
    }

    /// Common implementation for named-servo angle-set methods.
    async fn set_named_servo(
        &self,
        request: &Request,
        conn_id: u64,
        channel: u8,
        servo_name: &str,
    ) -> Response {
        let angle_deg = match request.params.get("angle_deg").and_then(|v| v.as_f64()) {
            Some(a) if (0.0..=180.0).contains(&a) => a,
            Some(_) | None => {
                return Response::err(
                    request.id.clone(),
                    "INVALID_PARAMS",
                    "angle_deg is required and must be 0.0–180.0",
                );
            }
        };
        let ttl_ms = match extract_ttl(request, self.config.servo_default_ttl_ms) {
            Ok(ms) => ms,
            Err(resp) => return resp,
        };

        match servo::set_servo_angle(&self.hat, channel, angle_deg).await {
            Ok(pulse_us) => {
                self.lease_manager.set_lease(channel, conn_id, ttl_ms).await;
                Response::ok(
                    request.id.clone(),
                    json!({
                        "servo": servo_name,
                        "channel": channel,
                        "angle_deg": angle_deg,
                        "pulse_us": pulse_us,
                    }),
                )
            }
            Err(e) => {
                warn!(error = %e, servo = servo_name, "set_named_servo failed");
                Response::err(request.id.clone(), hat_error_code(&e), e.to_string())
            }
        }
    }

    /// `read_grayscale {}` — read all three grayscale sensor ADC channels and
    /// return raw values as `[left, center, right]`.
    async fn handle_read_grayscale(&self, request: &Request) -> Response {
        let [ch0, ch1, ch2] = self.config.sensors.grayscale;
        let mut values = [0u16; 3];
        for (i, ch) in [ch0, ch1, ch2].iter().enumerate() {
            match adc::read_adc(&self.hat, *ch).await {
                Ok(raw) => values[i] = raw,
                Err(e) => {
                    warn!(error = %e, channel = ch, "read_grayscale ADC read failed");
                    return Response::err(
                        request.id.clone(),
                        hat_error_code(&e),
                        format!("grayscale channel {ch}: {e}"),
                    );
                }
            }
        }
        Response::ok(
            request.id.clone(),
            json!({
                "channels": [ch0, ch1, ch2],
                "values": [values[0], values[1], values[2]],
            }),
        )
    }

    /// `read_ultrasonic {}` — trigger the HC-SR04 sensor and return distance in
    /// centimetres. Uses the TRIG/ECHO BCM pins and timeout from config.
    async fn handle_read_ultrasonic(&self, request: &Request) -> Response {
        let cfg = &self.config.ultrasonic;
        match ultrasonic::read_distance_cm(
            &self.gpio,
            cfg.trig_pin_bcm,
            cfg.echo_pin_bcm,
            cfg.timeout_ms,
        )
        .await
        {
            Ok(distance_cm) => {
                Response::ok(request.id.clone(), json!({ "distance_cm": distance_cm }))
            }
            Err(ultrasonic::UltrasonicError::Timeout(ms)) => Response::err(
                request.id.clone(),
                "HARDWARE_ERROR",
                format!("ultrasonic measurement timed out after {ms} ms"),
            ),
            Err(ultrasonic::UltrasonicError::NoEcho) => Response::err(
                request.id.clone(),
                "HARDWARE_ERROR",
                "no valid echo received (object out of sensor range 2–400 cm)",
            ),
            Err(ultrasonic::UltrasonicError::Gpio(e)) => {
                Response::err(request.id.clone(), gpio_error_code(&e), e.to_string())
            }
        }
    }

    /// `enable_speaker {}` — assert the speaker-amplifier enable pin HIGH.
    async fn handle_enable_speaker(&self, request: &Request) -> Response {
        let bcm = self.config.speaker_en_pin_bcm;
        match self.gpio.bus.lock().await.write_pin(bcm, true) {
            Ok(()) => Response::ok(
                request.id.clone(),
                json!({ "enabled": true, "pin_bcm": bcm }),
            ),
            Err(e) => Response::err(request.id.clone(), gpio_error_code(&e), e.to_string()),
        }
    }

    /// `disable_speaker {}` — pull the speaker-amplifier enable pin LOW.
    async fn handle_disable_speaker(&self, request: &Request) -> Response {
        let bcm = self.config.speaker_en_pin_bcm;
        match self.gpio.bus.lock().await.write_pin(bcm, false) {
            Ok(()) => Response::ok(
                request.id.clone(),
                json!({ "enabled": false, "pin_bcm": bcm }),
            ),
            Err(e) => Response::err(request.id.clone(), gpio_error_code(&e), e.to_string()),
        }
    }
}

/// Extract and validate `channel` (0–11) from request params.
fn extract_channel(request: &Request) -> Result<u8, Response> {
    match request.params.get("channel").and_then(|v| v.as_u64()) {
        Some(ch) if ch <= 11 => Ok(ch as u8),
        Some(_) | None => Err(Response::err(
            request.id.clone(),
            "INVALID_PARAMS",
            "channel is required and must be 0–11",
        )),
    }
}

/// Extract and validate `ttl_ms` (100–5000) from request params, using
/// `default_ttl_ms` when the key is absent. The default is clamped to the
/// same 100–5000 ms range so misconfigured defaults are handled gracefully.
fn extract_ttl(request: &Request, default_ttl_ms: u64) -> Result<u64, Response> {
    const TTL_MIN_MS: u64 = 100;
    const TTL_MAX_MS: u64 = 5000;
    match request.params.get("ttl_ms") {
        None | Some(serde_json::Value::Null) => Ok(default_ttl_ms.clamp(TTL_MIN_MS, TTL_MAX_MS)),
        Some(v) => match v.as_u64() {
            Some(ms) if (TTL_MIN_MS..=TTL_MAX_MS).contains(&ms) => Ok(ms),
            _ => Err(Response::err(
                request.id.clone(),
                "INVALID_PARAMS",
                "ttl_ms must be an integer 100–5000",
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hat::gpio::{GpioBus, GpioError, HatGpio};
    use crate::hat::i2c::{HatError, I2cBus};

    struct MockI2c {
        response: [u8; 2],
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

    struct MockGpio {
        state: std::collections::HashMap<u8, bool>,
    }

    impl MockGpio {
        fn new() -> Self {
            Self {
                state: std::collections::HashMap::new(),
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

    fn test_handler() -> Handler {
        let hat = Arc::new(Hat::new(MockI2c { response: [0, 0] }, 0x14));
        let gpio = Arc::new(HatGpio::new(MockGpio::new()));
        Handler::new(Arc::new(Config::default()), hat, gpio)
    }

    fn test_handler_with_adc(hi: u8, lo: u8) -> Handler {
        let hat = Arc::new(Hat::new(MockI2c { response: [hi, lo] }, 0x14));
        let gpio = Arc::new(HatGpio::new(MockGpio::new()));
        Handler::new(Arc::new(Config::default()), hat, gpio)
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let handler = test_handler();
        let raw = r#"{"id":"1","method":"health","params":{}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "1");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["status"], "ok");
        assert_eq!(resp["result"]["schema_version"], "1.0.0");
        assert_eq!(resp["result"]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(resp["result"]["hat_address"], "0x14");
        assert_eq!(resp["result"]["i2c_bus"], 1);
        assert!(resp["result"]["uptime_s"].is_number());
    }

    #[tokio::test]
    async fn unknown_method_returns_error() {
        let handler = test_handler();
        let raw = r#"{"id":"2","method":"explode","params":{}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "2");
        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "UNKNOWN_METHOD");
    }

    #[tokio::test]
    async fn malformed_json_returns_error() {
        let handler = test_handler();
        let raw = "this is not json";
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[tokio::test]
    async fn missing_method_field_returns_error() {
        let handler = test_handler();
        let raw = r#"{"id":"3"}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[tokio::test]
    async fn params_default_to_null() {
        let handler = test_handler();
        // Valid request without params field — should still dispatch.
        let raw = r#"{"id":"4","method":"health"}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["status"], "ok");
    }

    #[tokio::test]
    async fn get_battery_voltage_returns_scaled_voltage() {
        // raw = 0x0FFF = 4095 (12-bit max) → voltage_v = (4095/4095) × 3.3 × 3.0 = 9.9 V
        let handler = test_handler_with_adc(0x0F, 0xFF);
        let raw = r#"{"id":"5","method":"get_battery_voltage","params":{}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "5");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["voltage_v"], 9.9_f64);
    }

    #[tokio::test]
    async fn set_servo_pulse_us_returns_channel_and_pulse() {
        let handler = test_handler();
        let raw =
            r#"{"id":"6","method":"set_servo_pulse_us","params":{"channel":0,"pulse_us":1500}}"#;
        let resp_str = handler.dispatch(raw, 1).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "6");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["channel"], 0);
        assert_eq!(resp["result"]["pulse_us"], 1500);
    }

    #[tokio::test]
    async fn set_servo_angle_returns_channel_angle_and_pulse() {
        let handler = test_handler();
        let raw =
            r#"{"id":"7","method":"set_servo_angle","params":{"channel":2,"angle_deg":90.0}}"#;
        let resp_str = handler.dispatch(raw, 1).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "7");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["channel"], 2);
        assert_eq!(resp["result"]["angle_deg"], 90.0_f64);
        assert_eq!(resp["result"]["pulse_us"], 1500);
    }

    #[tokio::test]
    async fn set_servo_pulse_us_invalid_channel_returns_invalid_params() {
        let handler = test_handler();
        let raw =
            r#"{"id":"8","method":"set_servo_pulse_us","params":{"channel":12,"pulse_us":1500}}"#;
        let resp_str = handler.dispatch(raw, 1).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[tokio::test]
    async fn set_servo_angle_invalid_angle_returns_invalid_params() {
        let handler = test_handler();
        let raw =
            r#"{"id":"9","method":"set_servo_angle","params":{"channel":0,"angle_deg":200.0}}"#;
        let resp_str = handler.dispatch(raw, 1).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[tokio::test]
    async fn reset_mcu_returns_reset_ms() {
        let handler = test_handler();
        let raw = r#"{"id":"r1","method":"reset_mcu","params":{}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "r1");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["reset_ms"], crate::reset::RESET_HOLD_MS);
    }

    #[tokio::test]
    async fn reset_mcu_rate_limited_on_rapid_retry() {
        let handler = test_handler();
        let raw = r#"{"id":"r2","method":"reset_mcu","params":{}}"#;
        // First call succeeds.
        handler.dispatch(raw, 0).await;
        // Immediate second call must be rate-limited.
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "HARDWARE_ERROR");
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains("rate-limited")
        );
    }

    #[tokio::test]
    async fn read_gpio_sw_returns_low_by_default() {
        let handler = test_handler();
        let raw = r#"{"id":"g1","method":"read_gpio","params":{"pin":"SW"}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "g1");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["pin"], "SW");
        assert_eq!(resp["result"]["high"], false);
    }

    #[tokio::test]
    async fn write_gpio_d4_returns_ok() {
        let handler = test_handler();
        let raw = r#"{"id":"g2","method":"write_gpio","params":{"pin":"D4","high":true}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "g2");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["pin"], "D4");
        assert_eq!(resp["result"]["high"], true);
    }

    #[tokio::test]
    async fn write_gpio_input_pin_returns_invalid_params() {
        let handler = test_handler();
        let raw = r#"{"id":"g3","method":"write_gpio","params":{"pin":"SW","high":true}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[tokio::test]
    async fn read_gpio_unknown_pin_returns_invalid_params() {
        let handler = test_handler();
        let raw = r#"{"id":"g4","method":"read_gpio","params":{"pin":"BADPIN"}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[tokio::test]
    async fn read_adc_valid_channel_returns_raw_value() {
        // raw bytes 0x00, 0x2A → u16 = 42
        let handler = test_handler_with_adc(0x00, 0x2A);
        let raw = r#"{"id":"a1","method":"read_adc","params":{"channel":0}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "a1");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["channel"], 0);
        assert_eq!(resp["result"]["raw_value"], 42);
    }

    #[tokio::test]
    async fn read_adc_max_channel_is_accepted() {
        let handler = test_handler_with_adc(0x01, 0x00);
        let raw = r#"{"id":"a2","method":"read_adc","params":{"channel":7}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["channel"], 7);
    }

    #[tokio::test]
    async fn read_adc_invalid_channel_returns_invalid_params() {
        let handler = test_handler();
        let raw = r#"{"id":"a3","method":"read_adc","params":{"channel":8}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[tokio::test]
    async fn get_servo_status_empty_returns_no_leases() {
        let handler = test_handler();
        let raw = r#"{"id":"v1","method":"get_servo_status","params":{}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "v1");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["active_leases"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn get_servo_status_after_set_returns_lease() {
        let handler = test_handler();
        // Register a lease on channel 3 owned by conn_id 42.
        let set_raw =
            r#"{"id":"6","method":"set_servo_pulse_us","params":{"channel":3,"pulse_us":1500}}"#;
        handler.dispatch(set_raw, 42).await;
        let raw = r#"{"id":"v2","method":"get_servo_status","params":{}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], true);
        let leases = resp["result"]["active_leases"].as_array().unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0]["channel"], 3);
        assert_eq!(leases[0]["conn_id"], 42);
        assert!(leases[0]["ttl_remaining_ms"].is_number());
    }

    #[tokio::test]
    async fn get_mcu_status_no_resets() {
        let handler = test_handler();
        let raw = r#"{"id":"m1","method":"get_mcu_status","params":{}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "m1");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["resets_since_start"], 0);
        assert!(resp["result"]["last_reset_s_ago"].is_null());
    }

    #[tokio::test]
    async fn get_mcu_status_after_reset() {
        let handler = test_handler();
        handler
            .dispatch(r#"{"id":"r1","method":"reset_mcu","params":{}}"#, 0)
            .await;
        let raw = r#"{"id":"m2","method":"get_mcu_status","params":{}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["resets_since_start"], 1);
        assert!(resp["result"]["last_reset_s_ago"].is_number());
    }

    // ------------------------------------------------------------------
    // Motor IPC handler tests
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn set_motor_speed_valid_returns_channel_and_speed() {
        let handler = test_handler();
        // Default config has motor channel 0 = pwm_channel 12, dir_pin BCM 24.
        let raw =
            r#"{"id":"mo1","method":"set_motor_speed","params":{"channel":0,"speed_pct":50.0}}"#;
        let resp_str = handler.dispatch(raw, 1).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "mo1");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["channel"], 0);
        assert_eq!(resp["result"]["speed_pct"], 50.0_f64);
    }

    #[tokio::test]
    async fn set_motor_speed_channel_out_of_range_returns_invalid_params() {
        let handler = test_handler();
        // Default config has 2 motors (indices 0 and 1); index 2 is not configured.
        let raw =
            r#"{"id":"mo2","method":"set_motor_speed","params":{"channel":2,"speed_pct":50.0}}"#;
        let resp_str = handler.dispatch(raw, 1).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[tokio::test]
    async fn set_motor_speed_out_of_range_speed_returns_invalid_params() {
        let handler = test_handler();
        let raw =
            r#"{"id":"mo3","method":"set_motor_speed","params":{"channel":0,"speed_pct":150.0}}"#;
        let resp_str = handler.dispatch(raw, 1).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[tokio::test]
    async fn stop_all_motors_returns_stopped_count() {
        let handler = test_handler();
        let raw = r#"{"id":"mo4","method":"stop_all_motors","params":{}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "mo4");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["stopped"], 2); // default config has 2 motors
    }

    #[tokio::test]
    async fn get_motor_status_empty_returns_no_leases() {
        let handler = test_handler();
        let raw = r#"{"id":"mo5","method":"get_motor_status","params":{}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "mo5");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["active_leases"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn get_motor_status_after_set_returns_lease() {
        let handler = test_handler();
        let set_raw =
            r#"{"id":"mo1","method":"set_motor_speed","params":{"channel":1,"speed_pct":75.0}}"#;
        handler.dispatch(set_raw, 99).await;
        let raw = r#"{"id":"mo6","method":"get_motor_status","params":{}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], true);
        let leases = resp["result"]["active_leases"].as_array().unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0]["channel"], 1);
        assert_eq!(leases[0]["conn_id"], 99);
    }

    // ------------------------------------------------------------------
    // Convenience / coordinated method tests
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn drive_sets_both_motors_and_returns_speed_and_count() {
        let handler = test_handler();
        let raw = r#"{"id":"d1","method":"drive","params":{"speed_pct":60.0}}"#;
        let resp_str = handler.dispatch(raw, 1).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "d1");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["speed_pct"], 60.0_f64);
        assert_eq!(resp["result"]["motors"], 2); // default config has 2 motors
    }

    #[tokio::test]
    async fn drive_creates_leases_for_all_motors() {
        let handler = test_handler();
        handler
            .dispatch(
                r#"{"id":"d1","method":"drive","params":{"speed_pct":30.0}}"#,
                7,
            )
            .await;
        let status_str = handler
            .dispatch(r#"{"id":"d2","method":"get_motor_status","params":{}}"#, 0)
            .await;
        let status: serde_json::Value = serde_json::from_str(&status_str).unwrap();
        let leases = status["result"]["active_leases"].as_array().unwrap();
        assert_eq!(leases.len(), 2);
    }

    #[tokio::test]
    async fn drive_invalid_speed_returns_invalid_params() {
        let handler = test_handler();
        let raw = r#"{"id":"d3","method":"drive","params":{"speed_pct":200.0}}"#;
        let resp_str = handler.dispatch(raw, 1).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[tokio::test]
    async fn steer_sets_steering_servo_by_name() {
        let handler = test_handler();
        // Default config: steering = channel 2
        let raw = r#"{"id":"s1","method":"steer","params":{"angle_deg":90.0}}"#;
        let resp_str = handler.dispatch(raw, 1).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "s1");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["servo"], "steering");
        assert_eq!(resp["result"]["channel"], 2);
        assert_eq!(resp["result"]["angle_deg"], 90.0_f64);
        assert_eq!(resp["result"]["pulse_us"], 1500);
    }

    #[tokio::test]
    async fn steer_invalid_angle_returns_invalid_params() {
        let handler = test_handler();
        let raw = r#"{"id":"s2","method":"steer","params":{"angle_deg":200.0}}"#;
        let resp_str = handler.dispatch(raw, 1).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[tokio::test]
    async fn pan_camera_sets_pan_servo_by_name() {
        let handler = test_handler();
        // Default config: camera_pan = channel 0
        let raw = r#"{"id":"p1","method":"pan_camera","params":{"angle_deg":45.0}}"#;
        let resp_str = handler.dispatch(raw, 1).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["servo"], "camera_pan");
        assert_eq!(resp["result"]["channel"], 0);
        assert_eq!(resp["result"]["angle_deg"], 45.0_f64);
    }

    #[tokio::test]
    async fn tilt_camera_sets_tilt_servo_by_name() {
        let handler = test_handler();
        // Default config: camera_tilt = channel 1
        let raw = r#"{"id":"t1","method":"tilt_camera","params":{"angle_deg":120.0}}"#;
        let resp_str = handler.dispatch(raw, 1).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["servo"], "camera_tilt");
        assert_eq!(resp["result"]["channel"], 1);
        assert_eq!(resp["result"]["angle_deg"], 120.0_f64);
    }

    #[tokio::test]
    async fn read_grayscale_returns_three_channel_values() {
        // raw bytes 0x00, 0x2A → u16 = 42 (returned for every ADC read)
        let handler = test_handler_with_adc(0x00, 0x2A);
        let raw = r#"{"id":"gs1","method":"read_grayscale","params":{}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "gs1");
        assert_eq!(resp["ok"], true);
        // Default grayscale channels are [0, 1, 2]
        assert_eq!(resp["result"]["channels"], serde_json::json!([0, 1, 2]));
        assert_eq!(resp["result"]["values"], serde_json::json!([42, 42, 42]));
    }

    #[tokio::test]
    async fn drive_missing_speed_returns_invalid_params() {
        let handler = test_handler();
        let raw = r#"{"id":"d4","method":"drive","params":{}}"#;
        let resp_str = handler.dispatch(raw, 1).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[tokio::test]
    async fn steer_missing_angle_returns_invalid_params() {
        let handler = test_handler();
        let raw = r#"{"id":"s3","method":"steer","params":{}}"#;
        let resp_str = handler.dispatch(raw, 1).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    // ------------------------------------------------------------------
    // Speaker
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn enable_speaker_returns_enabled_true() {
        let handler = test_handler();
        let raw = r#"{"id":"sp1","method":"enable_speaker","params":{}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "sp1");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["enabled"], true);
        // Default speaker_en_pin_bcm is 20.
        assert_eq!(resp["result"]["pin_bcm"], 20);
    }

    #[tokio::test]
    async fn disable_speaker_returns_enabled_false() {
        let handler = test_handler();
        let raw = r#"{"id":"sp2","method":"disable_speaker","params":{}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "sp2");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["enabled"], false);
        assert_eq!(resp["result"]["pin_bcm"], 20);
    }

    #[tokio::test]
    async fn enable_then_disable_speaker_toggles_pin() {
        let handler = test_handler();
        handler
            .dispatch(r#"{"id":"sp3","method":"enable_speaker","params":{}}"#, 0)
            .await;
        // After enable, pin 20 must be high (read via gpio mock).
        let pin_high = handler.gpio.bus.lock().await.read_pin(20).unwrap();
        assert!(pin_high, "BCM 20 should be HIGH after enable_speaker");

        handler
            .dispatch(r#"{"id":"sp4","method":"disable_speaker","params":{}}"#, 0)
            .await;
        let pin_high = handler.gpio.bus.lock().await.read_pin(20).unwrap();
        assert!(!pin_high, "BCM 20 should be LOW after disable_speaker");
    }
}
