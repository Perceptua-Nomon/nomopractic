// Integration test: start IPC listener, connect, send requests, verify responses.

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use nomopractic::config::Config;
use nomopractic::hat::gpio::{GpioBus, GpioError, HatGpio};
use nomopractic::hat::i2c::{Hat, HatError, I2cBus};

// ---------------------------------------------------------------------------
// Mock I2C bus
// ---------------------------------------------------------------------------

struct MockI2c {
    /// Fixed 2-byte response returned for every read.
    adc_response: [u8; 2],
}

impl MockI2c {
    fn new(hi: u8, lo: u8) -> Self {
        Self {
            adc_response: [hi, lo],
        }
    }
}

impl I2cBus for MockI2c {
    fn write_bytes(&mut self, _addr: u8, _data: &[u8]) -> Result<(), HatError> {
        Ok(())
    }

    fn read_bytes(&mut self, _addr: u8, buf: &mut [u8]) -> Result<(), HatError> {
        if buf.len() >= 2 {
            buf[0] = self.adc_response[0];
            buf[1] = self.adc_response[1];
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Mock GPIO bus
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Test server helpers
// ---------------------------------------------------------------------------

/// Start the IPC listener on a temporary socket and return the config and
/// shutdown sender. The returned TempDir must be kept alive for the socket.
async fn start_test_server() -> (
    Arc<Config>,
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
    tempfile::TempDir,
) {
    start_test_server_with_adc(0x00, 0x00).await
}

/// Like `start_test_server` but the mock ADC returns `[hi, lo]` for reads.
async fn start_test_server_with_adc(
    hi: u8,
    lo: u8,
) -> (
    Arc<Config>,
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("test.sock");

    let config = Arc::new(Config { socket_path: sock_path, ..Default::default() });

    let hat = Arc::new(Hat::new(MockI2c::new(hi, lo), config.hat_address));
    let gpio = Arc::new(HatGpio::new(MockGpio::new()));

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let cfg = Arc::clone(&config);
    let handle =
        tokio::spawn(async move { nomopractic::ipc::serve(cfg, hat, gpio, shutdown_rx).await });

    // Give the listener a moment to bind.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (config, shutdown_tx, handle, dir)
}

/// Send a raw JSON request and read back the response line.
async fn request(stream: &mut BufReader<UnixStream>, msg: &str) -> serde_json::Value {
    let inner = stream.get_mut();
    inner.write_all(msg.as_bytes()).await.unwrap();
    inner.write_all(b"\n").await.unwrap();
    inner.flush().await.unwrap();

    let mut line = String::new();
    stream.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_check_over_socket() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    let resp = request(&mut reader, r#"{"id":"1","method":"health","params":{}}"#).await;

    assert_eq!(resp["id"], "1");
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["result"]["status"], "ok");
    assert_eq!(resp["result"]["schema_version"], "1.0.0");
    assert!(resp["result"]["uptime_s"].is_number());

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}

#[tokio::test]
async fn unknown_method_over_socket() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    let resp = request(
        &mut reader,
        r#"{"id":"2","method":"nonexistent","params":{}}"#,
    )
    .await;

    assert_eq!(resp["id"], "2");
    assert_eq!(resp["ok"], false);
    assert_eq!(resp["error"]["code"], "UNKNOWN_METHOD");

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}

#[tokio::test]
async fn malformed_json_over_socket() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    let resp = request(&mut reader, "not json at all").await;

    assert_eq!(resp["ok"], false);
    assert_eq!(resp["error"]["code"], "INVALID_PARAMS");

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}

#[tokio::test]
async fn multiple_requests_on_same_connection() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    // First request.
    let resp1 = request(&mut reader, r#"{"id":"a","method":"health","params":{}}"#).await;
    assert_eq!(resp1["id"], "a");
    assert_eq!(resp1["ok"], true);

    // Second request on same connection.
    let resp2 = request(&mut reader, r#"{"id":"b","method":"health","params":{}}"#).await;
    assert_eq!(resp2["id"], "b");
    assert_eq!(resp2["ok"], true);

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}

#[tokio::test]
async fn multiple_concurrent_clients() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let mut handles = vec![];
    for i in 0..3 {
        let path = config.socket_path.clone();
        handles.push(tokio::spawn(async move {
            let stream = UnixStream::connect(&path).await.unwrap();
            let mut reader = BufReader::new(stream);
            let resp = request(
                &mut reader,
                &format!(r#"{{"id":"c{i}","method":"health","params":{{}}}}"#),
            )
            .await;
            assert_eq!(resp["ok"], true);
            assert_eq!(resp["id"], format!("c{i}"));
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let _ = shutdown_tx.send(true);
    let _ = handle.await;
}

#[tokio::test]
async fn serve_rejects_regular_file_at_socket_path() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("not_a_socket.sock");

    // Create a regular file at the socket path.
    std::fs::write(&sock_path, b"regular file content").unwrap();

    let config = Arc::new(Config { socket_path: sock_path, ..Default::default() });

    let hat = Arc::new(nomopractic::hat::i2c::Hat::new(
        MockI2c::new(0, 0),
        config.hat_address,
    ));
    let gpio = Arc::new(HatGpio::new(MockGpio::new()));
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let result = nomopractic::ipc::serve(config, hat, gpio, shutdown_rx).await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("not a Unix socket"),
        "unexpected error message: {msg}"
    );
}

#[tokio::test]
async fn serve_rejects_symlink_at_socket_path() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target_file");
    let sock_path = dir.path().join("link.sock");

    // Create a regular file and a symlink pointing to it.
    std::fs::write(&target, b"target").unwrap();
    std::os::unix::fs::symlink(&target, &sock_path).unwrap();

    let config = Arc::new(Config { socket_path: sock_path, ..Default::default() });

    let hat = Arc::new(nomopractic::hat::i2c::Hat::new(
        MockI2c::new(0, 0),
        config.hat_address,
    ));
    let gpio = Arc::new(HatGpio::new(MockGpio::new()));
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let result = nomopractic::ipc::serve(config, hat, gpio, shutdown_rx).await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("not a Unix socket"),
        "unexpected error message: {msg}"
    );
}

#[tokio::test]
async fn get_battery_voltage_over_socket() {
    // raw = 0x0001 = 1 → voltage_v = 1 × 3.0 = 3.0
    let (config, shutdown_tx, handle, _dir) = start_test_server_with_adc(0x00, 0x01).await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    let resp = request(
        &mut reader,
        r#"{"id":"bat1","method":"get_battery_voltage","params":{}}"#,
    )
    .await;

    assert_eq!(resp["id"], "bat1");
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["result"]["voltage_v"], 3.0_f64);

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}

// ---------------------------------------------------------------------------
// Servo IPC integration tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_servo_pulse_us_over_socket() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    let resp = request(
        &mut reader,
        r#"{"id":"s1","method":"set_servo_pulse_us","params":{"channel":0,"pulse_us":1500}}"#,
    )
    .await;

    assert_eq!(resp["id"], "s1");
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["result"]["channel"], 0);
    assert_eq!(resp["result"]["pulse_us"], 1500);

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}

