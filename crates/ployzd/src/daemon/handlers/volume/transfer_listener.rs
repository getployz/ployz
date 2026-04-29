use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ployz_runtime_api::RuntimeHandle;
use ployz_runtime_backends::storage::{ShellRunner, TokioShellRunner, ZfsDriver};
use ployz_store_api::{DeployRepository, MachineRegistry, StoreDriver};
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
    let source = validate_open_source(&store, &open, remote_addr).await?;
    tracing::info!(
        %remote_addr,
        source_machine_id = %source.0,
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

async fn read_transfer_header<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<String, String> {
    let mut header = Vec::new();
    let bytes_read = {
        let mut limited = reader.take((MAX_HEADER_BYTES + 1) as u64);
        limited
            .read_until(b'\n', &mut header)
            .await
            .map_err(|error| error.to_string())?
    };
    if bytes_read == 0 {
        return Err("connection closed before zfs transfer header".to_string());
    }
    // take() lets us read up to MAX_HEADER_BYTES of content plus the trailing
    // newline. If we hit that limit without finding `\n`, the content itself
    // already exceeded the limit; otherwise the read terminated early.
    if header.last() != Some(&b'\n') {
        if header.len() > MAX_HEADER_BYTES {
            return Err(format!(
                "zfs transfer header exceeded {MAX_HEADER_BYTES} bytes"
            ));
        }
        return Err("zfs transfer header missing newline".to_string());
    }
    header.pop();
    String::from_utf8(header).map_err(|error| format!("header was not UTF-8: {error}"))
}

async fn validate_open_source(
    store: &StoreDriver,
    open: &ZfsTransferOpen,
    remote_addr: SocketAddr,
) -> Result<MachineId, String> {
    let Some(source) = open.source_machine_id.as_ref() else {
        return Err("zfs transfer header missing source_machine_id".to_string());
    };
    validate_source_overlay(store, source, remote_addr).await?;
    validate_volume_ownership(store, source, &open.namespace, &open.volume).await?;
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
) -> Result<(), String> {
    let namespace = Namespace(namespace.to_string());
    match store.get_volume(&namespace, volume).await {
        Ok(Some(record)) if record.machine_id == *source => Ok(()),
        Ok(Some(record)) => {
            tracing::warn!(
                namespace = %namespace.0,
                volume,
                source = %source,
                owner = %record.machine_id,
                "zfs transfer rejected: source is not the volume owner",
            );
            Err(VOLUME_NOT_AUTHORIZED.to_string())
        }
        Ok(None) => {
            tracing::warn!(
                namespace = %namespace.0,
                volume,
                source = %source,
                "zfs transfer rejected: volume not found",
            );
            Err(VOLUME_NOT_AUTHORIZED.to_string())
        }
        Err(error) => {
            tracing::warn!(
                %error,
                namespace = %namespace.0,
                volume,
                "zfs transfer authorization lookup failed",
            );
            Err(VOLUME_NOT_AUTHORIZED.to_string())
        }
    }
}

const VOLUME_NOT_AUTHORIZED: &str = "zfs transfer not authorized";

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

#[derive(Debug, PartialEq, Eq)]
enum ReceiveDecision {
    AlreadyHave(u64),
    Proceed,
}

/// Pure decision logic for an incoming `zfs recv`. Generic over `ShellRunner`
/// so it can be unit-tested with a fake runner; the real `receive_stream`
/// wraps this and performs the actual stream I/O.
async fn prepare_receive<R: ShellRunner>(
    driver: &ZfsDriver<R>,
    dataset: &str,
    open: &ZfsTransferOpen,
) -> Result<ReceiveDecision, String> {
    // Idempotency: if a previous attempt already landed this snapshot with
    // the right GUID, the caller drains the source stream and returns the
    // guid without touching ZFS.
    if driver
        .snapshot_exists(dataset, &open.snapshot)
        .await
        .map_err(|error| error.to_string())?
    {
        let guid = driver
            .snapshot_guid(dataset, &open.snapshot)
            .await
            .map_err(|error| error.to_string())?;
        if guid == open.expected_guid {
            return Ok(ReceiveDecision::AlreadyHave(guid));
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
            .snapshot_exists(dataset, from_snapshot)
            .await
            .map_err(|error| error.to_string())?
        {
            return Err(format!(
                "incremental base snapshot '{dataset}@{from_snapshot}' missing on target"
            ));
        }
        if let Some(expected_from_guid) = open.from_snapshot_guid {
            let actual = driver
                .snapshot_guid(dataset, from_snapshot)
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
            .ensure_parent_dataset(dataset)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(ReceiveDecision::Proceed)
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

    if let ReceiveDecision::AlreadyHave(guid) = prepare_receive(&driver, &dataset, open).await? {
        // Drain whatever the source already started to write so the source
        // process exits cleanly instead of breaking on EPIPE.
        let _ = tokio::io::copy(reader, &mut tokio::io::sink()).await;
        return Ok(guid);
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

#[cfg(test)]
mod tests {
    use super::{
        MAX_HEADER_BYTES, ReceiveDecision, ZfsTransferOpen, prepare_receive, read_transfer_header,
        validate_open_source, validate_source_overlay, validate_volume_ownership,
    };
    use async_trait::async_trait;
    use ployz_runtime_backends::storage::{ShellOutput, ShellRunner, ZfsDriver};
    use ployz_store_api::{DeployCommit, DeployRepository, MachineRegistry, StoreDriver};
    use ployz_types::error::{Error, Result};
    use ployz_types::model::{
        DeployId, DeployRecord, DeployState, MachineId, MachineLifecycle, MachineMembership,
        MachineTopology, OverlayIp, PublicKey, VolumeRecord,
    };
    use ployz_types::spec::{Namespace, VolumeScope};
    use std::collections::{BTreeMap, VecDeque};
    use std::net::{IpAddr, Ipv6Addr, SocketAddr};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, Default)]
    struct FakeShellRunner {
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
    }

    #[async_trait]
    impl ShellRunner for FakeShellRunner {
        async fn run(&self, _program: &str, _args: &[&str]) -> Result<ShellOutput> {
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
            source_machine_id: Some(MachineId("source".into())),
            from_snapshot: None,
            from_snapshot_guid: None,
        }
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

        assert!(err.contains("guid 7"), "got: {err}");
        assert!(err.contains("source claims 42"), "got: {err}");
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

        assert!(err.contains("base snapshot"), "got: {err}");
        assert!(err.contains("missing on target"), "got: {err}");
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

        assert!(err.contains("guid 99"), "got: {err}");
        assert!(err.contains("source 11"), "got: {err}");
    }

    fn machine(id: &str, overlay: Ipv6Addr) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.into()),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp(overlay),
            topology: MachineTopology::local(),
            control_target: None,
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: MachineLifecycle::Active,
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
        let namespace = Namespace(namespace.to_string());
        let deploy_id = DeployId(format!("deploy-{volume}"));
        let record = VolumeRecord {
            namespace: namespace.clone(),
            volume_name: volume.to_string(),
            scope: VolumeScope::Single,
            machine_id: MachineId(owner.to_string()),
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
            coordinator_machine_id: MachineId(owner.to_string()),
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
                removed_services: Vec::new(),
                removed_volumes: Vec::new(),
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
        validate_source_overlay(&store, &MachineId("source".into()), remote)
            .await
            .expect("matching ip accepted");
    }

    #[tokio::test]
    async fn validate_source_overlay_rejects_mismatched_ip() {
        let claimed = "fd00::1".parse::<Ipv6Addr>().unwrap();
        let attacker = "fd00::2".parse::<Ipv6Addr>().unwrap();
        let store = store_with(vec![machine("source", claimed)]).await;
        let remote = SocketAddr::new(IpAddr::V6(attacker), 4319);
        let err = validate_source_overlay(&store, &MachineId("source".into()), remote)
            .await
            .expect_err("mismatched ip rejected");
        assert!(err.contains("connected from"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn validate_source_overlay_rejects_unknown_machine() {
        let overlay = "fd00::1".parse::<Ipv6Addr>().unwrap();
        let store = store_with(vec![machine("known", overlay)]).await;
        let remote = SocketAddr::new(IpAddr::V6(overlay), 4319);
        let err = validate_source_overlay(&store, &MachineId("ghost".into()), remote)
            .await
            .expect_err("unknown machine rejected");
        assert!(err.contains("not found"), "unexpected error: {err}");
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
        assert!(
            err.contains("missing source_machine_id"),
            "unexpected error: {err}"
        );
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
        assert!(err.contains("not authorized"), "unexpected error: {err}");
        assert!(!err.contains("owner"), "ownership detail leaked: {err}");
    }

    #[tokio::test]
    async fn validate_volume_ownership_rejects_unknown_volume() {
        let store = StoreDriver::memory();
        let err =
            validate_volume_ownership(&store, &MachineId("source".into()), "default", "missing")
                .await
                .expect_err("unknown volume rejected");
        assert!(err.contains("not authorized"), "unexpected error: {err}");
        assert!(!err.contains("missing"), "volume name leaked: {err}");
    }

    #[tokio::test]
    async fn read_transfer_header_rejects_oversized_header() {
        let mut body = vec![b'a'; MAX_HEADER_BYTES + 1];
        body.push(b'\n');
        let mut reader = tokio::io::BufReader::new(body.as_slice());

        let err = read_transfer_header(&mut reader)
            .await
            .expect_err("oversized header rejected");
        assert!(err.contains("exceeded"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn read_transfer_header_rejects_missing_newline() {
        let body = b"{\"namespace\":\"default\"}".as_slice();
        let mut reader = tokio::io::BufReader::new(body);

        let err = read_transfer_header(&mut reader)
            .await
            .expect_err("unterminated header rejected");
        assert!(err.contains("missing newline"), "unexpected error: {err}");
    }
}
