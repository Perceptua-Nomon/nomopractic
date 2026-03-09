// Request dispatch — routes method names to HAT driver functions.

use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use tracing::{debug, warn};

use super::schema::{Request, Response};
use crate::config::Config;
use crate::hat::battery;
use crate::hat::i2c::Hat;

/// Processes incoming IPC requests and returns serialized JSON responses.
pub struct Handler {
    config: Arc<Config>,
    start_time: Instant,
    hat: Arc<Hat>,
}

impl Handler {
    pub fn new(config: Arc<Config>, hat: Arc<Hat>) -> Self {
        Self {
            config,
            start_time: Instant::now(),
            hat,
        }
    }

    /// Parse a raw JSON line, dispatch the method, and return a JSON response string.
    pub async fn dispatch(&self, raw: &str) -> String {
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
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn test_handler() -> Handler {
        let hat = Arc::new(Hat::new(MockI2c { response: [0, 0] }, 0x14));
        Handler::new(Arc::new(Config::default()), hat)
    }

    fn test_handler_with_adc(hi: u8, lo: u8) -> Handler {
        let hat = Arc::new(Hat::new(MockI2c { response: [hi, lo] }, 0x14));
        Handler::new(Arc::new(Config::default()), hat)
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let handler = test_handler();
        let raw = r#"{"id":"1","method":"health","params":{}}"#;
        let resp_str = handler.dispatch(raw).await;
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
        let resp_str = handler.dispatch(raw).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "2");
        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "UNKNOWN_METHOD");
    }

    #[tokio::test]
    async fn malformed_json_returns_error() {
        let handler = test_handler();
        let raw = "this is not json";
        let resp_str = handler.dispatch(raw).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[tokio::test]
    async fn missing_method_field_returns_error() {
        let handler = test_handler();
        let raw = r#"{"id":"3"}"#;
        let resp_str = handler.dispatch(raw).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[tokio::test]
    async fn params_default_to_null() {
        let handler = test_handler();
        // Valid request without params field — should still dispatch.
        let raw = r#"{"id":"4","method":"health"}"#;
        let resp_str = handler.dispatch(raw).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["status"], "ok");
    }

    #[tokio::test]
    async fn get_battery_voltage_returns_scaled_voltage() {
        // raw = 0x0001 = 1 → voltage_v = 1 × 3.0 = 3.0
        let handler = test_handler_with_adc(0x00, 0x01);
        let raw = r#"{"id":"5","method":"get_battery_voltage","params":{}}"#;
        let resp_str = handler.dispatch(raw).await;
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "5");
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["voltage_v"], 3.0_f64);
    }
}
