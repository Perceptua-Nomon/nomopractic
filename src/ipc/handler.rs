// Request dispatch — routes method names to HAT driver functions.

use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use tracing::{debug, warn};

use super::schema::{Request, Response};
use crate::config::Config;

/// Processes incoming IPC requests and returns serialized JSON responses.
pub struct Handler {
    config: Arc<Config>,
    start_time: Instant,
}

impl Handler {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            start_time: Instant::now(),
        }
    }

    /// Parse a raw JSON line, dispatch the method, and return a JSON response string.
    pub fn dispatch(&self, raw: &str) -> String {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_handler() -> Handler {
        Handler::new(Arc::new(Config::default()))
    }

    #[test]
    fn health_returns_ok() {
        let handler = test_handler();
        let raw = r#"{"id":"1","method":"health","params":{}}"#;
        let resp_str = handler.dispatch(raw);
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

    #[test]
    fn unknown_method_returns_error() {
        let handler = test_handler();
        let raw = r#"{"id":"2","method":"explode","params":{}}"#;
        let resp_str = handler.dispatch(raw);
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["id"], "2");
        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "UNKNOWN_METHOD");
    }

    #[test]
    fn malformed_json_returns_error() {
        let handler = test_handler();
        let raw = "this is not json";
        let resp_str = handler.dispatch(raw);
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[test]
    fn missing_method_field_returns_error() {
        let handler = test_handler();
        let raw = r#"{"id":"3"}"#;
        let resp_str = handler.dispatch(raw);
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["code"], "INVALID_PARAMS");
    }

    #[test]
    fn params_default_to_null() {
        let handler = test_handler();
        // Valid request without params field — should still dispatch.
        let raw = r#"{"id":"4","method":"health"}"#;
        let resp_str = handler.dispatch(raw);
        let resp: serde_json::Value = serde_json::from_str(&resp_str).unwrap();

        assert_eq!(resp["ok"], true);
        assert_eq!(resp["result"]["status"], "ok");
    }
}
