use std::net::SocketAddr;
use std::path::PathBuf;

use ployz_runtime_api::RuntimeHandle;
use ployz_runtime_backends::storage::{TokioShellRunner, ZfsDriver};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ZfsTransferOpen {
    pub namespace: String,
    pub volume: String,
    pub snapshot: String,
    pub expected_guid: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ZfsTransferReceived {
    pub ok: bool,
    pub snapshot_guid: Option<u64>,
    pub message: String,
}

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
) -> Result<ZfsTransferListenerHandle, String> {
    let listener = TcpListener::bind(bind_addr)
        .await
        .map_err(|error| format!("bind zfs transfer listener {bind_addr}: {error}"))?;
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = task_cancel.cancelled() => break,
                accepted = listener.accept() => {
                    let (stream, remote_addr) = match accepted {
                        Ok(value) => value,
                        Err(error) => {
                            tracing::warn!(?error, "zfs transfer listener accept failed");
                            continue;
                        }
                    };
                    let zfs_root = zfs_root.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_transfer(stream, zfs_root, overcommit_ratio).await {
                            tracing::warn!(%error, %remote_addr, "zfs transfer failed");
                        }
                    });
                }
            }
        }
    });
    Ok(ZfsTransferListenerHandle { cancel, task })
}

async fn handle_transfer(
    stream: TcpStream,
    zfs_root: PathBuf,
    overcommit_ratio: f64,
) -> Result<(), String> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|error| format!("read zfs transfer header: {error}"))?;
    let open: ZfsTransferOpen =
        serde_json::from_str(&line).map_err(|error| format!("decode transfer header: {error}"))?;

    let result = receive_stream(&mut reader, &zfs_root, overcommit_ratio, &open).await;
    let response = match result {
        Ok(guid) if guid == open.expected_guid => ZfsTransferReceived {
            ok: true,
            snapshot_guid: Some(guid),
            message: "received".into(),
        },
        Ok(guid) => ZfsTransferReceived {
            ok: false,
            snapshot_guid: Some(guid),
            message: format!(
                "received snapshot guid {guid}, expected {}",
                open.expected_guid
            ),
        },
        Err(error) => ZfsTransferReceived {
            ok: false,
            snapshot_guid: None,
            message: error,
        },
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

async fn receive_stream<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    zfs_root: &PathBuf,
    overcommit_ratio: f64,
    open: &ZfsTransferOpen,
) -> Result<u64, String> {
    let root = zfs_root
        .to_str()
        .ok_or_else(|| format!("zfs_root is not valid UTF-8: {}", zfs_root.display()))?;
    let driver = ZfsDriver::new(TokioShellRunner, root, overcommit_ratio)
        .await
        .map_err(|error| error.to_string())?;
    let dataset = format!("{root}/{}/{}", open.namespace, open.volume);
    let mut recv = driver
        .spawn_recv(&dataset)
        .map_err(|error| error.to_string())?;
    let Some(mut stdin) = recv.stdin.take() else {
        return Err("zfs recv stdin was not piped".to_string());
    };
    tokio::io::copy(reader, &mut stdin)
        .await
        .map_err(|error| format!("copy stream into zfs recv: {error}"))?;
    drop(stdin);
    let output = recv
        .wait_with_output()
        .await
        .map_err(|error| format!("wait for zfs recv: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "zfs recv failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    driver
        .snapshot_guid(&dataset, &open.snapshot)
        .await
        .map_err(|error| error.to_string())
}
