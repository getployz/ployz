use std::path::PathBuf;
use std::time::Duration;

use ployz_api::{
    DaemonPayload, DaemonResponse, VolumeZfsInspectPayload, VolumeZfsPeerSendPayload,
    VolumeZfsSnapshotInfo, VolumeZfsSnapshotPayload, VolumeZfsTransferInfo,
    VolumeZfsTransferListPayload, VolumeZfsTransferPayload,
};
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject, RpcPolicy};
use ployz_runtime_backends::storage::{TokioShellRunner, ZfsDriver};
use ployz_store_api::{DeployStore, MachineMembershipStore};
use ployz_types::model::{MachineId, MachineLifecycle, MachineMembership, VolumeRecord};
use ployz_types::spec::{Namespace, VolumeScope};
use ployz_types::time::now_unix_secs;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::daemon::DaemonState;
use crate::daemon::handlers::volume::transfer_listener::{ZfsTransferOpen, ZfsTransferReceived};

const TRANSFERS_DIR_NAME: &str = "zfs-transfers";
const ACK_READ_TIMEOUT: Duration = Duration::from_secs(30);
const ZFS_SEND_RPC_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_ACK_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy)]
struct SendResult {
    bytes_transferred: u64,
    snapshot_guid: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum TransferStatus {
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

impl TransferStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TransferTransition {
    status: TransferStatus,
    last_error: Option<String>,
    at_unix_secs: u64,
}

impl TransferTransition {
    fn succeeded(at_unix_secs: u64) -> Self {
        Self {
            status: TransferStatus::Succeeded,
            last_error: None,
            at_unix_secs,
        }
    }

    fn failed(last_error: String, at_unix_secs: u64) -> Self {
        Self {
            status: TransferStatus::Failed,
            last_error: Some(last_error),
            at_unix_secs,
        }
    }

    fn interrupted(at_unix_secs: u64) -> Self {
        Self {
            status: TransferStatus::Interrupted,
            last_error: None,
            at_unix_secs,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TransferRecord {
    id: String,
    namespace: String,
    volume: String,
    source_machine: MachineId,
    target_machine: MachineId,
    status: TransferStatus,
    stage: String,
    snapshot_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    snapshot_guid: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from_snapshot_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from_snapshot_guid: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bytes_transferred: Option<u64>,
    started_at: u64,
    updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
}

impl TransferRecord {
    fn info(&self) -> VolumeZfsTransferInfo {
        VolumeZfsTransferInfo {
            id: self.id.clone(),
            namespace: self.namespace.clone(),
            volume: self.volume.clone(),
            source_machine: self.source_machine.clone(),
            target_machine: self.target_machine.clone(),
            status: self.status.as_str().to_string(),
            stage: self.stage.clone(),
            snapshot_name: self.snapshot_name.clone(),
            snapshot_guid: self.snapshot_guid,
            from_snapshot_name: self.from_snapshot_name.clone(),
            from_snapshot_guid: self.from_snapshot_guid,
            bytes_transferred: self.bytes_transferred,
            started_at: self.started_at,
            updated_at: self.updated_at,
            last_error: self.last_error.clone(),
        }
    }

    fn apply_transition(&mut self, transition: TransferTransition) {
        let TransferTransition {
            status,
            last_error,
            at_unix_secs,
        } = transition;
        self.status = status;
        match status {
            TransferStatus::Succeeded => self.last_error = None,
            TransferStatus::Failed | TransferStatus::Interrupted | TransferStatus::Running => {
                if let Some(last_error) = last_error {
                    self.last_error = Some(last_error);
                }
            }
        }
        self.updated_at = at_unix_secs;
    }
}

#[derive(Debug, Clone)]
struct TransferStore {
    root: PathBuf,
}

impl TransferStore {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn begin(
        &self,
        namespace: &Namespace,
        volume: &str,
        source_machine: MachineId,
        target_machine: MachineId,
        snapshot_name: String,
        from_snapshot_name: Option<String>,
    ) -> Result<TransferRecord, String> {
        let now = now_unix_secs();
        let record = TransferRecord {
            id: unique_transfer_id(now)?,
            namespace: namespace.0.clone(),
            volume: volume.to_string(),
            source_machine,
            target_machine,
            status: TransferStatus::Running,
            stage: "preflight".into(),
            snapshot_name,
            snapshot_guid: None,
            from_snapshot_name,
            from_snapshot_guid: None,
            bytes_transferred: None,
            started_at: now,
            updated_at: now,
            last_error: None,
        };
        self.save(&record)?;
        Ok(record)
    }

    fn update_stage(
        &self,
        record: &mut TransferRecord,
        stage: impl Into<String>,
    ) -> Result<(), String> {
        record.stage = stage.into();
        record.updated_at = now_unix_secs();
        self.save(record)
    }

    fn update_status(
        &self,
        record: &mut TransferRecord,
        status: TransferStatus,
        last_error: Option<String>,
    ) -> Result<(), String> {
        let at_unix_secs = now_unix_secs();
        let transition = match status {
            TransferStatus::Running => TransferTransition {
                status,
                last_error,
                at_unix_secs,
            },
            TransferStatus::Succeeded => TransferTransition::succeeded(at_unix_secs),
            TransferStatus::Failed => {
                TransferTransition::failed(last_error.unwrap_or_default(), at_unix_secs)
            }
            TransferStatus::Interrupted => TransferTransition::interrupted(at_unix_secs),
        };
        record.apply_transition(transition);
        self.save(record)
    }

    fn save(&self, record: &TransferRecord) -> Result<(), String> {
        let path = self.path_for(&record.id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("create zfs transfer dir '{}': {err}", parent.display()))?;
        }
        let body = serde_json::to_vec_pretty(record)
            .map_err(|err| format!("encode zfs transfer '{}': {err}", record.id))?;
        std::fs::write(&path, body)
            .map_err(|err| format!("write zfs transfer '{}': {err}", path.display()))
    }

    fn load(&self, id: &str) -> Result<Option<TransferRecord>, String> {
        validate_transfer_id(id)?;
        let path = self.path_for(id);
        if !path.exists() {
            return Ok(None);
        }
        read_transfer(&path).map(Some)
    }

    fn list(&self) -> Result<Vec<TransferRecord>, String> {
        let dir = self.dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(format!("read zfs transfer dir '{}': {err}", dir.display())),
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| format!("read zfs transfer entry: {err}"))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) == Some("json") {
                records.push(read_transfer(&path)?);
            }
        }
        records.sort_by(|left, right| {
            right
                .started_at
                .cmp(&left.started_at)
                .then(left.id.cmp(&right.id))
        });
        Ok(records)
    }

    fn recover_startup(&self) -> Result<usize, String> {
        let mut count = 0;
        for mut record in self.list()? {
            if record.status == TransferStatus::Running {
                self.update_status(&mut record, TransferStatus::Interrupted, None)?;
                count += 1;
            }
        }
        Ok(count)
    }

    fn dir(&self) -> PathBuf {
        self.root.join(TRANSFERS_DIR_NAME)
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.dir().join(format!("{id}.json"))
    }
}

fn validate_transfer_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 128 {
        return Err(format!("transfer id must be 1-128 chars, got {}", id.len()));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "transfer id '{id}' must contain only [A-Za-z0-9_-]"
        ));
    }
    Ok(())
}

