use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ployz_runtime_api::RuntimeHandle;
use ployz_runtime_backends::storage::{ShellRunner, TokioShellRunner, ZfsDriver};
use ployz_store_api::{DeployStore, MachineMembershipStore, StoreDriver};
use ployz_types::model::MachineId;
use ployz_types::spec::Namespace;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_CONCURRENT_TRANSFERS: usize = 4;
const GUID_MISMATCH_MESSAGE: &str = "zfs transfer rejected: snapshot guid mismatch";
const TRANSFER_FAILED_MESSAGE: &str = "zfs transfer failed";
const VOLUME_NOT_AUTHORIZED: &str = "zfs transfer not authorized";

#[derive(Debug, thiserror::Error)]
enum ZfsTransferValidationError {
    #[error("connection closed before zfs transfer header")]
    HeaderClosed,
    #[error("zfs transfer header exceeded {max_bytes} bytes")]
    HeaderTooLarge { max_bytes: usize },
    #[error("zfs transfer header missing newline")]
    HeaderMissingNewline,
    #[error("zfs transfer header was not UTF-8: {message}")]
    HeaderNotUtf8 { message: String },
    #[error("zfs transfer header missing source_machine_id")]
    MissingSourceMachineId,
    #[error("list machines for zfs transfer source validation: {message}")]
    SourceMachineLookupFailed { message: String },
    #[error("source machine '{machine_id}' not found")]
    SourceMachineNotFound { machine_id: MachineId },
    #[error("zfs transfer source '{machine_id}' connected from {actual}, expected {expected}")]
    SourceOverlayIpMismatch {
        machine_id: MachineId,
        actual: IpAddr,
        expected: IpAddr,
    },
    #[error("{VOLUME_NOT_AUTHORIZED}")]
    VolumeNotAuthorized {
        namespace: Namespace,
        volume: String,
        source_machine_id: MachineId,
        reason: VolumeAuthorizationRejection,
    },
    #[error(
        "snapshot '{dataset}@{snapshot}' already exists on target with guid {actual_guid}, source claims {expected_guid}"
    )]
    ExistingSnapshotGuidMismatch {
        dataset: String,
        snapshot: String,
        actual_guid: u64,
        expected_guid: u64,
    },
    #[error("incremental base snapshot '{dataset}@{snapshot}' missing on target")]
    IncrementalBaseMissing { dataset: String, snapshot: String },
    #[error(
        "incremental base snapshot '{dataset}@{snapshot}' guid {actual_guid} did not match source {expected_guid}"
    )]
    IncrementalBaseGuidMismatch {
        dataset: String,
        snapshot: String,
        actual_guid: u64,
        expected_guid: u64,
    },
    #[error("{operation}: {message}")]
    Backend {
        operation: &'static str,
        message: String,
    },
}

#[derive(Debug)]
enum VolumeAuthorizationRejection {
    OwnedByOtherMachine { owner: MachineId },
    VolumeNotFound,
    LookupFailed { message: String },
}

impl ZfsTransferValidationError {
    fn backend(operation: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Backend {
            operation,
            message: error.to_string(),
        }
    }

    fn log_context(&self) {
        if let Self::VolumeNotAuthorized {
            namespace,
            volume,
            source_machine_id,
            reason,
        } = self
        {
            match reason {
                VolumeAuthorizationRejection::OwnedByOtherMachine { owner } => tracing::warn!(
                    namespace = %namespace.as_str(),
                    volume,
                    source = %source_machine_id,
                    owner = %owner,
                    "zfs transfer rejected: source is not the volume owner",
                ),
                VolumeAuthorizationRejection::VolumeNotFound => tracing::warn!(
                    namespace = %namespace.as_str(),
                    volume,
                    source = %source_machine_id,
                    "zfs transfer rejected: volume not found",
                ),
                VolumeAuthorizationRejection::LookupFailed { message } => tracing::warn!(
                    error = %message,
                    namespace = %namespace.as_str(),
                    volume,
                    "zfs transfer authorization lookup failed",
                ),
            }
        }
    }
}

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