#[tokio::test]
async fn set_servo_angle_over_socket() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    let resp = request(
        &mut reader,
        r#"{"id":"s2","method":"set_servo_angle","params":{"channel":1,"angle_deg":0.0}}"#,
    )
    .await;

    assert_eq!(resp["id"], "s2");
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["result"]["channel"], 1);
    assert_eq!(resp["result"]["angle_deg"], 0.0_f64);
    assert_eq!(resp["result"]["pulse_us"], 500);

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}

#[tokio::test]
async fn set_servo_angle_180_degrees_over_socket() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    let resp = request(
        &mut reader,
        r#"{"id":"s3","method":"set_servo_angle","params":{"channel":0,"angle_deg":180.0}}"#,
    )
    .await;

    assert_eq!(resp["ok"], true);
    assert_eq!(resp["result"]["pulse_us"], 2500);

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}

#[tokio::test]
async fn set_servo_pulse_us_invalid_channel_returns_invalid_params() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    let resp = request(
        &mut reader,
        r#"{"id":"s4","method":"set_servo_pulse_us","params":{"channel":12,"pulse_us":1500}}"#,
    )
    .await;

    assert_eq!(resp["ok"], false);
    assert_eq!(resp["error"]["code"], "INVALID_PARAMS");

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}

#[tokio::test]
async fn oversized_message_is_dropped_connection_remains_usable() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    // Send a message that exceeds MAX_MESSAGE_LEN (4096 bytes).
    // The server must reject it without echoing a response and without
    // terminating the connection, so the following valid request still works.
    let oversized = "x".repeat(4097);
    {
        let inner = reader.get_mut();
        inner.write_all(oversized.as_bytes()).await.unwrap();
        inner.write_all(b"\n").await.unwrap();
        inner.flush().await.unwrap();
    }

    // A valid request after the oversized one must still succeed.
    let resp = request(
        &mut reader,
        r#"{"id":"after_big","method":"health","params":{}}"#,
    )
    .await;

    assert_eq!(resp["id"], "after_big");
    assert_eq!(resp["ok"], true);

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}

