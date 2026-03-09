// Unix domain socket listener — spawns a tokio task per client connection.

pub mod handler;
pub mod schema;

use std::path::Path;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::hat::i2c::Hat;
use handler::Handler;

/// Maximum NDJSON message size (bytes).
const MAX_MESSAGE_LEN: usize = 4096;

/// Start the IPC listener. Runs until the `shutdown` signal resolves.
pub async fn serve(
    config: Arc<Config>,
    hat: Arc<Hat>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let sock_path = &config.socket_path;

    // Remove stale socket file if it exists.
    if sock_path.exists() {
        std::fs::remove_file(sock_path)?;
    }

    // Ensure parent directory exists.
    if let Some(parent) = sock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(sock_path)?;
    set_socket_permissions(sock_path, config.socket_mode)?;
    info!(path = %sock_path.display(), "IPC listener started");

    let handler = Arc::new(Handler::new(Arc::clone(&config), Arc::clone(&hat)));

    loop {
        let mut shutdown_for_select = shutdown.clone();
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _addr)) => {
                        let handler = Arc::clone(&handler);
                        let mut shutdown_rx = shutdown.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_client(stream, handler, &mut shutdown_rx).await {
                                warn!(error = %e, "client session error");
                            }
                        });
                    }
                    Err(e) => {
                        error!(error = %e, "failed to accept connection");
                    }
                }
            }
            _ = shutdown_changed(&mut shutdown_for_select) => {
                info!("IPC listener shutting down");
                break;
            }
        }
    }

    // Clean up socket file.
    let _ = std::fs::remove_file(sock_path);
    Ok(())
}

async fn shutdown_changed(rx: &mut tokio::sync::watch::Receiver<bool>) {
    // Wait until the value becomes true.
    while !*rx.borrow_and_update() {
        if rx.changed().await.is_err() {
            return;
        }
    }
}

async fn handle_client(
    stream: tokio::net::UnixStream,
    handler: Arc<Handler>,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader);
    let mut buf = String::new();

    info!("client connected");

    loop {
        buf.clear();
        tokio::select! {
            result = lines.read_line(&mut buf) => {
                match result {
                    Ok(0) => {
                        info!("client disconnected");
                        break;
                    }
                    Ok(n) if n > MAX_MESSAGE_LEN => {
                        warn!(bytes = n, "message exceeds max size, dropping");
                        continue;
                    }
                    Ok(_) => {
                        let response_json = handler.dispatch(buf.trim_end()).await;
                        writer.write_all(response_json.as_bytes()).await?;
                        writer.write_all(b"\n").await?;
                        writer.flush().await?;
                    }
                    Err(e) => {
                        warn!(error = %e, "read error");
                        break;
                    }
                }
            }
            _ = shutdown_changed(shutdown) => {
                info!("client session interrupted by shutdown");
                break;
            }
        }
    }

    Ok(())
}

#[cfg(unix)]
fn set_socket_permissions(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}
