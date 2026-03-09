// Integration test: start IPC listener, connect, send requests, verify responses.

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use nomopractic::config::Config;
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

    let mut config = Config::default();
    config.socket_path = sock_path;
    let config = Arc::new(config);

    let hat = Arc::new(Hat::new(MockI2c::new(hi, lo), config.hat_address));

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let cfg = Arc::clone(&config);
    let handle = tokio::spawn(async move { nomopractic::ipc::serve(cfg, hat, shutdown_rx).await });

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

    let mut config = Config::default();
    config.socket_path = sock_path;
    let config = Arc::new(config);

    let hat = Arc::new(nomopractic::hat::i2c::Hat::new(
        MockI2c::new(0, 0),
        config.hat_address,
    ));
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let result = nomopractic::ipc::serve(config, hat, shutdown_rx).await;
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

    let mut config = Config::default();
    config.socket_path = sock_path;
    let config = Arc::new(config);

    let hat = Arc::new(nomopractic::hat::i2c::Hat::new(
        MockI2c::new(0, 0),
        config.hat_address,
    ));
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let result = nomopractic::ipc::serve(config, hat, shutdown_rx).await;
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
