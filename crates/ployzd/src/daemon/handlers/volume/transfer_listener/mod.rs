mod protocol;
mod receive;
mod validation;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ployz_runtime_api::RuntimeHandle;
use ployz_store_api::StoreDriver;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use protocol::read_transfer_header;
pub(crate) use protocol::{ZfsTransferOpen, ZfsTransferReceived};
use receive::receive_stream;
use validation::validate_open_source;

const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CONCURRENT_TRANSFERS: usize = 4;
const GUID_MISMATCH_MESSAGE: &str = "zfs transfer rejected: snapshot guid mismatch";
const TRANSFER_FAILED_MESSAGE: &str = "zfs transfer failed";

pub(crate) struct ZfsTransferListenerHandle {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl ZfsTransferListenerHandle {
    #[must_use]
    pub(crate) fn noop() -> Self {
        Self {
            cancel: CancellationToken::new(),
            task: tokio::spawn(async {}),
        }
    }

    pub(crate) async fn shutdown(self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

#[async_trait::async_trait]
impl RuntimeHandle for ZfsTransferListenerHandle {
    async fn shutdown(self: Box<Self>) -> std::result::Result<(), String> {
        ZfsTransferListenerHandle::shutdown(*self).await;
        Ok(())
    }
}

pub(crate) async fn serve(
    bind_addr: SocketAddr,
    zfs_root: PathBuf,
    overcommit_ratio: f64,
    store: StoreDriver,
) -> Result<ZfsTransferListenerHandle, String> {
    let listener = TcpListener::bind(bind_addr)
        .await
        .map_err(|error| format!("bind zfs transfer listener {bind_addr}: {error}"))?;
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_TRANSFERS));
    let task = tokio::spawn(async move {
        loop {
            // Reserve a transfer slot before accepting; if all permits are
            // held, the kernel queues new connections instead of letting us
            // pile up unbounded sleeping tasks with open sockets.
            let permit = tokio::select! {
                _ = task_cancel.cancelled() => break,
                permit = Arc::clone(&semaphore).acquire_owned() => match permit {
                    Ok(permit) => permit,
                    Err(error) => {
                        tracing::warn!(?error, "zfs transfer semaphore closed");
                        break;
                    }
                },
            };

            let (stream, remote_addr) = tokio::select! {
                _ = task_cancel.cancelled() => break,
                accepted = listener.accept() => match accepted {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(?error, "zfs transfer listener accept failed");
                        drop(permit);
                        continue;
                    }
                },
            };

            let zfs_root = zfs_root.clone();
            let store = store.clone();
            tokio::spawn(async move {
                if let Err(error) =
                    handle_transfer(stream, zfs_root, overcommit_ratio, store, remote_addr).await
                {
                    tracing::warn!(%error, %remote_addr, "zfs transfer failed");
                }
                drop(permit);
            });
        }
    });
    Ok(ZfsTransferListenerHandle { cancel, task })
}

async fn handle_transfer(
    stream: TcpStream,
    zfs_root: PathBuf,
    overcommit_ratio: f64,
    store: StoreDriver,
    remote_addr: SocketAddr,
) -> Result<(), String> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let line = tokio::time::timeout(HEADER_READ_TIMEOUT, read_transfer_header(&mut reader))
        .await
        .map_err(|_| format!("timed out waiting for zfs transfer header from {remote_addr}"))?;
    let line = line.map_err(|error| format!("read zfs transfer header: {error}"))?;
    let open: ZfsTransferOpen =
        serde_json::from_str(&line).map_err(|error| format!("decode transfer header: {error}"))?;
    let source = validate_open_source(&store, &open, remote_addr)
        .await
        .map_err(|error| error.to_string())?;
    tracing::info!(
        %remote_addr,
        source_machine_id = %source.as_str(),
        namespace = %open.namespace,
        volume = %open.volume,
        snapshot = %open.snapshot,
        "zfs transfer accepted",
    );

    let result = receive_stream(&mut reader, &zfs_root, overcommit_ratio, &open).await;
    let response = match result {
        Ok(guid) if guid == open.expected_guid => ZfsTransferReceived {
            ok: true,
            snapshot_guid: Some(guid),
            message: "received".into(),
        },
        Ok(guid) => {
            // Don't echo the local guid back — that's snapshot metadata the
            // peer doesn't need. The sender already knows what they claimed.
            tracing::warn!(
                received_guid = guid,
                expected_guid = open.expected_guid,
                namespace = %open.namespace,
                volume = %open.volume,
                snapshot = %open.snapshot,
                %remote_addr,
                "zfs transfer guid mismatch",
            );
            ZfsTransferReceived {
                ok: false,
                snapshot_guid: None,
                message: GUID_MISMATCH_MESSAGE.into(),
            }
        }
        Err(error) => {
            tracing::warn!(
                %error,
                %remote_addr,
                namespace = %open.namespace,
                volume = %open.volume,
                snapshot = %open.snapshot,
                "zfs transfer receive failed",
            );
            ZfsTransferReceived {
                ok: false,
                snapshot_guid: None,
                message: TRANSFER_FAILED_MESSAGE.into(),
            }
        }
    };
    let mut body =
        serde_json::to_string(&response).map_err(|error| format!("encode response: {error}"))?;
    body.push('\n');
    writer
        .write_all(body.as_bytes())
        .await
        .map_err(|error| format!("write zfs transfer response: {error}"))?;
    writer
        .shutdown()
        .await
        .map_err(|error| format!("shutdown zfs transfer response: {error}"))?;
    Ok(())
}
