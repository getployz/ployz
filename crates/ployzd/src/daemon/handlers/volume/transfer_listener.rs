use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ployz_runtime_api::RuntimeHandle;
use ployz_runtime_backends::storage::{TokioShellRunner, ZfsDriver};
use ployz_store_api::{MachineStore, StoreDriver};
use ployz_types::model::MachineId;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CONCURRENT_TRANSFERS: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ZfsTransferOpen {
    pub namespace: String,
    pub volume: String,
    pub snapshot: String,
    pub expected_guid: u64,
    /// Identifier the source daemon claims for itself. The receiver validates
    /// it against the remote overlay address before accepting the stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_machine_id: Option<MachineId>,
    /// Set when the source is sending an incremental stream. The receiver
    /// requires the named base snapshot to already exist on the target with
    /// the matching GUID before piping into `zfs recv`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_snapshot_guid: Option<u64>,
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
                    let store = store.clone();
                    let semaphore = Arc::clone(&semaphore);
                    tokio::spawn(async move {
                        let permit = match semaphore.acquire_owned().await {
                            Ok(permit) => permit,
                            Err(error) => {
                                tracing::warn!(?error, %remote_addr, "zfs transfer semaphore closed");
                                return;
                            }
                        };
                        if let Err(error) = handle_transfer(stream, zfs_root, overcommit_ratio, store, remote_addr).await {
                            tracing::warn!(%error, %remote_addr, "zfs transfer failed");
                        }
                        drop(permit);
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
    store: StoreDriver,
    remote_addr: SocketAddr,
) -> Result<(), String> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let header_read = tokio::time::timeout(HEADER_READ_TIMEOUT, reader.read_line(&mut line))
        .await
        .map_err(|_| format!("timed out waiting for zfs transfer header from {remote_addr}"))?;
    header_read.map_err(|error| format!("read zfs transfer header: {error}"))?;
    let open: ZfsTransferOpen =
        serde_json::from_str(&line).map_err(|error| format!("decode transfer header: {error}"))?;
    if let Some(source) = open.source_machine_id.as_ref() {
        validate_source_overlay(&store, source, remote_addr).await?;
        tracing::info!(
            %remote_addr,
            source_machine_id = %source.0,
            namespace = %open.namespace,
            volume = %open.volume,
            snapshot = %open.snapshot,
            "zfs transfer accepted",
        );
    }

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

async fn validate_source_overlay(
    store: &StoreDriver,
    source: &MachineId,
    remote_addr: SocketAddr,
) -> Result<(), String> {
    let machines = store
        .list_machines()
        .await
        .map_err(|error| format!("list machines for zfs transfer source validation: {error}"))?;
    let source_machine = machines
        .into_iter()
        .find(|machine| machine.id == *source)
        .ok_or_else(|| format!("source machine '{source}' not found"))?;
    let expected = IpAddr::V6(source_machine.overlay_ip.0);
    if remote_addr.ip() != expected {
        return Err(format!(
            "zfs transfer source '{}' connected from {}, expected {}",
            source,
            remote_addr.ip(),
            expected
        ));
    }
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

    // Idempotency: if a previous attempt already landed this snapshot with
    // the right GUID, drain the source stream and return success without
    // touching ZFS.
    if driver
        .snapshot_exists(&dataset, &open.snapshot)
        .await
        .map_err(|error| error.to_string())?
    {
        let guid = driver
            .snapshot_guid(&dataset, &open.snapshot)
            .await
            .map_err(|error| error.to_string())?;
        if guid == open.expected_guid {
            // Drain whatever the source already started to write so the source
            // process exits cleanly instead of breaking on EPIPE.
            let _ = tokio::io::copy(reader, &mut tokio::io::sink()).await;
            return Ok(guid);
        }
        return Err(format!(
            "snapshot '{}@{}' already exists on target with guid {guid}, source claims {}",
            dataset, open.snapshot, open.expected_guid
        ));
    }

    // For incrementals, refuse to recv unless the named base snapshot is
    // already on disk with the GUID the source claims it had. Catches the
    // "wrong base" footgun that `zfs recv` would otherwise surface as a
    // confusing checksum/lineage error mid-stream.
    if let Some(from_snapshot) = open.from_snapshot.as_deref() {
        if !driver
            .snapshot_exists(&dataset, from_snapshot)
            .await
            .map_err(|error| error.to_string())?
        {
            return Err(format!(
                "incremental base snapshot '{dataset}@{from_snapshot}' missing on target"
            ));
        }
        if let Some(expected_from_guid) = open.from_snapshot_guid {
            let actual = driver
                .snapshot_guid(&dataset, from_snapshot)
                .await
                .map_err(|error| error.to_string())?;
            if actual != expected_from_guid {
                return Err(format!(
                    "incremental base snapshot '{dataset}@{from_snapshot}' guid {actual} did not match source {expected_from_guid}",
                ));
            }
        }
    } else {
        // Full sends require the parent namespace dataset to exist; `zfs recv`
        // does not auto-create ancestors.
        driver
            .ensure_parent_dataset(&dataset)
            .await
            .map_err(|error| error.to_string())?;
    }

    let mut recv = driver
        .spawn_recv(&dataset)
        .map_err(|error| error.to_string())?;
    let Some(mut stdin) = recv.stdin.take() else {
        return Err("zfs recv stdin was not piped".to_string());
    };
    let copy_result = tokio::io::copy(reader, &mut stdin).await;
    drop(stdin);
    let output = recv
        .wait_with_output()
        .await
        .map_err(|error| format!("wait for zfs recv: {error}"))?;
    if let Err(error) = copy_result {
        cleanup_partial(&driver, &dataset, &open.snapshot).await;
        return Err(format!("copy stream into zfs recv: {error}"));
    }
    if !output.status.success() {
        cleanup_partial(&driver, &dataset, &open.snapshot).await;
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

/// Best-effort cleanup of a partial snapshot left behind by a failed `zfs
/// recv`. The dataset itself is intentionally not destroyed; an operator may
/// want to inspect it before retrying.
async fn cleanup_partial(driver: &ZfsDriver<TokioShellRunner>, dataset: &str, snapshot: &str) {
    if let Err(error) = driver.destroy_snapshot(dataset, snapshot).await {
        tracing::warn!(
            %error,
            dataset,
            snapshot,
            "failed to clean up partial snapshot after recv failure"
        );
    }
}