fn read_transfer(path: &std::path::Path) -> Result<TransferRecord, String> {
    let body = std::fs::read(path)
        .map_err(|err| format!("read zfs transfer '{}': {err}", path.display()))?;
    serde_json::from_slice(&body)
        .map_err(|err| format!("decode zfs transfer '{}': {err}", path.display()))
}

fn unique_transfer_id(now: u64) -> Result<String, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(|err| format!("system clock is before UNIX epoch: {err}"))?;
    Ok(format!("zfs-transfer-{now}-{}-{nanos}", std::process::id()))
}

impl DaemonState {
    fn zfs_transfer_store(&self) -> TransferStore {
        TransferStore::new(self.data_dir.clone())
    }

    pub(crate) async fn recover_zfs_transfers_on_startup(&self) {
        match self.zfs_transfer_store().recover_startup() {
            Ok(count) if count > 0 => {
                tracing::warn!(count, "marked running zfs transfers interrupted")
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "failed to recover zfs transfer startup state"),
        }
    }

    pub(crate) async fn handle_volume_zfs_inspect(
        &self,
        namespace: &str,
        volume: &str,
        machine: Option<&str>,
    ) -> DaemonResponse {
        let namespace = Namespace(namespace.to_string());
        let target_machine: Option<String> = match machine {
            Some(machine) => Some(machine.to_string()),
            None => match self.volume_record(&namespace, volume).await {
                Ok(record) => Some(record.machine_id.0),
                Err(error) => return self.err("VOLUME_ZFS_INSPECT_FAILED", error),
            },
        };
        if let Some(machine) = target_machine
            && machine != self.identity.machine_id.0
        {
            return self
                .forward_volume_zfs_inspect(&namespace.0, volume, &machine)
                .await;
        }
        match self.inspect_local_volume_zfs(&namespace, volume).await {
            Ok(payload) => self.ok_with_payload(
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "ok".into()),
                Some(DaemonPayload::VolumeZfsInspect(payload)),
            ),
            Err(error) => self.err("VOLUME_ZFS_INSPECT_FAILED", error.to_string()),
        }
    }

    pub(crate) async fn handle_volume_zfs_snapshot(
        &self,
        namespace: &str,
        volume: &str,
        snapshot: &str,
    ) -> DaemonResponse {
        let namespace = Namespace(namespace.to_string());
        let record = match self.volume_record(&namespace, volume).await {
            Ok(record) => record,
            Err(error) => return self.err("VOLUME_ZFS_SNAPSHOT_FAILED", error),
        };
        if record.machine_id != self.identity.machine_id {
            return self
                .forward_volume_zfs_snapshot(&record.machine_id, &namespace, volume, snapshot)
                .await;
        }
        match self
            .snapshot_local_volume_zfs(&namespace, volume, snapshot)
            .await
        {
            Ok(payload) => self.ok_with_payload(
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "ok".into()),
                Some(DaemonPayload::VolumeZfsSnapshot(payload)),
            ),
            Err(error) => self.err("VOLUME_ZFS_SNAPSHOT_FAILED", error.to_string()),
        }
    }

    pub(crate) async fn handle_volume_zfs_send(
        &self,
        namespace: &str,
        volume: &str,
        snapshot: &str,
        target_machine: &str,
        from_snapshot: Option<&str>,
    ) -> DaemonResponse {
        let namespace = Namespace(namespace.to_string());
        let record = match self.volume_record(&namespace, volume).await {
            Ok(record) => record,
            Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
        };
        if record.scope != VolumeScope::Single {
            return self.err(
                "VOLUME_ZFS_SCOPE_NOT_SUPPORTED",
                format!(
                    "volume '{}/{}' has scope {:?}; only Single is supported in this build",
                    namespace.0, volume, record.scope
                ),
            );
        }
        let source = match self.find_active_machine(&record.machine_id.0).await {
            Ok(record) => record,
            Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
        };
        let target = match self.find_active_machine(target_machine).await {
            Ok(record) => record,
            Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
        };
        let local_driver =
            if source.id == self.identity.machine_id || target.id == self.identity.machine_id {
                match self.local_zfs_driver().await {
                    Ok(driver) => Some(driver),
                    Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
                }
            } else {
                None
            };
        let transfer_port = self.zfs_transfer_port;
        let needs_nats_rpc = source.id != self.identity.machine_id
            || (from_snapshot.is_some() && target.id != self.identity.machine_id);
        let nats_rpc = if needs_nats_rpc {
            match self.nats_node_rpc_client().await {
                Ok(client) => Some(client.with_policy(RpcPolicy {
                    timeout: ZFS_SEND_RPC_TIMEOUT,
                })),
                Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
            }
        } else {
            None
        };

        let store = self.zfs_transfer_store();
        let transfer = match store.begin(
            &namespace,
            volume,
            source.id.clone(),
            target.id.clone(),
            snapshot.to_string(),
            from_snapshot.map(str::to_string),
        ) {
            Ok(transfer) => transfer,
            Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
        };

        let info = transfer.info();
        let task_store = store.clone();
        let task_record = record;
        let task_source = source;
        let task_target = target;
        let task_local_driver = local_driver;
        let task_local = self.identity.machine_id.clone();
        let task_snapshot = snapshot.to_string();
        let task_from = from_snapshot.map(str::to_string);
        let mut task_transfer = transfer;
        tokio::spawn(async move {
            let result = run_coordinated_zfs_transfer_inner(
                &task_store,
                &mut task_transfer,
                &task_record,
                &task_source,
                &task_target,
                task_local_driver.as_ref(),
                nats_rpc,
                transfer_port,
                &task_local,
                &task_snapshot,
                task_from.as_deref(),
            )
            .await;
            finalize_zfs_transfer(&task_store, &mut task_transfer, result);
        });

        self.ok_with_payload(
            serde_json::to_string_pretty(&info).unwrap_or_else(|_| info.id.clone()),
            Some(DaemonPayload::VolumeZfsTransfer(VolumeZfsTransferPayload {
                transfer: info,
            })),
        )
    }

    pub(crate) async fn handle_volume_zfs_peer_snapshot(
        &self,
        namespace: &str,
        volume: &str,
        snapshot: &str,
    ) -> DaemonResponse {
        let namespace = Namespace(namespace.to_string());
        match self
            .snapshot_local_source_volume_zfs(&namespace, volume, snapshot)
            .await
        {
            Ok(payload) => self.ok_with_payload(
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "ok".into()),
                Some(DaemonPayload::VolumeZfsSnapshot(payload)),
            ),
            Err(error) => self.err("VOLUME_ZFS_PEER_SNAPSHOT_FAILED", error),
        }
    }

    pub(crate) async fn handle_volume_zfs_peer_snapshot_guid(
        &self,
        namespace: &str,
        volume: &str,
        snapshot: &str,
    ) -> DaemonResponse {
        let namespace = Namespace(namespace.to_string());
        match self
            .snapshot_guid_local_volume_zfs(&namespace, volume, snapshot)
            .await
        {
            Ok(payload) => self.ok_with_payload(
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "ok".into()),
                Some(DaemonPayload::VolumeZfsSnapshot(payload)),
            ),
            Err(error) => self.err("VOLUME_ZFS_PEER_SNAPSHOT_GUID_FAILED", error),
        }
    }

    pub(crate) async fn handle_volume_zfs_peer_start_send(
        &self,
        namespace: &str,
        volume: &str,
        snapshot: &str,
        target_machine: &str,
        expected_guid: u64,
        from_snapshot: Option<&str>,
        from_snapshot_guid: Option<u64>,
    ) -> DaemonResponse {
        let namespace = Namespace(namespace.to_string());
        let record = match self.volume_record(&namespace, volume).await {
            Ok(record) => record,
            Err(error) => return self.err("VOLUME_ZFS_PEER_START_SEND_FAILED", error),
        };
        if record.machine_id != self.identity.machine_id {
            return self.err(
                "VOLUME_ZFS_PEER_START_SEND_FAILED",
                format!(
                    "volume '{}/{}' is pinned to machine '{}', not local machine '{}'",
                    namespace.0, volume, record.machine_id, self.identity.machine_id
                ),
            );
        }
        let target = match self.find_active_machine(target_machine).await {
            Ok(record) => record,
            Err(error) => return self.err("VOLUME_ZFS_PEER_START_SEND_FAILED", error),
        };
        let driver = match self.local_zfs_driver().await {
            Ok(driver) => driver,
            Err(error) => return self.err("VOLUME_ZFS_PEER_START_SEND_FAILED", error),
        };
        let transfer_port = self.zfs_transfer_port;
        match send_zfs_stream_from_local(
            &record,
            &target,
            &driver,
            transfer_port,
            &self.identity.machine_id,
            snapshot,
            expected_guid,
            from_snapshot,
            from_snapshot_guid,
        )
        .await
        {
            Ok(result) => self.ok_with_payload(
                format!("sent {} bytes", result.bytes_transferred),
                Some(DaemonPayload::VolumeZfsPeerSend(VolumeZfsPeerSendPayload {
                    bytes_transferred: result.bytes_transferred,
                    snapshot_guid: result.snapshot_guid,
                })),
            ),
            Err(error) => self.err("VOLUME_ZFS_PEER_START_SEND_FAILED", error),
        }
    }

    pub(crate) async fn handle_volume_zfs_transfer_get(&self, id: &str) -> DaemonResponse {
        match self.zfs_transfer_store().load(id) {
            Ok(Some(record)) => self.ok_with_payload(
                serde_json::to_string_pretty(&record.info()).unwrap_or_else(|_| id.to_string()),
                Some(DaemonPayload::VolumeZfsTransfer(VolumeZfsTransferPayload {
                    transfer: record.info(),
                })),
            ),
            Ok(None) => self.err(
                "VOLUME_ZFS_TRANSFER_NOT_FOUND",
                format!("zfs transfer '{id}' not found"),
            ),
            Err(error) => self.err("VOLUME_ZFS_TRANSFER_GET_FAILED", error),
        }
    }

    pub(crate) async fn handle_volume_zfs_transfer_list(&self) -> DaemonResponse {
        match self.zfs_transfer_store().list() {
            Ok(records) => {
                let payload = VolumeZfsTransferListPayload {
                    transfers: records.iter().map(TransferRecord::info).collect(),
                };
                self.ok_with_payload(
                    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "ok".into()),
                    Some(DaemonPayload::VolumeZfsTransferList(payload)),
                )
            }
            Err(error) => self.err("VOLUME_ZFS_TRANSFER_LIST_FAILED", error),
        }
    }

    async fn inspect_local_volume_zfs(
        &self,
        namespace: &Namespace,
        volume: &str,
    ) -> Result<VolumeZfsInspectPayload, String> {
        let record = self.volume_record(namespace, volume).await?;
        let driver = self.local_zfs_driver().await?;
        let dataset = volume_dataset(driver.root_dataset(), namespace, volume);
        let info = driver
            .inspect_dataset(&dataset)
            .await
            .map_err(|error| error.to_string())?;
        Ok(VolumeZfsInspectPayload {
            namespace: namespace.0.clone(),
            volume: volume.to_string(),
            machine_id: record.machine_id,
            dataset,
            mountpoint: info.mountpoint.display().to_string(),
            quota: info.quota,
            used_bytes: info.used_bytes,
            snapshots: info
                .snapshots
                .into_iter()
                .map(|snapshot| VolumeZfsSnapshotInfo {
                    name: snapshot.name,
                    guid: snapshot.guid,
                })
                .collect(),
        })
    }

    async fn snapshot_local_volume_zfs(
        &self,
        namespace: &Namespace,
        volume: &str,
        snapshot: &str,
    ) -> Result<VolumeZfsSnapshotPayload, String> {
        let record = self.volume_record(namespace, volume).await?;
        let driver = self.local_zfs_driver().await?;
        let dataset = volume_dataset(driver.root_dataset(), namespace, volume);
        let snapshot = driver
            .create_snapshot(&dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        Ok(VolumeZfsSnapshotPayload {
            namespace: namespace.0.clone(),
            volume: volume.to_string(),
            machine_id: record.machine_id,
            dataset,
            snapshot: snapshot.name,
            guid: snapshot.guid,
        })
    }

    async fn snapshot_local_source_volume_zfs(
        &self,
        namespace: &Namespace,
        volume: &str,
        snapshot: &str,
    ) -> Result<VolumeZfsSnapshotPayload, String> {
        let record = self.volume_record(namespace, volume).await?;
        if record.machine_id != self.identity.machine_id {
            return Err(format!(
                "volume '{}/{}' is pinned to machine '{}', not local machine '{}'",
                namespace.0, volume, record.machine_id, self.identity.machine_id
            ));
        }
        self.snapshot_local_volume_zfs(namespace, volume, snapshot)
            .await
    }

    async fn snapshot_guid_local_volume_zfs(
        &self,
        namespace: &Namespace,
        volume: &str,
        snapshot: &str,
    ) -> Result<VolumeZfsSnapshotPayload, String> {
        let driver = self.local_zfs_driver().await?;
        let dataset = volume_dataset(driver.root_dataset(), namespace, volume);
        let guid = driver
            .snapshot_guid(&dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        Ok(VolumeZfsSnapshotPayload {
            namespace: namespace.0.clone(),
            volume: volume.to_string(),
            machine_id: self.identity.machine_id.clone(),
            dataset,
            snapshot: snapshot.to_string(),
            guid,
        })
    }

    async fn volume_record(
        &self,
        namespace: &Namespace,
        volume: &str,
    ) -> Result<VolumeRecord, String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no mesh is running".to_string())?;
        active
            .mesh
            .store
            .get_volume(namespace, volume)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("volume '{namespace}/{volume}' not found"))
    }

    async fn local_zfs_driver(&self) -> Result<ZfsDriver<TokioShellRunner>, String> {
        self.zfs_storage_driver()
            .await?
            .ok_or_else(|| "daemon has no [storage] zfs_root configured".to_string())
            .map(|driver| driver.as_ref().clone())
    }

    async fn forward_volume_zfs_inspect(
        &self,
        namespace: &str,
        volume: &str,
        machine: &str,
    ) -> DaemonResponse {
        let Some(machine) = self.find_machine(machine).await else {
            return self.err(
                "MACHINE_NOT_FOUND",
                format!("machine '{machine}' not found"),
            );
        };
        let client = match self.nats_node_rpc_client().await {
            Ok(client) => client,
            Err(error) => return self.err("VOLUME_ZFS_INSPECT_FAILED", error),
        };
        match client
            .request(
                NodeCommandSubject::volume_zfs_inspect(&machine.id),
                &ployz_api::DaemonRequest::VolumeZfsInspect {
                    namespace: namespace.to_string(),
                    volume: volume.to_string(),
                    machine: None,
                },
            )
            .await
        {
            Ok(response) => response,
            Err(error) => self.err("VOLUME_ZFS_INSPECT_FAILED", error.to_string()),
        }
    }

    async fn forward_volume_zfs_snapshot(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        volume: &str,
        snapshot: &str,
    ) -> DaemonResponse {
        let Some(machine) = self.find_machine(&machine_id.0).await else {
            return self.err(
                "MACHINE_NOT_FOUND",
                format!("machine '{}' not found", machine_id),
            );
        };
        let client = match self.nats_node_rpc_client().await {
            Ok(client) => client,
            Err(error) => return self.err("VOLUME_ZFS_SNAPSHOT_FAILED", error),
        };
        match client
            .request(
                NodeCommandSubject::volume_zfs_snapshot(&machine.id),
                &ployz_api::DaemonRequest::VolumeZfsSnapshot {
                    namespace: namespace.0.clone(),
                    volume: volume.to_string(),
                    snapshot: snapshot.to_string(),
                },
            )
            .await
        {
            Ok(response) => response,
            Err(error) => self.err("VOLUME_ZFS_SNAPSHOT_FAILED", error.to_string()),
        }
    }

    async fn find_machine(&self, machine: &str) -> Option<ployz_types::model::MachineMembership> {
        let active = self.active.as_ref()?;
        let machines = active.mesh.store.list_machines().await.ok()?;
        machines.into_iter().find(|record| record.id.0 == machine)
    }

    async fn find_active_machine(&self, machine: &str) -> Result<MachineMembership, String> {
        let record = self
            .find_machine(machine)
            .await
            .ok_or_else(|| format!("machine '{machine}' not found"))?;
        if record.lifecycle != MachineLifecycle::Active {
            return Err(format!(
                "machine '{}' is {}, expected active",
                record.id, record.lifecycle
            ));
        }
        Ok(record)
    }
}

