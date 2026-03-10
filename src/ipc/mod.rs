// Unix domain socket listener — spawns a tokio task per client connection.

pub mod handler;
pub mod schema;

use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::hat::gpio::HatGpio;
use crate::hat::i2c::Hat;
use crate::hat::pwm;
use handler::Handler;

/// Maximum NDJSON message size (bytes).
const MAX_MESSAGE_LEN: usize = 4096;

/// Start the IPC listener. Runs until the `shutdown` signal resolves.
pub async fn serve(
    config: Arc<Config>,
    hat: Arc<Hat>,
    gpio: Arc<HatGpio>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let sock_path = &config.socket_path;

    // Remove stale socket file if it exists, but only if it is a Unix socket.
    // symlink_metadata is used so that a symlink at the path is never followed;
    // a symlink (even one pointing to a socket) is rejected to prevent an
    // attacker-controlled path from causing deletion of an arbitrary file.
    match std::fs::symlink_metadata(sock_path) {
        Ok(meta) if meta.file_type().is_socket() => {
            std::fs::remove_file(sock_path)?;
        }
        Ok(_) => {
            anyhow::bail!(
                "socket path {} exists but is not a Unix socket; refusing to remove",
                sock_path.display()
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }

    // Ensure parent directory exists.
    if let Some(parent) = sock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(sock_path)?;
    set_socket_permissions(sock_path, config.socket_mode)?;
    info!(path = %sock_path.display(), "IPC listener started");

    let handler = Arc::new(Handler::new(Arc::clone(&config), Arc::clone(&hat), gpio));

    // Spawn the TTL lease watchdog — idles servo channels when leases expire.
    {
        let watchdog_handler = Arc::clone(&handler);
        let poll_ms = config.watchdog_poll_ms;
        let watchdog_shutdown = shutdown.clone();
        tokio::spawn(watchdog_task(watchdog_handler, poll_ms, watchdog_shutdown));
    }

    let conn_counter = Arc::new(AtomicU64::new(1));

    loop {
        let mut shutdown_for_select = shutdown.clone();
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _addr)) => {
                        let handler = Arc::clone(&handler);
                        let conn_id = conn_counter.fetch_add(1, Ordering::Relaxed);
                        let mut shutdown_rx = shutdown.clone();
                        tokio::spawn(async move {
                            if let Err(e) =
                                handle_client(stream, Arc::clone(&handler), conn_id, &mut shutdown_rx)
                                    .await
                            {
                                warn!(error = %e, "client session error");
                            }
                            handler.on_client_disconnect(conn_id).await;
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
    conn_id: u64,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut buf = String::new();

    info!("client connected");

    loop {
        buf.clear();

        // Bind the bounded reader to a local so the temporary lives long enough
        // for `select!` to poll it.  Allocation is capped at MAX_MESSAGE_LEN + 1
        // bytes per call, so a client sending a very long line cannot force an
        // arbitrarily large heap allocation before the size check runs.
        let mut bounded = (&mut reader).take((MAX_MESSAGE_LEN + 1) as u64);

        let result = tokio::select! {
            r = bounded.read_line(&mut buf) => Some(r),
            _ = shutdown_changed(shutdown) => None,
        };

        // Drop `bounded` now so `reader` is no longer mutably borrowed and can
        // be passed to `drain_line` below.
        drop(bounded);

        match result {
            None => {
                info!("client session interrupted by shutdown");
                break;
            }
            Some(Ok(0)) => {
                info!("client disconnected");
                break;
            }
            Some(Ok(n)) if n > MAX_MESSAGE_LEN && !buf.ends_with('\n') => {
                // take() hit its limit before the newline; the content length
                // exceeds MAX_MESSAGE_LEN (the trailing \n is not included in
                // the limit).  Discard the rest of the line so the next read
                // starts at a clean message boundary.
                warn!(bytes = n, "message exceeds max size, dropping");
                if let Err(e) = drain_line(&mut reader).await {
                    warn!(error = %e, "drain error after oversized message");
                    break;
                }
            }
            Some(Ok(_)) => {
                let response_json = handler.dispatch(buf.trim_end(), conn_id).await;
                writer.write_all(response_json.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
            }
            Some(Err(e)) => {
                warn!(error = %e, "read error");
                break;
            }
        }
    }

    Ok(())
}

/// Discard bytes from `reader` up to and including the next `\n` (or EOF).
///
/// Uses `fill_buf`/`consume` so no additional heap allocation is needed; the
/// discard is done entirely within the `BufReader`'s existing internal buffer.
async fn drain_line<R>(reader: &mut R) -> std::io::Result<()>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            return Ok(()); // EOF
        }
        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            reader.consume(pos + 1);
            return Ok(());
        }
        let n = buf.len();
        reader.consume(n);
    }
}

/// Background task that polls the lease store every `poll_ms` milliseconds and
/// idles any PWM channels whose TTL has expired.
async fn watchdog_task(
    handler: Arc<Handler>,
    poll_ms: u64,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(poll_ms)) => {
                let expired = handler.lease_manager().poll_expired().await;
                for ch in expired {
                    warn!(channel = ch, "servo lease expired; idling channel");
                    if let Err(e) = pwm::set_channel_pulse_us(handler.hat(), ch, 0).await {
                        error!(error = %e, channel = ch, "failed to idle expired servo channel");
                    }
                }
            }
            _ = shutdown_changed(&mut shutdown) => break,
        }
    }
}

#[cfg(unix)]
fn set_socket_permissions(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}
