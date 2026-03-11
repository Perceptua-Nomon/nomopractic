use std::sync::Arc;

use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use nomopractic::config::Config;
use nomopractic::hat::gpio::{HatGpio, RppalGpio};
use nomopractic::hat::i2c::{Hat, RppalI2c};
use nomopractic::hat::pwm;

/// nomopractic — low-latency HAT hardware daemon for the nomon fleet.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(short, long, default_value = "/etc/nomopractic/config.toml")]
    config: std::path::PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let config = Config::load(&cli.config)?;

    // Init tracing with level from config, overridable by RUST_LOG.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level)),
        )
        .init();

    info!(
        version = env!("CARGO_PKG_VERSION"),
        config_path = %cli.config.display(),
        i2c_bus = config.i2c_bus,
        hat_address = format!("0x{:02x}", config.hat_address),
        "nomopractic starting"
    );

    let hat = Arc::new(Hat::new(
        RppalI2c::open(config.i2c_bus).map_err(anyhow::Error::new)?,
        config.hat_address,
    ));

    pwm::init_pwm(&hat, pwm::SERVO_FREQ)
        .await
        .map_err(|e| anyhow::anyhow!("PWM init failed: {e}"))?;

    info!("PWM initialized at {} Hz", pwm::SERVO_FREQ);

    if !config.motors.is_empty() {
        pwm::init_motor_pwm(&hat, pwm::MOTOR_FREQ)
            .await
            .map_err(|e| anyhow::anyhow!("Motor PWM init failed: {e}"))?;
        info!(
            "Motor PWM initialized at {} Hz ({} motor(s) configured)",
            pwm::MOTOR_FREQ,
            config.motors.len()
        );
    }

    let gpio = Arc::new(HatGpio::new(
        RppalGpio::open().map_err(|e| anyhow::anyhow!("GPIO init failed: {e}"))?,
    ));

    info!("GPIO initialized");

    let config = Arc::new(config);

    // Shutdown signal — set to true on ctrl-c.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Spawn ctrl-c handler.
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for ctrl-c");
        info!("shutdown signal received");
        let _ = shutdown_tx.send(true);
    });

    nomopractic::ipc::serve(config, hat, gpio, shutdown_rx).await
}
