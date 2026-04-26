use std::path::PathBuf;

use ployz_api::{
    DaemonPayload, DaemonResponse, VolumeZfsInspectPayload, VolumeZfsSnapshotInfo,
    VolumeZfsSnapshotPayload, VolumeZfsTransferInfo, VolumeZfsTransferListPayload,
    VolumeZfsTransferPayload,
};
use ployz_runtime_backends::storage::{TokioShellRunner, ZfsDriver};
use ployz_store_api::{DeployStore, MachineStore};
use ployz_types::model::{MachineId, VolumeRecord};
use ployz_types::spec::Namespace;
use ployz_types::time::now_unix_secs;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::daemon::DaemonState;
use crate::daemon::handlers::peer_rpc::overlay_rpc;
use crate::daemon::handlers::volume::transfer_listener::{ZfsTransferOpen, ZfsTransferReceived};

const TRANSFERS_DIR_NAME: &str = "zfs-transfers";

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
            id: unique_transfer_id(now),
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
        record.status = status;
        if let Some(last_error) = last_error {
            record.last_error = Some(last_error);
        }
        record.updated_at = now_unix_secs();
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

    fn reconcile_startup(&self) -> Result<usize, String> {
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

fn read_transfer(path: &std::path::Path) -> Result<TransferRecord, String> {
    let body = std::fs::read(path)
        .map_err(|err| format!("read zfs transfer '{}': {err}", path.display()))?;
    serde_json::from_slice(&body)
        .map_err(|err| format!("decode zfs transfer '{}': {err}", path.display()))
}

fn unique_transfer_id(now: u64) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("zfs-transfer-{now}-{}-{nanos}", std::process::id())
}

impl DaemonState {
    fn zfs_transfer_store(&self) -> TransferStore {
        TransferStore::new(self.data_dir.clone())
    }

    pub(crate) async fn reconcile_zfs_transfers_on_startup(&self) {
        match self.zfs_transfer_store().reconcile_startup() {
            Ok(count) if count > 0 => {
                tracing::warn!(count, "marked running zfs transfers interrupted")
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "failed to reconcile zfs transfers"),
        }
    }