fn volume_dataset(root: &str, namespace: &Namespace, volume: &str) -> String {
    format!("{root}/{}/{}", namespace.0, volume)
}

#[allow(clippy::too_many_arguments)]
async fn run_coordinated_zfs_transfer_inner(
    store: &TransferStore,
    transfer: &mut TransferRecord,
    record: &VolumeRecord,
    source: &MachineMembership,
    target: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    nats_rpc: Option<NatsNodeRpcClient>,
    transfer_port: u16,
    local_machine_id: &MachineId,
    snapshot: &str,
    from_snapshot: Option<&str>,
) -> Result<(), String> {
    store.update_stage(transfer, "snapshot")?;
    let snap_info = snapshot_on_machine(
        source,
        local_driver,
        nats_rpc.as_ref(),
        local_machine_id,
        &record.namespace,
        &record.volume_name,
        snapshot,
    )
    .await?;
    transfer.snapshot_guid = Some(snap_info.guid);
    store.save(transfer)?;

    if let Some(from_snapshot) = from_snapshot {
        store.update_stage(transfer, "verify-base")?;
        let from_guid = snapshot_guid_on_machine(
            source,
            local_driver,
            nats_rpc.as_ref(),
            local_machine_id,
            &record.namespace,
            &record.volume_name,
            from_snapshot,
        )
        .await?;
        let target_from_guid = snapshot_guid_on_machine(
            target,
            local_driver,
            nats_rpc.as_ref(),
            local_machine_id,
            &record.namespace,
            &record.volume_name,
            from_snapshot,
        )
        .await?;
        if target_from_guid.guid != from_guid.guid {
            return Err(format!(
                "target base snapshot guid {} did not match source {}",
                target_from_guid.guid, from_guid.guid
            ));
        }
        transfer.from_snapshot_guid = Some(from_guid.guid);
        store.save(transfer)?;
    }

    store.update_stage(transfer, "send")?;
    let result = start_send_on_machine(
        source,
        target,
        local_driver,
        nats_rpc.as_ref(),
        local_machine_id,
        transfer_port,
        record,
        snapshot,
        snap_info.guid,
        from_snapshot,
        transfer.from_snapshot_guid,
    )
    .await?;
    transfer.bytes_transferred = Some(result.bytes_transferred);
    store.save(transfer)?;

    store.update_stage(transfer, "verify")?;
    if result.snapshot_guid != snap_info.guid {
        return Err(format!(
            "target snapshot guid {} did not match source {}",
            result.snapshot_guid, snap_info.guid
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn send_zfs_stream_from_local(
    record: &VolumeRecord,
    target: &MachineMembership,
    driver: &ZfsDriver<TokioShellRunner>,
    transfer_port: u16,
    local_machine_id: &MachineId,
    snapshot: &str,
    expected_guid: u64,
    from_snapshot: Option<&str>,
    from_snapshot_guid: Option<u64>,
) -> Result<SendResult, String> {
    let dataset = volume_dataset(
        driver.root_dataset(),
        &record.namespace,
        &record.volume_name,
    );
    let actual_guid = driver
        .snapshot_guid(&dataset, snapshot)
        .await
        .map_err(|err| err.to_string())?;
    if actual_guid != expected_guid {
        return Err(format!(
            "local snapshot '{dataset}@{snapshot}' guid {actual_guid} did not match expected {expected_guid}"
        ));
    }
    if let Some(from_snapshot) = from_snapshot
        && let Some(expected_from) = from_snapshot_guid
    {
        let actual_from = driver
            .snapshot_guid(&dataset, from_snapshot)
            .await
            .map_err(|err| err.to_string())?;
        if actual_from != expected_from {
            return Err(format!(
                "local base snapshot '{dataset}@{from_snapshot}' guid {actual_from} did not match expected {expected_from}"
            ));
        }
    }
    let address =
        std::net::SocketAddr::new(std::net::IpAddr::V6(target.overlay_ip.0), transfer_port);
    let stream = TcpStream::connect(address)
        .await
        .map_err(|err| format!("connect zfs transfer target {address}: {err}"))?;
    let (reader, mut writer) = stream.into_split();
    let open = ZfsTransferOpen {
        namespace: record.namespace.0.clone(),
        volume: record.volume_name.clone(),
        snapshot: snapshot.to_string(),
        expected_guid,
        source_machine_id: Some(local_machine_id.clone()),
        from_snapshot: from_snapshot.map(str::to_string),
        from_snapshot_guid,
    };
    let mut header =
        serde_json::to_string(&open).map_err(|err| format!("encode transfer open: {err}"))?;
    header.push('\n');
    writer
        .write_all(header.as_bytes())
        .await
        .map_err(|err| format!("write transfer open: {err}"))?;

    let mut send = match from_snapshot {
        Some(from_snapshot) => driver
            .spawn_send_incremental(&dataset, from_snapshot, snapshot)
            .map_err(|err| err.to_string())?,
        None => driver
            .spawn_send_full(&dataset, snapshot)
            .map_err(|err| err.to_string())?,
    };
    let Some(mut stdout) = send.stdout.take() else {
        return Err("zfs send stdout was not piped".to_string());
    };
    let copy_result = tokio::io::copy(&mut stdout, &mut writer).await;
    drop(stdout);
    let bytes = match copy_result {
        Ok(bytes) => bytes,
        Err(err) => {
            let copy_error = format!("copy zfs send stream: {err}");
            // Always reap the child even if kill fails, so we never leave a
            // zombie behind on stream errors.
            if let Err(kill_err) = send.kill().await {
                tracing::warn!(error = %kill_err, "failed to kill zfs send after copy error");
            }
            if let Err(wait_err) = send.wait_with_output().await {
                return Err(format!("{copy_error}; failed to reap zfs send: {wait_err}"));
            }
            return Err(copy_error);
        }
    };
    writer
        .shutdown()
        .await
        .map_err(|err| format!("shutdown zfs transfer stream: {err}"))?;
    let output = send
        .wait_with_output()
        .await
        .map_err(|err| format!("wait for zfs send: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "zfs send failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let mut reader = BufReader::new(reader).take((MAX_ACK_BYTES + 1) as u64);
    let mut buf = Vec::new();
    let bytes_read = tokio::time::timeout(ACK_READ_TIMEOUT, reader.read_until(b'\n', &mut buf))
        .await
        .map_err(|_| "timed out waiting for zfs transfer response".to_string())?
        .map_err(|err| format!("read zfs transfer response: {err}"))?;
    if bytes_read == 0 {
        return Err("connection closed before zfs transfer response".to_string());
    }
    if buf.len() > MAX_ACK_BYTES {
        return Err(format!(
            "zfs transfer response exceeded {MAX_ACK_BYTES} bytes"
        ));
    }
    let response: ZfsTransferReceived = serde_json::from_slice(&buf)
        .map_err(|err| format!("decode zfs transfer response: {err}"))?;
    if !response.ok {
        return Err(response.message);
    }
    let Some(snapshot_guid) = response.snapshot_guid else {
        return Err("target did not report snapshot guid".to_string());
    };
    Ok(SendResult {
        bytes_transferred: bytes,
        snapshot_guid,
    })
}

async fn snapshot_on_machine(
    machine: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    nats_rpc: Option<&NatsNodeRpcClient>,
    local_machine_id: &MachineId,
    namespace: &Namespace,
    volume: &str,
    snapshot: &str,
) -> Result<VolumeZfsSnapshotPayload, String> {
    if machine.id == *local_machine_id {
        let driver =
            local_driver.ok_or_else(|| "local zfs driver is not configured".to_string())?;
        let dataset = volume_dataset(driver.root_dataset(), namespace, volume);
        let snap_info = driver
            .create_snapshot(&dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(VolumeZfsSnapshotPayload {
            namespace: namespace.0.clone(),
            volume: volume.to_string(),
            machine_id: machine.id.clone(),
            dataset,
            snapshot: snap_info.name,
            guid: snap_info.guid,
        });
    }

    let nats_rpc = nats_rpc.ok_or_else(|| "NATS RPC client is not configured".to_string())?;
    let response = nats_rpc
        .request(
            NodeCommandSubject::volume_zfs_snapshot(&machine.id),
            &ployz_api::DaemonRequest::VolumeZfsPeerSnapshot {
                namespace: namespace.0.clone(),
                volume: volume.to_string(),
                snapshot: snapshot.to_string(),
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    expect_snapshot_payload(response, "remote peer snapshot")
}

async fn snapshot_guid_on_machine(
    machine: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    nats_rpc: Option<&NatsNodeRpcClient>,
    local_machine_id: &MachineId,
    namespace: &Namespace,
    volume: &str,
    snapshot: &str,
) -> Result<VolumeZfsSnapshotPayload, String> {
    if machine.id == *local_machine_id {
        let driver =
            local_driver.ok_or_else(|| "local zfs driver is not configured".to_string())?;
        let dataset = volume_dataset(driver.root_dataset(), namespace, volume);
        let guid = driver
            .snapshot_guid(&dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(VolumeZfsSnapshotPayload {
            namespace: namespace.0.clone(),
            volume: volume.to_string(),
            machine_id: machine.id.clone(),
            dataset,
            snapshot: snapshot.to_string(),
            guid,
        });
    }

    let nats_rpc = nats_rpc.ok_or_else(|| "NATS RPC client is not configured".to_string())?;
    let response = nats_rpc
        .request(
            NodeCommandSubject::volume_zfs_snapshot_guid(&machine.id),
            &ployz_api::DaemonRequest::VolumeZfsPeerSnapshotGuid {
                namespace: namespace.0.clone(),
                volume: volume.to_string(),
                snapshot: snapshot.to_string(),
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    expect_snapshot_payload(response, "remote peer snapshot guid")
}

#[allow(clippy::too_many_arguments)]
async fn start_send_on_machine(
    source: &MachineMembership,
    target: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    nats_rpc: Option<&NatsNodeRpcClient>,
    local_machine_id: &MachineId,
    transfer_port: u16,
    record: &VolumeRecord,
    snapshot: &str,
    expected_guid: u64,
    from_snapshot: Option<&str>,
    from_snapshot_guid: Option<u64>,
) -> Result<SendResult, String> {
    if source.id == *local_machine_id {
        let driver =
            local_driver.ok_or_else(|| "local zfs driver is not configured".to_string())?;
        return send_zfs_stream_from_local(
            record,
            target,
            driver,
            transfer_port,
            local_machine_id,
            snapshot,
            expected_guid,
            from_snapshot,
            from_snapshot_guid,
        )
        .await;
    }

    let nats_rpc = nats_rpc.ok_or_else(|| "NATS RPC client is not configured".to_string())?;
    let response = nats_rpc
        .request(
            NodeCommandSubject::volume_zfs_start_send(&source.id),
            &ployz_api::DaemonRequest::VolumeZfsPeerStartSend {
                namespace: record.namespace.0.clone(),
                volume: record.volume_name.clone(),
                snapshot: snapshot.to_string(),
                target_machine: target.id.0.clone(),
                expected_guid,
                from_snapshot: from_snapshot.map(str::to_string),
                from_snapshot_guid,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    if !response.ok {
        return Err(format!(
            "remote peer start-send failed [{}]: {}",
            response.code, response.message
        ));
    }
    let Some(DaemonPayload::VolumeZfsPeerSend(payload)) = response.payload else {
        return Err("remote peer start-send response missing payload".to_string());
    };
    Ok(SendResult {
        bytes_transferred: payload.bytes_transferred,
        snapshot_guid: payload.snapshot_guid,
    })
}

fn expect_snapshot_payload(
    response: DaemonResponse,
    operation: &str,
) -> Result<VolumeZfsSnapshotPayload, String> {
    if !response.ok {
        return Err(format!(
            "{operation} failed [{}]: {}",
            response.code, response.message
        ));
    }
    let Some(DaemonPayload::VolumeZfsSnapshot(payload)) = response.payload else {
        return Err(format!("{operation} response missing payload"));
    };
    Ok(payload)
}

fn finalize_zfs_transfer(
    store: &TransferStore,
    transfer: &mut TransferRecord,
    result: Result<(), String>,
) {
    let transfer_id = transfer.id.clone();
    match result {
        Ok(()) => {
            if let Err(error) = store.update_stage(transfer, "complete") {
                tracing::warn!(%error, transfer_id, "failed to record complete stage");
            }
            if let Err(error) = store.update_status(transfer, TransferStatus::Succeeded, None) {
                tracing::warn!(%error, transfer_id, "failed to record success status");
            }
        }
        Err(error) => {
            if let Err(save_err) =
                store.update_status(transfer, TransferStatus::Failed, Some(error))
            {
                tracing::warn!(%save_err, transfer_id, "failed to record failed status");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TransferStatus, TransferStore, finalize_zfs_transfer};
    use ployz_types::model::MachineId;
    use ployz_types::spec::Namespace;
    use std::path::PathBuf;

    fn tmp_root(label: &str) -> PathBuf {
        let id = super::unique_transfer_id(0).expect("unique id");
        std::env::temp_dir().join(format!("ployz-zfs-transfer-test-{label}-{id}"))
    }

    fn begin(store: &TransferStore) -> super::TransferRecord {
        store
            .begin(
                &Namespace("default".into()),
                "data",
                MachineId("source".into()),
                MachineId("target".into()),
                "snap".into(),
                None,
            )
            .expect("begin transfer")
    }

    #[test]
    fn startup_recovery_marks_running_transfers_interrupted() {
        let root = tmp_root("startup-recovery");
        let store = TransferStore::new(root.clone());
        let transfer = begin(&store);
        assert_eq!(transfer.status, TransferStatus::Running);

        let count = store.recover_startup().expect("recover startup");
        assert_eq!(count, 1);
        let loaded = store
            .load(&transfer.id)
            .expect("load")
            .expect("record exists");
        assert_eq!(loaded.status, TransferStatus::Interrupted);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn load_rejects_path_segment_id() {
        let store = TransferStore::new(tmp_root("reject-id"));
        for bad in ["../etc/passwd", "foo/bar", "with space", "", "."] {
            let err = store
                .load(bad)
                .expect_err(&format!("id '{bad}' should be rejected"));
            assert!(err.contains("transfer id"), "got: {err}");
        }
    }

    #[test]
    fn list_orders_by_started_at_desc_then_id_asc() {
        let root = tmp_root("list-order");
        let store = TransferStore::new(root.clone());
        let mut a = begin(&store);
        let mut b = begin(&store);
        let mut c = begin(&store);
        // begin uses nanos in the id, so ids grow with call order
        assert!(a.id < b.id && b.id < c.id);

        a.started_at = 100;
        b.started_at = 200;
        c.started_at = 200;
        store.save(&a).expect("save a");
        store.save(&b).expect("save b");
        store.save(&c).expect("save c");

        let listed = store.list().expect("list");
        let ids: Vec<&str> = listed.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec![b.id.as_str(), c.id.as_str(), a.id.as_str()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finalize_records_complete_stage_and_succeeded_status() {
        let root = tmp_root("finalize-ok");
        let store = TransferStore::new(root.clone());
        let mut transfer = begin(&store);
        store
            .update_stage(&mut transfer, "snapshot")
            .expect("stage snapshot");
        store
            .update_stage(&mut transfer, "send")
            .expect("stage send");

        finalize_zfs_transfer(&store, &mut transfer, Ok(()));

        let loaded = store
            .load(&transfer.id)
            .expect("load")
            .expect("record exists");
        assert_eq!(loaded.status, TransferStatus::Succeeded);
        assert_eq!(loaded.stage, "complete");
        assert!(loaded.last_error.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finalize_captures_last_error_on_failure() {
        let root = tmp_root("finalize-err");
        let store = TransferStore::new(root.clone());
        let mut transfer = begin(&store);

        finalize_zfs_transfer(&store, &mut transfer, Err("boom".into()));

        let loaded = store
            .load(&transfer.id)
            .expect("load")
            .expect("record exists");
        assert_eq!(loaded.status, TransferStatus::Failed);
        assert_eq!(loaded.last_error.as_deref(), Some("boom"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_transfer_preserves_prior_failure_error() {
        let root = tmp_root("interrupt-after-failure");
        let store = TransferStore::new(root.clone());
        let mut transfer = begin(&store);
        store
            .update_status(
                &mut transfer,
                TransferStatus::Failed,
                Some("send failed".into()),
            )
            .expect("record transfer failure");

        store
            .update_status(&mut transfer, TransferStatus::Interrupted, None)
            .expect("record interruption");

        let loaded = store
            .load(&transfer.id)
            .expect("load")
            .expect("record exists");
        assert_eq!(loaded.status, TransferStatus::Interrupted);
        assert_eq!(loaded.last_error.as_deref(), Some("send failed"));
        assert_eq!(
            loaded.info().last_error.as_deref(),
            Some("send failed"),
            "operator-facing payload should preserve the failure audience"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
