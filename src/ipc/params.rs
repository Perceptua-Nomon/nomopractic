// Typed parameter extraction from IPC requests.
//
// `ParamExtractor` wraps a `&Request` and provides ergonomic methods that
// validate, range-check, and convert JSON params in one step.  On failure
// they return an already-formatted `Response::err(...)` so handlers can
// simply use `?` or `match`.

use super::schema::{Request, Response};

/// Helper for extracting and validating IPC request parameters.
pub struct ParamExtractor<'a> {
    request: &'a Request,
}

impl<'a> ParamExtractor<'a> {
    pub fn new(request: &'a Request) -> Self {
        Self { request }
    }

    /// Extract a required `u64` param in `[min, max]`, returning it as `u8`.
    pub fn required_u64_as_u8(
        &self,
        key: &str,
        min: u64,
        max: u64,
        msg: &str,
    ) -> Result<u8, Response> {
        match self.request.params.get(key).and_then(|v| v.as_u64()) {
            Some(v) if (min..=max).contains(&v) => Ok(v as u8),
            Some(_) | None => Err(Response::err(
                self.request.id.clone(),
                "INVALID_PARAMS",
                msg,
            )),
        }
    }

    /// Extract a required `f64` param in `[min, max]`.
    pub fn required_f64(&self, key: &str, min: f64, max: f64, msg: &str) -> Result<f64, Response> {
        match self.request.params.get(key).and_then(|v| v.as_f64()) {
            Some(v) if (min..=max).contains(&v) => Ok(v),
            Some(_) | None => Err(Response::err(
                self.request.id.clone(),
                "INVALID_PARAMS",
                msg,
            )),
        }
    }

    /// Extract a required string param.
    pub fn required_str(&self, key: &str, msg: &str) -> Result<String, Response> {
        match self.request.params.get(key).and_then(|v| v.as_str()) {
            Some(s) => Ok(s.to_owned()),
            None => Err(Response::err(
                self.request.id.clone(),
                "INVALID_PARAMS",
                msg,
            )),
        }
    }

    /// Extract a required bool param.
    pub fn required_bool(&self, key: &str, msg: &str) -> Result<bool, Response> {
        match self.request.params.get(key).and_then(|v| v.as_bool()) {
            Some(b) => Ok(b),
            None => Err(Response::err(
                self.request.id.clone(),
                "INVALID_PARAMS",
                msg,
            )),
        }
    }

    /// Extract an optional `f64` param, validating range if present.
    pub fn optional_f64(
        &self,
        key: &str,
        min: f64,
        max: f64,
        msg: &str,
    ) -> Result<Option<f64>, Response> {
        match self.request.params.get(key).and_then(|v| v.as_f64()) {
            Some(v) if (min..=max).contains(&v) => Ok(Some(v)),
            Some(_) => Err(Response::err(
                self.request.id.clone(),
                "INVALID_PARAMS",
                msg,
            )),
            None => Ok(None),
        }
    }

    /// Extract an optional `u64` param (no range check).
    pub fn optional_u64(&self, key: &str) -> Option<u64> {
        self.request.params.get(key).and_then(|v| v.as_u64())
    }

    /// Extract an optional bool param.
    pub fn optional_bool(&self, key: &str) -> Option<bool> {
        self.request.params.get(key).and_then(|v| v.as_bool())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(params: serde_json::Value) -> Request {
        Request {
            id: "test".into(),
            method: "test".into(),
            params,
        }
    }

    #[test]
    fn required_u64_as_u8_in_range() {
        let req = make_request(serde_json::json!({ "channel": 5 }));
        let ext = ParamExtractor::new(&req);
        assert_eq!(ext.required_u64_as_u8("channel", 0, 7, "fail").unwrap(), 5);
    }

    #[test]
    fn required_u64_as_u8_out_of_range() {
        let req = make_request(serde_json::json!({ "channel": 8 }));
        let ext = ParamExtractor::new(&req);
        assert!(ext.required_u64_as_u8("channel", 0, 7, "fail").is_err());
    }

    #[test]
    fn required_u64_as_u8_missing() {
        let req = make_request(serde_json::json!({}));
        let ext = ParamExtractor::new(&req);
        assert!(ext.required_u64_as_u8("channel", 0, 7, "fail").is_err());
    }

    #[test]
    fn required_f64_in_range() {
        let req = make_request(serde_json::json!({ "angle": 90.0 }));
        let ext = ParamExtractor::new(&req);
        assert_eq!(ext.required_f64("angle", 0.0, 180.0, "fail").unwrap(), 90.0);
    }

    #[test]
    fn required_f64_out_of_range() {
        let req = make_request(serde_json::json!({ "angle": 200.0 }));
        let ext = ParamExtractor::new(&req);
        assert!(ext.required_f64("angle", 0.0, 180.0, "fail").is_err());
    }

    #[test]
    fn required_str_present() {
        let req = make_request(serde_json::json!({ "name": "explore" }));
        let ext = ParamExtractor::new(&req);
        assert_eq!(ext.required_str("name", "fail").unwrap(), "explore");
    }

    #[test]
    fn required_str_missing() {
        let req = make_request(serde_json::json!({}));
        let ext = ParamExtractor::new(&req);
        assert!(ext.required_str("name", "fail").is_err());
    }

    #[test]
    fn required_bool_present() {
        let req = make_request(serde_json::json!({ "high": true }));
        let ext = ParamExtractor::new(&req);
        assert!(ext.required_bool("high", "fail").unwrap());
    }

    #[test]
    fn optional_f64_present_in_range() {
        let req = make_request(serde_json::json!({ "speed_pct": 50.0 }));
        let ext = ParamExtractor::new(&req);
        assert_eq!(
            ext.optional_f64("speed_pct", 1.0, 100.0, "fail").unwrap(),
            Some(50.0)
        );
    }

    #[test]
    fn optional_f64_present_out_of_range() {
        let req = make_request(serde_json::json!({ "speed_pct": 200.0 }));
        let ext = ParamExtractor::new(&req);
        assert!(ext.optional_f64("speed_pct", 1.0, 100.0, "fail").is_err());
    }

    #[test]
    fn optional_f64_absent() {
        let req = make_request(serde_json::json!({}));
        let ext = ParamExtractor::new(&req);
        assert_eq!(
            ext.optional_f64("speed_pct", 1.0, 100.0, "fail").unwrap(),
            None
        );
    }

    #[test]
    fn optional_u64_present() {
        let req = make_request(serde_json::json!({ "ttl": 500 }));
        let ext = ParamExtractor::new(&req);
        assert_eq!(ext.optional_u64("ttl"), Some(500));
    }

    #[test]
    fn optional_u64_absent() {
        let req = make_request(serde_json::json!({}));
        let ext = ParamExtractor::new(&req);
        assert_eq!(ext.optional_u64("ttl"), None);
    }

    #[test]
    fn optional_bool_present() {
        let req = make_request(serde_json::json!({ "reversed": true }));
        let ext = ParamExtractor::new(&req);
        assert_eq!(ext.optional_bool("reversed"), Some(true));
    }

    #[test]
    fn optional_bool_absent() {
        let req = make_request(serde_json::json!({}));
        let ext = ParamExtractor::new(&req);
        assert_eq!(ext.optional_bool("reversed"), None);
    }
}
