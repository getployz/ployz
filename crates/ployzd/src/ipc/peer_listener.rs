use std::net::SocketAddr;

use ployz_api::{DaemonRequest, DaemonResponse};
use ployz_runtime_api::RuntimeHandle;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::listener::IncomingCommand;

pub struct PeerListenerHandle {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl PeerListenerHandle {
    #[must_use]
    pub fn noop() -> Self {
        Self {
            cancel: CancellationToken::new(),
            task: tokio::spawn(async {}),
        }
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.task.await;
    }
}

#[async_trait::async_trait]
impl RuntimeHandle for PeerListenerHandle {
    async fn shutdown(self: Box<Self>) -> std::result::Result<(), String> {
        PeerListenerHandle::shutdown(*self).await;
        Ok(())
    }
}

pub async fn serve(
    bind_addr: SocketAddr,
    tx: mpsc::Sender<IncomingCommand>,
) -> std::io::Result<PeerListenerHandle> {
    let listener = TcpListener::bind(bind_addr).await?;
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        info!(%bind_addr, "peer rpc listener listening");
        loop {
            tokio::select! {
                _ = task_cancel.cancelled() => {
                    info!(%bind_addr, "peer rpc listener shutting down");
                    break;
                }
                accepted = listener.accept() => {
                    let (stream, remote_addr) = match accepted {
                        Ok(value) => value,
                        Err(error) => {
                            warn!(?error, %bind_addr, "peer rpc accept failed");
                            continue;
                        }
                    };
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_connection(stream, tx).await {
                            warn!(?error, %remote_addr, "peer rpc connection failed");
                        }
                    });
                }
            }
        }
    });
    Ok(PeerListenerHandle { cancel, task })
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    tx: mpsc::Sender<IncomingCommand>,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf = BufReader::new(reader);
    let mut line = String::new();
    buf.read_line(&mut line).await?;

    let request: DaemonRequest = serde_json::from_str(&line)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;

    let (reply_tx, reply_rx) = oneshot::channel();
    tx.send(IncomingCommand {
        request,
        reply: reply_tx,
    })
    .await
    .map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "daemon command channel closed",
        )
    })?;

    let response: DaemonResponse = reply_rx.await.map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::BrokenPipe, "daemon dropped response")
    })?;
    let mut response_line = serde_json::to_string(&response)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    response_line.push('\n');
    writer.write_all(response_line.as_bytes()).await?;
    writer.shutdown().await?;
    Ok(())
}