async fn read_transfer_header<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<String, ZfsTransferValidationError> {
    let mut header = Vec::new();
    let bytes_read = {
        let mut limited = reader.take((MAX_HEADER_BYTES + 1) as u64);
        limited
            .read_until(b'\n', &mut header)
            .await
            .map_err(|error| ZfsTransferValidationError::backend("read_transfer_header", error))?
    };
    if bytes_read == 0 {
        return Err(ZfsTransferValidationError::HeaderClosed);
    }
    // take() lets us read up to MAX_HEADER_BYTES of content plus the trailing
    // newline. If we hit that limit without finding `\n`, the content itself
    // already exceeded the limit; otherwise the read terminated early.
    if header.last() != Some(&b'\n') {
        if header.len() > MAX_HEADER_BYTES {
            return Err(ZfsTransferValidationError::HeaderTooLarge {
                max_bytes: MAX_HEADER_BYTES,
            });
        }
        return Err(ZfsTransferValidationError::HeaderMissingNewline);
    }
    header.pop();
    String::from_utf8(header).map_err(|error| ZfsTransferValidationError::HeaderNotUtf8 {
        message: error.to_string(),
    })
}

async fn validate_open_source(
    store: &StoreDriver,
    open: &ZfsTransferOpen,
    remote_addr: SocketAddr,
) -> Result<MachineId, ZfsTransferValidationError> {
    let Some(source) = open.source_machine_id.as_ref() else {
        return Err(ZfsTransferValidationError::MissingSourceMachineId);
    };
    validate_source_overlay(store, source, remote_addr).await?;
    if let Err(error) =
        validate_volume_ownership(store, source, &open.namespace, &open.volume).await
    {
        error.log_context();
        return Err(error);
    }
    Ok(source.clone())
}

/// Verify the claimed source machine actually owns the namespace/volume the
/// header asks us to write into. Without this check, any active mesh peer can
/// pass IP validation and then redirect a `zfs recv` at an arbitrary dataset
/// under the configured root by picking different namespace/volume fields.
///
/// Returns a uniform "not authorized" message regardless of cause so that any
/// peer on the overlay cannot probe which namespaces or volumes exist on the
/// receiver. The detailed reason is logged locally for operators.
async fn validate_volume_ownership(
    store: &StoreDriver,
    source: &MachineId,
    namespace: &str,
    volume: &str,
) -> Result<(), ZfsTransferValidationError> {
    let namespace = Namespace::new(namespace.to_string());
    match store.get_volume(&namespace, volume).await {
        Ok(Some(record)) if record.machine_id == *source => Ok(()),
        Ok(Some(record)) => Err(ZfsTransferValidationError::VolumeNotAuthorized {
            namespace,
            volume: volume.to_string(),
            source_machine_id: source.clone(),
            reason: VolumeAuthorizationRejection::OwnedByOtherMachine {
                owner: record.machine_id,
            },
        }),
        Ok(None) => Err(ZfsTransferValidationError::VolumeNotAuthorized {
            namespace,
            volume: volume.to_string(),
            source_machine_id: source.clone(),
            reason: VolumeAuthorizationRejection::VolumeNotFound,
        }),
        Err(error) => {
            let message = error.to_string();
            Err(ZfsTransferValidationError::VolumeNotAuthorized {
                namespace,
                volume: volume.to_string(),
                source_machine_id: source.clone(),
                reason: VolumeAuthorizationRejection::LookupFailed { message },
            })
        }
    }
}