/// MAX_MESSAGE_LEN boundary: a message whose content is exactly 4096 bytes
/// (excluding the framing newline) must be accepted and produce a response.
#[tokio::test]
async fn message_at_max_size_boundary_is_accepted() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    // Build a health request whose total JSON length is exactly 4096 bytes.
    // Base template without the id value: {"id":"","method":"health","params":{}}
    // That is 38 bytes; fill the id field to bring the total to 4096.
    let base = r#"{"id":"","method":"health","params":{}}"#;
    let id_padding = "a".repeat(4096 - base.len());
    let msg = format!(r#"{{"id":"{id_padding}","method":"health","params":{{}}}}"#);
    assert_eq!(msg.len(), 4096, "test message must be exactly 4096 bytes");

    let resp = request(&mut reader, &msg).await;

    // The server must respond (not drop the message).
    assert_eq!(resp["ok"], true);

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}

/// MAX_MESSAGE_LEN boundary: a message whose content is exactly 4097 bytes
/// (one byte over the limit, excluding the framing newline) must be rejected
/// without closing the connection.
#[tokio::test]
async fn message_one_over_max_size_is_rejected() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    // 4097-byte payload — one byte over MAX_MESSAGE_LEN.
    let one_over = "x".repeat(4097);
    {
        let inner = reader.get_mut();
        inner.write_all(one_over.as_bytes()).await.unwrap();
        inner.write_all(b"\n").await.unwrap();
        inner.flush().await.unwrap();
    }

    // The server must not close the connection; a subsequent valid request works.
    let resp = request(
        &mut reader,
        r#"{"id":"after_one_over","method":"health","params":{}}"#,
    )
    .await;

    assert_eq!(resp["id"], "after_one_over");
    assert_eq!(resp["ok"], true);

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}

// ---------------------------------------------------------------------------
// GPIO IPC integration tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reset_mcu_over_socket() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    let resp = request(
        &mut reader,
        r#"{"id":"rst1","method":"reset_mcu","params":{}}"#,
    )
    .await;

    assert_eq!(resp["id"], "rst1");
    assert_eq!(resp["ok"], true);
    assert_eq!(
        resp["result"]["reset_ms"],
        nomopractic::reset::RESET_HOLD_MS
    );

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}

#[tokio::test]
async fn read_gpio_sw_over_socket() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    let resp = request(
        &mut reader,
        r#"{"id":"gpio1","method":"read_gpio","params":{"pin":"SW"}}"#,
    )
    .await;

    assert_eq!(resp["id"], "gpio1");
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["result"]["pin"], "SW");
    assert_eq!(resp["result"]["high"], false);

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}

#[tokio::test]
async fn write_gpio_led_over_socket() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    let resp = request(
        &mut reader,
        r#"{"id":"gpio2","method":"write_gpio","params":{"pin":"LED","high":true}}"#,
    )
    .await;

    assert_eq!(resp["id"], "gpio2");
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["result"]["pin"], "LED");
    assert_eq!(resp["result"]["high"], true);

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}

#[tokio::test]
async fn write_gpio_input_pin_returns_invalid_params_over_socket() {
    let (config, shutdown_tx, handle, _dir) = start_test_server().await;

    let stream = UnixStream::connect(&config.socket_path).await.unwrap();
    let mut reader = BufReader::new(stream);

    let resp = request(
        &mut reader,
        r#"{"id":"gpio3","method":"write_gpio","params":{"pin":"SW","high":false}}"#,
    )
    .await;

    assert_eq!(resp["id"], "gpio3");
    assert_eq!(resp["ok"], false);
    assert_eq!(resp["error"]["code"], "INVALID_PARAMS");

    let _ = shutdown_tx.send(true);
    drop(reader);
    let _ = handle.await;
}
