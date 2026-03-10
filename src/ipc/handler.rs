// Request dispatch — routes method names to HAT driver functions.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tracing::{debug, error, warn};

use super::schema::{Request, Response};
use crate::config::Config;
use crate::hat::battery;
use crate::hat::gpio::{self, GpioError, HatGpio};
use crate::hat::i2c::{Hat, HatError};
use crate::hat::servo::LeaseManager;
use crate::hat::{pwm, servo};
use crate::reset;

/// Minimum interval between consecutive MCU reset requests (ms).
const RESET_MIN_INTERVAL_MS: u64 = 1000;

/// Classify a `HatError` into an IPC error code string.
fn hat_error_code(e: &HatError) -> &'static str {
    match e {
        HatError::I2c(_) => "HARDWARE_ERROR",
        HatError::InvalidChannel(_)
        | HatError::InvalidServoChannel(_)
        | HatError::InvalidPulse(_)
        | HatError::InvalidAngle(_) => "INVALID_PARAMS",
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
    gpio: Arc<HatGpio>,
    last_reset_at: tokio::sync::Mutex<Option<Instant>>,
}

impl Handler {
    pub fn new(config: Arc<Config>, hat: Arc<Hat>, gpio: Arc<HatGpio>) -> Self {
        Self {
            config,
            start_time: Instant::now(),
            hat,
            lease_manager: Arc::new(LeaseManager::new()),
            gpio,
            last_reset_at: tokio::sync::Mutex::new(None),
        }
    }

    pub fn lease_manager(&self) -> &Arc<LeaseManager> {
        &self.lease_manager
    }

    pub fn hat(&self) -> &Arc<Hat> {
        &self.hat
    }

    /// Release all servo leases for a disconnected connection and idle any active channels.
    pub async fn on_client_disconnect(&self, conn_id: u64) {
        let channels = self.lease_manager.release_connection(conn_id).await;
        for ch in channels {
            warn!(
                channel = ch,
                conn_id, "client disconnected; idling leased servo channel"
            );
            if let Err(e) = pwm::set_channel_pulse_us(&self.hat, ch, 0).await {
                error!(error = %e, channel = ch, "failed to idle channel on client disconnect");
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
            "set_servo_pulse_us" => self.handle_set_servo_pulse_us(&request, conn_id).await,
            "set_servo_angle" => self.handle_set_servo_angle(&request, conn_id).await,
            "read_gpio" => self.handle_read_gpio(&request).await,
            "write_gpio" => self.handle_write_gpio(&request).await,
            "reset_mcu" => self.handle_reset_mcu(&request).await,
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
            // under `last_reset_at` — the two locks are independent and this
            // avoids holding `last_reset_at` across an async sleep.
            // The lock is re-acquired below only on success; a failed attempt
            // therefore does not block a retry.
            let guard = self.last_reset_at.lock().await;
            if let Some(last) = *guard {
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
                *self.last_reset_at.lock().await = Some(now);
                Response::ok(request.id.clone(), json!({ "reset_ms": result.reset_ms }))
            }
            Err(e) => {
                warn!(error = %e, "reset_mcu failed");
                Response::err(request.id.clone(), "HARDWARE_ERROR", e.to_string())
            }
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
        // raw = 0x0001 = 1 → voltage_v = 1 × 3.0 = 3.0
        let handler = test_handler_with_adc(0x00, 0x01);
        let raw = r#"{"id":"5","method":"get_battery_voltage","params":{}}"#;
        let resp_str = handler.dispatch(raw, 0).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "5");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["voltage_v"], 3.0_f64);
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
}
