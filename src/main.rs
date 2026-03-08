use clap::Parser;
use tracing::info;

mod config;
mod hat;
mod ipc;
mod reset;

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
    todo!("Phase 1: parse CLI, load config, init tracing, start IPC listener")
}