    pub(crate) async fn handle_volume_zfs_inspect(
        &self,
        namespace: &str,
        volume: &str,
        machine: Option<&str>,
    ) -> DaemonResponse {
        if let Some(machine) = machine
            && machine != self.identity.machine_id.0
        {
            return self
                .forward_volume_zfs_inspect(namespace, volume, machine)
                .await;
        }
        match self
            .inspect_local_volume_zfs(&Namespace(namespace.to_string()), volume)
            .await
        {
            Ok(payload) => self.ok_with_payload(
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "ok".into()),
                Some(DaemonPayload::VolumeZfsInspect(payload)),
            ),
            Err(error) => self.err("VOLUME_ZFS_INSPECT_FAILED", error),
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
            Err(error) => self.err("VOLUME_ZFS_SNAPSHOT_FAILED", error),
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
        if record.machine_id != self.identity.machine_id {
            return self
                .forward_volume_zfs_send(
                    &record.machine_id,
                    &namespace,
                    volume,
                    snapshot,
                    target_machine,
                    from_snapshot,
                )
                .await;
        }
        let store = self.zfs_transfer_store();
        let mut transfer = match store.begin(
            &namespace,
            volume,
            record.machine_id.clone(),
            MachineId(target_machine.to_string()),
            snapshot.to_string(),
            from_snapshot.map(str::to_string),
        ) {
            Ok(transfer) => transfer,
            Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
        };

        let result = self
            .run_transfer_preflight(&store, &mut transfer, &record, snapshot, from_snapshot)
            .await;
        match result {
            Ok(()) => {
                let transfer_result = self
                    .send_zfs_stream(&store, &mut transfer, &record, snapshot, from_snapshot)
                    .await;
                match transfer_result {
                    Ok(()) => {
                        let _ = store.update_stage(&mut transfer, "complete");
                        let _ = store.update_status(&mut transfer, TransferStatus::Succeeded, None);
                    }
                    Err(error) => {
                        let _ =
                            store.update_status(&mut transfer, TransferStatus::Failed, Some(error));
                    }
                }
            }
            Err(error) => {
                let _ = store.update_status(&mut transfer, TransferStatus::Failed, Some(error));
            }
        }

        self.ok_with_payload(
            serde_json::to_string_pretty(&transfer.info()).unwrap_or_else(|_| transfer.id.clone()),
            Some(DaemonPayload::VolumeZfsTransfer(VolumeZfsTransferPayload {
                transfer: transfer.info(),
            })),
        )
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

    async fn run_transfer_preflight(
        &self,
        store: &TransferStore,
        transfer: &mut TransferRecord,
        record: &VolumeRecord,
        snapshot: &str,
        from_snapshot: Option<&str>,
    ) -> Result<(), String> {
        store.update_stage(transfer, "snapshot")?;
        let snapshot_payload = if record.machine_id == self.identity.machine_id {
            self.snapshot_local_volume_zfs(&record.namespace, &record.volume_name, snapshot)
                .await?
        } else {
            self.snapshot_remote_volume_zfs(
                &record.machine_id,
                &record.namespace,
                &record.volume_name,
                snapshot,
            )
            .await?
        };
        transfer.snapshot_guid = Some(snapshot_payload.guid);
        store.save(transfer)?;

        if let Some(from_snapshot) = from_snapshot {
            store.update_stage(transfer, "verify-base")?;
            let from_guid = if record.machine_id == self.identity.machine_id {
                self.snapshot_guid_local(&record.namespace, &record.volume_name, from_snapshot)
                    .await?
            } else {
                self.snapshot_guid_remote(
                    &record.machine_id,
                    &record.namespace,
                    &record.volume_name,
                    from_snapshot,
                )
                .await?
            };
            transfer.from_snapshot_guid = Some(from_guid);
            store.save(transfer)?;
        }
        store.update_stage(transfer, "connect")?;
        Ok(())
    }

    async fn send_zfs_stream(
        &self,
        store: &TransferStore,
        transfer: &mut TransferRecord,
        record: &VolumeRecord,
        snapshot: &str,
        from_snapshot: Option<&str>,
    ) -> Result<(), String> {
        if record.machine_id != self.identity.machine_id {
            return Err(
                "zfs transfer must be initiated on the source machine in this build".into(),
            );
        }
        store.update_stage(transfer, "send")?;
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no mesh is running".to_string())?;
        let target = active
            .mesh
            .store
            .list_machines()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|machine| machine.id == transfer.target_machine)
            .ok_or_else(|| format!("target machine '{}' not found", transfer.target_machine))?;
        let driver = self.local_zfs_driver().await?;
        let dataset = volume_dataset(
            driver.root_dataset(),
            &record.namespace,
            &record.volume_name,
        );
        let mut send = match from_snapshot {
            Some(from_snapshot) => driver
                .spawn_send_incremental(&dataset, from_snapshot, snapshot)
                .map_err(|error| error.to_string())?,
            None => driver
                .spawn_send_full(&dataset, snapshot)
                .map_err(|error| error.to_string())?,
        };
        let Some(mut stdout) = send.stdout.take() else {
            return Err("zfs send stdout was not piped".to_string());
        };
        let port = self
            .zfs_transfer_port()
            .map_err(|error| error.to_string())?;
        let address = std::net::SocketAddr::new(std::net::IpAddr::V6(target.overlay_ip.0), port);
        let stream = TcpStream::connect(address)
            .await
            .map_err(|error| format!("connect zfs transfer target {address}: {error}"))?;
        let (reader, mut writer) = stream.into_split();
        let open = ZfsTransferOpen {
            namespace: record.namespace.0.clone(),
            volume: record.volume_name.clone(),
            snapshot: snapshot.to_string(),
            expected_guid: transfer
                .snapshot_guid
                .ok_or_else(|| "transfer missing snapshot guid".to_string())?,
        };
        let mut header = serde_json::to_string(&open)
            .map_err(|error| format!("encode transfer open: {error}"))?;
        header.push('\n');
        writer
            .write_all(header.as_bytes())
            .await
            .map_err(|error| format!("write transfer open: {error}"))?;
        let bytes = tokio::io::copy(&mut stdout, &mut writer)
            .await
            .map_err(|error| format!("copy zfs send stream: {error}"))?;
        transfer.bytes_transferred = Some(bytes);
        store.save(transfer)?;
        writer
            .shutdown()
            .await
            .map_err(|error| format!("shutdown zfs transfer stream: {error}"))?;
        let output = send
            .wait_with_output()
            .await
            .map_err(|error| format!("wait for zfs send: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "zfs send failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        store.update_stage(transfer, "verify")?;
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .map_err(|error| format!("read zfs transfer response: {error}"))?;
        let response: ZfsTransferReceived = serde_json::from_str(&line)
            .map_err(|error| format!("decode zfs transfer response: {error}"))?;
        if !response.ok {
            return Err(response.message);
        }
        if response.snapshot_guid != transfer.snapshot_guid {
            return Err(format!(
                "target snapshot guid {:?} did not match source {:?}",
                response.snapshot_guid, transfer.snapshot_guid
            ));
        }
        Ok(())
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

    async fn snapshot_guid_local(
        &self,
        namespace: &Namespace,
        volume: &str,
        snapshot: &str,
    ) -> Result<u64, String> {
        let driver = self.local_zfs_driver().await?;
        let dataset = volume_dataset(driver.root_dataset(), namespace, volume);
        driver
            .snapshot_guid(&dataset, snapshot)
            .await
            .map_err(|error| error.to_string())
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
        match overlay_rpc(
            machine.overlay_ip,
            self.peer_control_port()
                .unwrap_or(self.remote_control_port + 1),
            ployz_api::DaemonRequest::VolumeZfsInspect {
                namespace: namespace.to_string(),
                volume: volume.to_string(),
                machine: None,
            },
        )
        .await
        {
            Ok(response) => response,
            Err(error) => self.err("VOLUME_ZFS_INSPECT_FAILED", error),
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
        match overlay_rpc(
            machine.overlay_ip,
            self.peer_control_port()
                .unwrap_or(self.remote_control_port + 1),
            ployz_api::DaemonRequest::VolumeZfsSnapshot {
                namespace: namespace.0.clone(),
                volume: volume.to_string(),
                snapshot: snapshot.to_string(),
            },
        )
        .await
        {
            Ok(response) => response,
            Err(error) => self.err("VOLUME_ZFS_SNAPSHOT_FAILED", error),
        }
    }

    async fn forward_volume_zfs_send(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        volume: &str,
        snapshot: &str,
        target_machine: &str,
        from_snapshot: Option<&str>,
    ) -> DaemonResponse {
        let Some(machine) = self.find_machine(&machine_id.0).await else {
            return self.err(
                "MACHINE_NOT_FOUND",
                format!("machine '{}' not found", machine_id),
            );
        };
        match overlay_rpc(
            machine.overlay_ip,
            self.peer_control_port()
                .unwrap_or(self.remote_control_port + 1),
            ployz_api::DaemonRequest::VolumeZfsSend {
                namespace: namespace.0.clone(),
                volume: volume.to_string(),
                snapshot: snapshot.to_string(),
                target_machine: target_machine.to_string(),
                from_snapshot: from_snapshot.map(str::to_string),
            },
        )
        .await
        {
            Ok(response) => response,
            Err(error) => self.err("VOLUME_ZFS_SEND_FAILED", error),
        }
    }

    async fn snapshot_remote_volume_zfs(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        volume: &str,
        snapshot: &str,
    ) -> Result<VolumeZfsSnapshotPayload, String> {
        let response = self
            .forward_volume_zfs_snapshot(machine_id, namespace, volume, snapshot)
            .await;
        if !response.ok {
            return Err(format!(
                "remote snapshot failed [{}]: {}",
                response.code, response.message
            ));
        }
        let Some(DaemonPayload::VolumeZfsSnapshot(payload)) = response.payload else {
            return Err("remote snapshot response missing payload".to_string());
        };
        Ok(payload)
    }

    async fn snapshot_guid_remote(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        volume: &str,
        snapshot: &str,
    ) -> Result<u64, String> {
        let response = self
            .forward_volume_zfs_snapshot(machine_id, namespace, volume, snapshot)
            .await;
        if !response.ok {
            return Err(format!(
                "remote snapshot guid failed [{}]: {}",
                response.code, response.message
            ));
        }
        let Some(DaemonPayload::VolumeZfsSnapshot(payload)) = response.payload else {
            return Err("remote snapshot guid response missing payload".to_string());
        };
        Ok(payload.guid)
    }

    async fn find_machine(&self, machine: &str) -> Option<ployz_types::model::MachineRecord> {
        let active = self.active.as_ref()?;
        let machines = active.mesh.store.list_machines().await.ok()?;
        machines.into_iter().find(|record| record.id.0 == machine)
    }
}

fn volume_dataset(root: &str, namespace: &Namespace, volume: &str) -> String {
    format!("{root}/{}/{}", namespace.0, volume)
}

#[cfg(test)]
mod tests {
    use super::{TransferStatus, TransferStore};
    use ployz_types::model::MachineId;
    use ployz_types::spec::Namespace;

    #[test]
    fn startup_reconciliation_marks_running_transfers_interrupted() {
        let root = std::env::temp_dir().join(format!(
            "ployz-zfs-transfer-test-{}",
            super::unique_transfer_id(0)
        ));
        let store = TransferStore::new(root.clone());
        let transfer = store
            .begin(
                &Namespace("default".into()),
                "data",
                MachineId("source".into()),
                MachineId("target".into()),
                "snap".into(),
                None,
            )
            .expect("begin transfer");
        assert_eq!(transfer.status, TransferStatus::Running);

        let count = store.reconcile_startup().expect("reconcile");
        assert_eq!(count, 1);
        let loaded = store
            .load(&transfer.id)
            .expect("load")
            .expect("record exists");
        assert_eq!(loaded.status, TransferStatus::Interrupted);
        let _ = std::fs::remove_dir_all(root);
    }
}