async fn validate_source_overlay(
    store: &StoreDriver,
    source: &MachineId,
    remote_addr: SocketAddr,
) -> Result<(), ZfsTransferValidationError> {
    let machines = store.list_machines().await.map_err(|error| {
        ZfsTransferValidationError::SourceMachineLookupFailed {
            message: error.to_string(),
        }
    })?;
    let source_machine = machines
        .into_iter()
        .find(|machine| machine.id == *source)
        .ok_or_else(|| ZfsTransferValidationError::SourceMachineNotFound {
            machine_id: source.clone(),
        })?;
    let expected = IpAddr::V6(source_machine.overlay_ip.0);
    if remote_addr.ip() != expected {
        return Err(ZfsTransferValidationError::SourceOverlayIpMismatch {
            machine_id: source.clone(),
            actual: remote_addr.ip(),
            expected,
        });
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum ReceiveDecision {
    AlreadyHave(u64),
    Proceed { cleanup: ReceiveFailureCleanup },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReceiveFailureCleanup {
    SnapshotOnly,
}

/// Pure decision logic for an incoming `zfs recv`. Generic over `ShellRunner`
/// so it can be unit-tested with a fake runner; the real `receive_stream`
/// wraps this and performs the actual stream I/O.
async fn prepare_receive<R: ShellRunner>(
    driver: &ZfsDriver<R>,
    dataset: &str,
    open: &ZfsTransferOpen,
) -> Result<ReceiveDecision, ZfsTransferValidationError> {
    // Idempotency: if a previous attempt already landed this snapshot with
    // the right GUID, the caller drains the source stream and returns the
    // guid without touching ZFS.
    if driver
        .snapshot_exists(dataset, &open.snapshot)
        .await
        .map_err(|error| ZfsTransferValidationError::backend("snapshot_exists", error))?
    {
        let guid = driver
            .snapshot_guid(dataset, &open.snapshot)
            .await
            .map_err(|error| ZfsTransferValidationError::backend("snapshot_guid", error))?;
        if guid == open.expected_guid {
            return Ok(ReceiveDecision::AlreadyHave(guid));
        }
        return Err(ZfsTransferValidationError::ExistingSnapshotGuidMismatch {
            dataset: dataset.to_string(),
            snapshot: open.snapshot.clone(),
            actual_guid: guid,
            expected_guid: open.expected_guid,
        });
    }

    // For incrementals, refuse to recv unless the named base snapshot is
    // already on disk with the GUID the source claims it had. Catches the
    // "wrong base" footgun that `zfs recv` would otherwise surface as a
    // confusing checksum/lineage error mid-stream.
    let cleanup = if let Some(from_snapshot) = open.from_snapshot.as_deref() {
        if !driver
            .snapshot_exists(dataset, from_snapshot)
            .await
            .map_err(|error| ZfsTransferValidationError::backend("snapshot_exists", error))?
        {
            return Err(ZfsTransferValidationError::IncrementalBaseMissing {
                dataset: dataset.to_string(),
                snapshot: from_snapshot.to_string(),
            });
        }
        if let Some(expected_from_guid) = open.from_snapshot_guid {
            let actual = driver
                .snapshot_guid(dataset, from_snapshot)
                .await
                .map_err(|error| ZfsTransferValidationError::backend("snapshot_guid", error))?;
            if actual != expected_from_guid {
                return Err(ZfsTransferValidationError::IncrementalBaseGuidMismatch {
                    dataset: dataset.to_string(),
                    snapshot: from_snapshot.to_string(),
                    actual_guid: actual,
                    expected_guid: expected_from_guid,
                });
            }
        }
        ReceiveFailureCleanup::SnapshotOnly
    } else {
        // Full sends require the parent namespace dataset to exist; `zfs recv`
        // does not auto-create ancestors.
        driver
            .ensure_parent_dataset(dataset)
            .await
            .map_err(|error| ZfsTransferValidationError::backend("ensure_parent_dataset", error))?;
        ReceiveFailureCleanup::SnapshotOnly
    };
    Ok(ReceiveDecision::Proceed { cleanup })
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

    let cleanup = match prepare_receive(&driver, &dataset, open)
        .await
        .map_err(|error| error.to_string())?
    {
        ReceiveDecision::AlreadyHave(guid) => {
            // Drain whatever the source already started to write so the source
            // process exits cleanly instead of breaking on EPIPE.
            let _ = tokio::io::copy(reader, &mut tokio::io::sink()).await;
            return Ok(guid);
        }
        ReceiveDecision::Proceed { cleanup } => cleanup,
    };

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
        cleanup_partial(&driver, &dataset, open, cleanup).await;
        return Err(format!("copy stream into zfs recv: {error}"));
    }
    if !output.status.success() {
        cleanup_partial(&driver, &dataset, open, cleanup).await;
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

/// Best-effort cleanup of partial state left behind by a failed `zfs recv`.
/// Dataset cleanup is deliberately conservative. A failed receive may leave an
/// empty dataset behind, but deleting recursively would require an ownership
/// marker or per-dataset receive lock to prove the dataset still belongs to
/// this transfer.
async fn cleanup_partial<R: ShellRunner>(
    driver: &ZfsDriver<R>,
    dataset: &str,
    open: &ZfsTransferOpen,
    cleanup: ReceiveFailureCleanup,
) {
    if let Err(error) = driver.destroy_snapshot(dataset, &open.snapshot).await {
        tracing::warn!(
            %error,
            dataset,
            snapshot = %open.snapshot,
            "failed to clean up partial snapshot after recv failure"
        );
    }
    let _ = cleanup;
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_HEADER_BYTES, ReceiveDecision, ReceiveFailureCleanup, ZfsTransferOpen,
        ZfsTransferValidationError, cleanup_partial, prepare_receive, read_transfer_header,
        validate_open_source, validate_source_overlay, validate_volume_ownership,
    };
    use async_trait::async_trait;
    use ployz_runtime_backends::storage::{ShellOutput, ShellRunner, ZfsDriver};
    use ployz_store_api::{DeployCommit, DeployStore, MachineMembershipStore, StoreDriver};
    use ployz_types::error::{Error, Result};
    use ployz_types::model::{
        DeployId, DeployRecord, DeployState, MachineId, MachineLifecycle, MachineMembership,
        MachineTopology, OverlayIp, PublicKey, StorageParticipation, VolumeRecord,
    };
    use ployz_types::spec::{Namespace, VolumeScope};
    use std::collections::{BTreeMap, VecDeque};
    use std::net::{IpAddr, Ipv6Addr, SocketAddr};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, Default)]
    struct FakeShellRunner {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        outputs: Arc<Mutex<VecDeque<ShellOutput>>>,
    }

    impl FakeShellRunner {
        fn push(&self, status: i32, stdout: &str, stderr: &str) {
            self.outputs
                .lock()
                .expect("outputs")
                .push_back(ShellOutput {
                    status,
                    stdout: stdout.as_bytes().to_vec(),
                    stderr: stderr.as_bytes().to_vec(),
                });
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("calls").clone()
        }
    }

    #[async_trait]
    impl ShellRunner for FakeShellRunner {
        async fn run(&self, program: &str, args: &[&str]) -> Result<ShellOutput> {
            let mut call = vec![program.to_string()];
            call.extend(args.iter().map(|arg| (*arg).to_string()));
            self.calls.lock().expect("calls").push(call);
            self.outputs
                .lock()
                .expect("outputs")
                .pop_front()
                .ok_or_else(|| Error::operation("fake_shell", "missing output"))
        }
    }

    /// Build a driver wired to fake `tank/ployz` root. The driver constructor
    /// itself runs one `zfs list` call, so we satisfy that first.
    async fn driver(fake: &FakeShellRunner) -> ZfsDriver<FakeShellRunner> {
        fake.push(0, "/tank/ployz\n", "");
        ZfsDriver::new(fake.clone(), "tank/ployz", 1.0)
            .await
            .expect("driver")
    }

    fn open(snapshot: &str, expected_guid: u64) -> ZfsTransferOpen {
        ZfsTransferOpen {
            namespace: "default".into(),
            volume: "data".into(),
            snapshot: snapshot.into(),
            expected_guid,
            source_machine_id: Some(MachineId::new("source")),
            from_snapshot: None,
            from_snapshot_guid: None,
        }
    }

    #[tokio::test]
    async fn cleanup_partial_full_receive_keeps_dataset() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "", "");

        cleanup_partial(
            &driver,
            "tank/ployz/default/data",
            &open("snap", 42),
            ReceiveFailureCleanup::SnapshotOnly,
        )
        .await;

        let calls = fake.calls();
        assert_eq!(calls[1], ["zfs", "destroy", "tank/ployz/default/data@snap"]);
        assert_eq!(calls.len(), 2);
    }

    #[tokio::test]
    async fn cleanup_partial_incremental_receive_keeps_dataset() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "", "");
        let mut transfer = open("snap", 42);
        transfer.from_snapshot = Some("base".into());
        transfer.from_snapshot_guid = Some(11);

        cleanup_partial(
            &driver,
            "tank/ployz/default/data",
            &transfer,
            ReceiveFailureCleanup::SnapshotOnly,
        )
        .await;

        let calls = fake.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1], ["zfs", "destroy", "tank/ployz/default/data@snap"]);
    }

    #[tokio::test]
    async fn prepare_receive_returns_already_have_when_snapshot_matches() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        // snapshot_exists -> status 0 means present
        fake.push(0, "tank/ployz/default/data@snap\n", "");
        // snapshot_guid -> stdout is the guid
        fake.push(0, "42\n", "");

        let decision = prepare_receive(&driver, "tank/ployz/default/data", &open("snap", 42))
            .await
            .expect("prepare");

        assert_eq!(decision, ReceiveDecision::AlreadyHave(42));
    }

    #[tokio::test]
    async fn prepare_receive_rejects_existing_snapshot_with_mismatched_guid() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        fake.push(0, "tank/ployz/default/data@snap\n", "");
        fake.push(0, "7\n", "");

        let err = prepare_receive(&driver, "tank/ployz/default/data", &open("snap", 42))
            .await
            .expect_err("mismatched guid should fail");

        assert!(matches!(
            err,
            ZfsTransferValidationError::ExistingSnapshotGuidMismatch {
                actual_guid: 7,
                expected_guid: 42,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn prepare_receive_rejects_incremental_when_base_missing() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        // target snapshot missing
        fake.push(1, "", "snapshot does not exist");
        // base snapshot also missing
        fake.push(1, "", "snapshot does not exist");
        let mut o = open("snap", 42);
        o.from_snapshot = Some("base".into());
        o.from_snapshot_guid = Some(11);

        let err = prepare_receive(&driver, "tank/ployz/default/data", &o)
            .await
            .expect_err("missing base should fail");

        assert!(matches!(
            err,
            ZfsTransferValidationError::IncrementalBaseMissing { .. }
        ));
    }

    #[tokio::test]
    async fn prepare_receive_rejects_incremental_when_base_guid_diverges() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        // target snapshot missing
        fake.push(1, "", "snapshot does not exist");
        // base snapshot present
        fake.push(0, "tank/ployz/default/data@base\n", "");
        // base guid -> different from claim
        fake.push(0, "99\n", "");
        let mut o = open("snap", 42);
        o.from_snapshot = Some("base".into());
        o.from_snapshot_guid = Some(11);

        let err = prepare_receive(&driver, "tank/ployz/default/data", &o)
            .await
            .expect_err("mismatched base guid should fail");

        assert!(matches!(
            err,
            ZfsTransferValidationError::IncrementalBaseGuidMismatch {
                actual_guid: 99,
                expected_guid: 11,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn prepare_receive_keeps_existing_dataset_on_full_receive_failure() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        // target snapshot missing
        fake.push(1, "", "snapshot does not exist");
        // parent lookup performed by ensure_parent_dataset
        fake.push(0, "tank/ployz/default\t1G\t/tank/ployz/default\n", "");

        let decision = prepare_receive(&driver, "tank/ployz/default/data", &open("snap", 42))
            .await
            .expect("prepare");

        assert_eq!(
            decision,
            ReceiveDecision::Proceed {
                cleanup: ReceiveFailureCleanup::SnapshotOnly
            }
        );
    }

    #[tokio::test]
    async fn prepare_receive_keeps_new_full_receive_dataset_on_failure() {
        let fake = FakeShellRunner::default();
        let driver = driver(&fake).await;
        // target snapshot missing
        fake.push(1, "", "snapshot does not exist");
        // parent lookup performed by ensure_parent_dataset
        fake.push(0, "tank/ployz/default\t1G\t/tank/ployz/default\n", "");

        let decision = prepare_receive(&driver, "tank/ployz/default/data", &open("snap", 42))
            .await
            .expect("prepare");

        assert_eq!(
            decision,
            ReceiveDecision::Proceed {
                cleanup: ReceiveFailureCleanup::SnapshotOnly
            }
        );
    }

    fn machine(id: &str, overlay: Ipv6Addr) -> MachineMembership {
        MachineMembership {
            id: MachineId::new(id),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp(overlay),
            topology: MachineTopology::local(),
            region_role: ployz_types::model::RegionRole::HomeData,
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: MachineLifecycle::Active,
            storage_role: StorageParticipation::default_authority().into(),
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }

    async fn store_with(machines: Vec<MachineMembership>) -> StoreDriver {
        let store = StoreDriver::memory();
        for m in machines {
            store.upsert_self_machine(&m).await.expect("upsert");
        }
        store
    }

    async fn insert_volume(store: &StoreDriver, namespace: &str, volume: &str, owner: &str) {
        let namespace = Namespace::new(namespace.to_string());
        let deploy_id = DeployId::new(format!("deploy-{volume}"));
        let record = VolumeRecord {
            namespace: namespace.clone(),
            volume_name: volume.to_string(),
            scope: VolumeScope::Single,
            machine_id: MachineId::new(owner.to_string()),
            quota: "1G".into(),
            mode: "rw".into(),
            owner: "0:0".into(),
            attached_services: Vec::new(),
            created_at: 0,
            created_by_deploy_id: deploy_id.clone(),
            last_modified_at: 0,
            last_modified_by_deploy_id: deploy_id.clone(),
        };
        let deploy = DeployRecord {
            deploy_id: deploy_id.clone(),
            namespace: namespace.clone(),
            coordinator_machine_id: MachineId::new(owner.to_string()),
            manifest_hash: "test".into(),
            state: DeployState::Committed,
            started_at: 0,
            committed_at: Some(0),
            finished_at: Some(0),
            summary_json: "{}".into(),
        };
        store
            .commit_deploy(&DeployCommit {
                namespace: namespace.clone(),
                revisions: Vec::new(),
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
                branch_lineage: Vec::new(),
                volume_movements: Vec::new(),
                volume_branches: Vec::new(),
                phase_commits: Vec::new(),
                releases: Vec::new(),
                volumes: vec![record],
                deploy,
            })
            .await
            .expect("commit volume");
    }

    #[tokio::test]
    async fn validate_source_overlay_accepts_matching_ip() {
        let overlay = "fd00::1".parse::<Ipv6Addr>().unwrap();
        let store = store_with(vec![machine("source", overlay)]).await;
        let remote = SocketAddr::new(IpAddr::V6(overlay), 4319);
        validate_source_overlay(&store, &MachineId::new("source"), remote)
            .await
            .expect("matching ip accepted");
    }

    #[tokio::test]
    async fn validate_source_overlay_rejects_mismatched_ip() {
        let claimed = "fd00::1".parse::<Ipv6Addr>().unwrap();
        let attacker = "fd00::2".parse::<Ipv6Addr>().unwrap();
        let store = store_with(vec![machine("source", claimed)]).await;
        let remote = SocketAddr::new(IpAddr::V6(attacker), 4319);
        let err = validate_source_overlay(&store, &MachineId::new("source"), remote)
            .await
            .expect_err("mismatched ip rejected");
        assert!(matches!(
            err,
            ZfsTransferValidationError::SourceOverlayIpMismatch { .. }
        ));
    }

    #[tokio::test]
    async fn validate_source_overlay_rejects_unknown_machine() {
        let overlay = "fd00::1".parse::<Ipv6Addr>().unwrap();
        let store = store_with(vec![machine("known", overlay)]).await;
        let remote = SocketAddr::new(IpAddr::V6(overlay), 4319);
        let err = validate_source_overlay(&store, &MachineId::new("ghost"), remote)
            .await
            .expect_err("unknown machine rejected");
        assert!(matches!(
            err,
            ZfsTransferValidationError::SourceMachineNotFound { .. }
        ));
    }

    #[tokio::test]
    async fn validate_open_source_rejects_missing_source_machine_id() {
        let overlay = "fd00::1".parse::<Ipv6Addr>().unwrap();
        let store = store_with(vec![machine("source", overlay)]).await;
        let remote = SocketAddr::new(IpAddr::V6(overlay), 4319);
        let open = ZfsTransferOpen {
            namespace: "default".into(),
            volume: "data".into(),
            snapshot: "snap".into(),
            expected_guid: 1,
            source_machine_id: None,
            from_snapshot: None,
            from_snapshot_guid: None,
        };

        let err = validate_open_source(&store, &open, remote)
            .await
            .expect_err("missing source rejected");
        assert!(matches!(
            err,
            ZfsTransferValidationError::MissingSourceMachineId
        ));
    }

    #[tokio::test]
    async fn validate_open_source_accepts_when_source_owns_volume() {
        let overlay = "fd00::1".parse::<Ipv6Addr>().unwrap();
        let store = store_with(vec![machine("source", overlay)]).await;
        insert_volume(&store, "default", "data", "source").await;
        let remote = SocketAddr::new(IpAddr::V6(overlay), 4319);

        validate_open_source(&store, &open("snap", 1), remote)
            .await
            .expect("owner accepted");
    }

    #[tokio::test]
    async fn validate_open_source_rejects_when_volume_owned_by_other_machine() {
        let overlay = "fd00::1".parse::<Ipv6Addr>().unwrap();
        let other = "fd00::2".parse::<Ipv6Addr>().unwrap();
        let store = store_with(vec![machine("source", overlay), machine("owner", other)]).await;
        insert_volume(&store, "default", "data", "owner").await;
        let remote = SocketAddr::new(IpAddr::V6(overlay), 4319);

        let err = validate_open_source(&store, &open("snap", 1), remote)
            .await
            .expect_err("non-owner rejected");
        let rendered = err.to_string();
        assert!(matches!(
            err,
            ZfsTransferValidationError::VolumeNotAuthorized { .. }
        ));
        assert!(
            rendered.contains("not authorized"),
            "unexpected error: {rendered}"
        );
        assert!(
            !rendered.contains("owner"),
            "ownership detail leaked: {rendered}"
        );
    }

    #[tokio::test]
    async fn validate_volume_ownership_rejects_unknown_volume() {
        let store = StoreDriver::memory();
        let err =
            validate_volume_ownership(&store, &MachineId::new("source"), "default", "missing")
                .await
                .expect_err("unknown volume rejected");
        let rendered = err.to_string();
        assert!(matches!(
            err,
            ZfsTransferValidationError::VolumeNotAuthorized { .. }
        ));
        assert!(
            rendered.contains("not authorized"),
            "unexpected error: {rendered}"
        );
        assert!(
            !rendered.contains("missing"),
            "volume name leaked: {rendered}"
        );
    }

    #[tokio::test]
    async fn read_transfer_header_rejects_oversized_header() {
        let mut body = vec![b'a'; MAX_HEADER_BYTES + 1];
        body.push(b'\n');
        let mut reader = tokio::io::BufReader::new(body.as_slice());

        let err = read_transfer_header(&mut reader)
            .await
            .expect_err("oversized header rejected");
        assert!(matches!(
            err,
            ZfsTransferValidationError::HeaderTooLarge {
                max_bytes: MAX_HEADER_BYTES
            }
        ));
    }

    #[tokio::test]
    async fn read_transfer_header_rejects_missing_newline() {
        let body = b"{\"namespace\":\"default\"}".as_slice();
        let mut reader = tokio::io::BufReader::new(body);

        let err = read_transfer_header(&mut reader)
            .await
            .expect_err("unterminated header rejected");
        assert!(matches!(
            err,
            ZfsTransferValidationError::HeaderMissingNewline
        ));
    }
}
