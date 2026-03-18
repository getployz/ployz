use ployz_api::{DaemonRequest, DaemonResponse};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// A command received from a client, paired with a channel to send the response back.
pub struct IncomingCommand {
    pub request: DaemonRequest,
    pub reply: oneshot::Sender<DaemonResponse>,
}

/// Listen on a Unix socket and forward incoming requests as IncomingCommand.
pub async fn serve(
    socket_path: &str,
    tx: mpsc::Sender<IncomingCommand>,
    cancel: CancellationToken,
) -> std::io::Result<()> {
    // Remove stale socket file if it exists.
    let _ = std::fs::remove_file(socket_path);
    if let Some(parent) = std::path::Path::new(socket_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(socket_path)?;
    info!(path = socket_path, "daemon socket listening");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("socket listener shutting down");
                break;
            }
            accept = listener.accept() => {
                let (stream, _) = accept?;
                let tx = tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, tx).await {
                        warn!(?e, "client connection error");
                    }
                });
            }
        }
    }

    let _ = std::fs::remove_file(socket_path);
    Ok(())
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    tx: mpsc::Sender<IncomingCommand>,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf = BufReader::new(reader);
    let mut line = String::new();
    buf.read_line(&mut line).await?;

    let request: DaemonRequest = serde_json::from_str(&line)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let (reply_tx, reply_rx) = oneshot::channel();
    let cmd = IncomingCommand {
        request,
        reply: reply_tx,
    };

    tx.send(cmd).await.map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "daemon command channel closed",
        )
    })?;

    let response = reply_rx.await.map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::BrokenPipe, "daemon dropped response")
    })?;

    let mut resp_line = serde_json::to_string(&response)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    resp_line.push('\n');
    writer.write_all(resp_line.as_bytes()).await?;

    Ok(())
}
