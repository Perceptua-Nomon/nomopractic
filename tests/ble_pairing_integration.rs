#![cfg(feature = "ble")]

// BLE pairing integration test — gated behind NOMON_RUN_BLE_INTEGRATION env var.
//
// This test attempts to start the BLE GATT server (register BlueZ agent,
// advertise service) and then cleanly shut it down. It is intended to run
// only in lab/dev environments where BlueZ and a Bluetooth controller are
// available. To run locally:
//
// NOMON_RUN_BLE_INTEGRATION=1 cargo test --test ble_pairing_integration --features ble

use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;

use tokio::time::timeout;

use nomopractic::ble::BLE_CONN_ID;
use nomopractic::config::{BleConfig, Config as FullConfig};
use nomopractic::hat::audio::AlsaControl;
use nomopractic::hat::gpio::HatGpio;
use nomopractic::hat::i2c::Hat;
use nomopractic::ipc::handler::Handler;

use nomopractic::testing::{MockAlsaControl, MockGpio, MockI2c};

#[tokio::test]
async fn ble_server_registers_agent_and_advertises() {
    if std::env::var("NOMON_RUN_BLE_INTEGRATION").is_err() {
        eprintln!(
            "Skipping BLE integration test; set NOMON_RUN_BLE_INTEGRATION=1 to run"
        );
        return;
    }

    // Create a temporary passkey file.
    let dir = tempdir().expect("tempdir");
    let passfile = dir.path().join("pairing_secret");
    std::fs::write(&passfile, b"123456\n").expect("write passkey");

    // BleConfig pointing at our temp passkey file.
    let mut ble_cfg = BleConfig::default();
    ble_cfg.enabled = true;
    ble_cfg.device_name = "nomon-integration-test".into();
    ble_cfg.pairing_secret_path = passfile.clone();

    // Full Config for Handler; use defaults but with our BleConfig set.
    let full_cfg = FullConfig { ble: ble_cfg.clone(), ..Default::default() };
    let cfg = Arc::new(full_cfg);

    // Create handler with mock dependencies (no real hardware required).
    let hat = Arc::new(Hat::new(MockI2c { response: [0, 0] }, cfg.hat_address));
    let gpio = Arc::new(HatGpio::new(MockGpio::new()));
    let alsa: Arc<dyn AlsaControl> = Arc::new(MockAlsaControl::new(50, 50));

    let handler = Arc::new(Handler::with_alsa(cfg.clone(), hat, gpio, alsa));

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Spawn the BLE server task.
    let ble_cfg_clone = ble_cfg.clone();
    let handler_clone = Arc::clone(&handler);
    let handle = tokio::spawn(async move {
        nomopractic::ble::start_ble_server(&ble_cfg_clone, handler_clone, shutdown_rx).await
    });

    // Give the server a moment to start and register the agent.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Request shutdown and await completion (with timeout).
    let _ = shutdown_tx.send(true);

    let res = timeout(Duration::from_secs(10), handle).await;
    match res {
        Ok(join_res) => match join_res {
            Ok(Ok(())) => {
                // success
            }
            Ok(Err(e)) => panic!("BLE server returned error: {e}"),
            Err(join_err) => panic!("BLE server task join failed: {join_err}"),
        },
        Err(_) => panic!("Timeout waiting for BLE server to shut down"),
    }
}
