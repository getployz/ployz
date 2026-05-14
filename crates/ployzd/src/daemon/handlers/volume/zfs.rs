mod responses;
mod transfer_execution;

use std::time::Duration;

use ployz_api::{
    DaemonPayload, DaemonResponse, VolumeZfsClonePayload, VolumeZfsInspectPayload,
    VolumeZfsPeerSendPayload, VolumeZfsSnapshotInfo, VolumeZfsSnapshotPayload,
    VolumeZfsTransferListPayload, VolumeZfsTransferPayload,
};
use ployz_model::{MachineId, MachineLifecycle, MachineMembership, VolumeRecord};
use ployz_node_api::{NodeVolumeZfsInspectPayload, NodeVolumeZfsSnapshotPayload};
use ployz_node_runtime::{
    NodeRpcPolicy, VolumeZfsNodeClient, VolumeZfsNodePayload, VolumeZfsNodeResponse,
};
use ployz_spec::{Namespace, VolumeScope};
use ployz_storage_api::{CloneMetadata, DatasetSpec};
use ployz_store_api::{DeployStore, MachineMembershipStore};
use ployz_time::now_unix_secs;
use ployz_volume_zfs::{
    ClaimedTransfer, MoveClaimOutcome, TokioShellRunner, TransferStore, ZfsDriver,
    validate_transfer_id, wait_for_claimed_transfer_record,
};
#[cfg(test)]
use ployz_volume_zfs::{TransferRecord, TransferStatus, move_claim_key, unique_transfer_id};

use crate::daemon::DaemonState;
use crate::daemon::node_rpc::NatsVolumeZfsRpcTransport;
use responses::transfer_info;
use transfer_execution::{
    finalize_zfs_transfer, run_coordinated_zfs_transfer_inner, send_zfs_stream_from_local,
};

const ZFS_SEND_RPC_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

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
        let namespace = match Namespace::try_new(namespace) {
            Ok(namespace) => namespace,
            Err(error) => return self.err("VOLUME_ZFS_INSPECT_FAILED", error),
        };
        let target_machine: Option<String> = match machine {
            Some(machine) => Some(machine.to_string()),
            None => match self.volume_record(&namespace, volume).await {
                Ok(record) => Some(record.machine_id.as_str().to_string()),
                Err(error) => return self.err("VOLUME_ZFS_INSPECT_FAILED", error),
            },
        };
        if let Some(machine) = target_machine
            && machine != self.identity.machine_id.as_str()
        {
            return self
                .forward_volume_zfs_inspect(&namespace.as_str(), volume, &machine)
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
        let namespace = match Namespace::try_new(namespace) {
            Ok(namespace) => namespace,
            Err(error) => return self.err("VOLUME_ZFS_SNAPSHOT_FAILED", error),
        };
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn handle_deploy_node_clone_volume(
        &self,
        namespace: &str,
        deploy_id: &str,
        volume: &str,
        source_namespace: &str,
        source_volume: &str,
        snapshot: &str,
        quota: &str,
        mode: &str,
        owner: &str,
    ) -> DaemonResponse {
        let namespace = Namespace::new(namespace.to_string());
        let source_namespace = Namespace::new(source_namespace.to_string());
        match self
            .clone_local_volume_zfs(
                &namespace,
                deploy_id,
                volume,
                &source_namespace,
                source_volume,
                snapshot,
                quota,
                mode,
                owner,
            )
            .await
        {
            Ok(payload) => self.ok_with_payload(
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "ok".into()),
                Some(DaemonPayload::VolumeZfsClone(payload)),
            ),
            Err(error) => self.err("VOLUME_ZFS_CLONE_FAILED", error),
        }
    }

    pub(crate) async fn handle_deploy_node_cleanup_uncommitted_volume_clone(
        &self,
        namespace: &str,
        deploy_id: &str,
        volume: &str,
        source_namespace: &str,
        source_volume: &str,
        snapshot: &str,
    ) -> DaemonResponse {
        let namespace = Namespace::new(namespace.to_string());
        let source_namespace = Namespace::new(source_namespace.to_string());
        match self
            .cleanup_uncommitted_local_volume_clone_zfs(
                &namespace,
                deploy_id,
                volume,
                &source_namespace,
                source_volume,
                snapshot,
            )
            .await
        {
            Ok(()) => self.ok("uncommitted volume clone cleaned up"),
            Err(error) => self.err("VOLUME_ZFS_CLONE_CLEANUP_FAILED", error),
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
        let namespace = match Namespace::try_new(namespace) {
            Ok(namespace) => namespace,
            Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
        };
        let record = match self.volume_record(&namespace, volume).await {
            Ok(record) => record,
            Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
        };
        if record.scope != VolumeScope::Single {
            return self.err(
                "VOLUME_ZFS_SCOPE_NOT_SUPPORTED",
                format!(
                    "volume '{}/{}' has scope {:?}; only Single is supported in this build",
                    namespace.as_str(),
                    volume,
                    record.scope
                ),
            );
        }
        let source = match self
            .find_volume_move_source_machine(&record.machine_id.as_str())
            .await
        {
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
        let volume_rpc = if needs_nats_rpc {
            match self.nats_node_rpc_client().await {
                Ok(client) => Some(
                    VolumeZfsNodeClient::new(NatsVolumeZfsRpcTransport::new(client)).with_policy(
                        NodeRpcPolicy {
                            timeout: ZFS_SEND_RPC_TIMEOUT,
                        },
                    ),
                ),
                Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
            }
        } else {
            None
        };

        let store = self.zfs_transfer_store();
        match store.find_reusable(
            &namespace,
            volume,
            &source.id,
            &target.id,
            snapshot,
            from_snapshot,
        ) {
            Ok(Some(transfer)) => {
                let info = transfer_info(&transfer);
                return self.ok_with_payload(
                    serde_json::to_string_pretty(&info).unwrap_or_else(|_| info.id.clone()),
                    Some(DaemonPayload::VolumeZfsTransfer(VolumeZfsTransferPayload {
                        transfer: info,
                    })),
                );
            }
            Ok(None) => {}
            Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
        }
        let transfer = {
            let mut transfer = None;
            for _ in 0..2 {
                let transfer_id = match store.new_transfer_id() {
                    Ok(transfer_id) => transfer_id,
                    Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
                };
                match store.create_move_claim(
                    &namespace,
                    volume,
                    &source.id,
                    &target.id,
                    snapshot,
                    from_snapshot,
                    &transfer_id,
                ) {
                    Ok(MoveClaimOutcome::Created) => {
                        match store.begin_with_id(
                            transfer_id,
                            &namespace,
                            volume,
                            source.id.clone(),
                            target.id.clone(),
                            snapshot.to_string(),
                            from_snapshot.map(str::to_string),
                            now_unix_secs(),
                        ) {
                            Ok(record) => {
                                transfer = Some(record);
                                break;
                            }
                            Err(error) => {
                                store.delete_move_claim(
                                    &namespace,
                                    volume,
                                    &source.id,
                                    &target.id,
                                    snapshot,
                                    from_snapshot,
                                );
                                return self.err("VOLUME_ZFS_SEND_FAILED", error);
                            }
                        }
                    }
                    Ok(MoveClaimOutcome::Exists(existing_id)) => {
                        if let Err(error) = validate_transfer_id(&existing_id) {
                            tracing::warn!(
                                transfer_id = %existing_id,
                                %error,
                                "deleting stale zfs transfer claim with invalid transfer id"
                            );
                            store.delete_move_claim(
                                &namespace,
                                volume,
                                &source.id,
                                &target.id,
                                snapshot,
                                from_snapshot,
                            );
                            continue;
                        }
                        match wait_for_claimed_transfer_record(&store, &existing_id).await {
                            Ok(ClaimedTransfer::Running(existing)) => {
                                let info = transfer_info(&existing);
                                return self.ok_with_payload(
                                    serde_json::to_string_pretty(&info)
                                        .unwrap_or_else(|_| info.id.clone()),
                                    Some(DaemonPayload::VolumeZfsTransfer(
                                        VolumeZfsTransferPayload { transfer: info },
                                    )),
                                );
                            }
                            Ok(ClaimedTransfer::Terminal(existing)) => {
                                tracing::warn!(
                                    transfer_id = %existing.id,
                                    status = ?existing.status(),
                                    "deleting stale zfs transfer claim for terminal transfer"
                                );
                                store.delete_move_claim(
                                    &namespace,
                                    volume,
                                    &source.id,
                                    &target.id,
                                    snapshot,
                                    from_snapshot,
                                );
                            }
                            Ok(ClaimedTransfer::MissingAfterWait) => {
                                tracing::warn!(
                                    transfer_id = %existing_id,
                                    "deleting stale zfs transfer claim for missing transfer"
                                );
                                store.delete_move_claim(
                                    &namespace,
                                    volume,
                                    &source.id,
                                    &target.id,
                                    snapshot,
                                    from_snapshot,
                                );
                            }
                            Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
                        }
                    }
                    Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
                }
            }
            match transfer {
                Some(transfer) => transfer,
                None => {
                    return self.err(
                        "VOLUME_ZFS_SEND_FAILED",
                        "zfs transfer claim could not be acquired after stale claim cleanup",
                    );
                }
            }
        };

        let info = transfer_info(&transfer);
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
                volume_rpc,
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
        let namespace = match Namespace::try_new(namespace) {
            Ok(namespace) => namespace,
            Err(error) => return self.err("VOLUME_ZFS_PEER_SNAPSHOT_FAILED", error),
        };
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
        let namespace = match Namespace::try_new(namespace) {
            Ok(namespace) => namespace,
            Err(error) => return self.err("VOLUME_ZFS_PEER_SNAPSHOT_GUID_FAILED", error),
        };
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
        let namespace = match Namespace::try_new(namespace) {
            Ok(namespace) => namespace,
            Err(error) => return self.err("VOLUME_ZFS_PEER_START_SEND_FAILED", error),
        };
        let record = match self.volume_record(&namespace, volume).await {
            Ok(record) => record,
            Err(error) => return self.err("VOLUME_ZFS_PEER_START_SEND_FAILED", error),
        };
        if record.machine_id != self.identity.machine_id {
            return self.err(
                "VOLUME_ZFS_PEER_START_SEND_FAILED",
                format!(
                    "volume '{}/{}' is pinned to machine '{}', not local machine '{}'",
                    namespace.as_str(),
                    volume,
                    record.machine_id,
                    self.identity.machine_id
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
                serde_json::to_string_pretty(&transfer_info(&record))
                    .unwrap_or_else(|_| id.to_string()),
                Some(DaemonPayload::VolumeZfsTransfer(VolumeZfsTransferPayload {
                    transfer: transfer_info(&record),
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
                    transfers: records.iter().map(transfer_info).collect(),
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
            namespace: namespace.as_str().to_string(),
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
            namespace: namespace.as_str().to_string(),
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
                namespace.as_str(),
                volume,
                record.machine_id,
                self.identity.machine_id
            ));
        }
        self.snapshot_local_volume_zfs(namespace, volume, snapshot)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn clone_local_volume_zfs(
        &self,
        namespace: &Namespace,
        deploy_id: &str,
        volume: &str,
        source_namespace: &Namespace,
        source_volume: &str,
        snapshot: &str,
        quota: &str,
        mode: &str,
        owner: &str,
    ) -> Result<VolumeZfsClonePayload, String> {
        let source_record = self.volume_record(source_namespace, source_volume).await?;
        if source_record.scope != VolumeScope::Single {
            return Err(format!(
                "volume '{}/{}' has scope {:?}, expected single",
                source_namespace.as_str(),
                source_volume,
                source_record.scope
            ));
        }
        if source_record.machine_id != self.identity.machine_id {
            return Err(format!(
                "volume '{}/{}' is pinned to machine '{}', not local machine '{}'",
                source_namespace.as_str(),
                source_volume,
                source_record.machine_id,
                self.identity.machine_id
            ));
        }
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no mesh is running".to_string())?;
        if active
            .mesh
            .store
            .get_volume(namespace, volume)
            .await
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err(format!(
                "volume '{}/{}' already has a committed record",
                namespace.as_str(),
                volume
            ));
        }

        let driver = self.local_zfs_driver().await?;
        let source_dataset = volume_dataset(driver.root_dataset(), source_namespace, source_volume);
        let target_dataset = volume_dataset(driver.root_dataset(), namespace, volume);
        let target = DatasetSpec {
            dataset: target_dataset.clone(),
            mountpoint: driver
                .root_mountpoint()
                .join(&namespace.as_str())
                .join(volume),
            quota: quota.to_string(),
            mode: mode.to_string(),
            owner: owner.to_string(),
        };
        let snapshot_info = driver
            .create_snapshot(&source_dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        let clone_info = driver
            .clone_snapshot(
                &source_dataset,
                &snapshot_info.name,
                &target,
                &CloneMetadata {
                    deploy_id: deploy_id.to_string(),
                    namespace: namespace.as_str().to_string(),
                    volume: volume.to_string(),
                    source_namespace: source_namespace.as_str().to_string(),
                    source_volume: source_volume.to_string(),
                    snapshot: snapshot.to_string(),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(VolumeZfsClonePayload {
            namespace: namespace.as_str().to_string(),
            volume: volume.to_string(),
            source_namespace: source_namespace.as_str().to_string(),
            source_volume: source_volume.to_string(),
            machine_id: self.identity.machine_id.clone(),
            source_dataset,
            target_dataset,
            snapshot: clone_info.name,
            guid: clone_info.guid,
        })
    }

    async fn cleanup_uncommitted_local_volume_clone_zfs(
        &self,
        namespace: &Namespace,
        deploy_id: &str,
        volume: &str,
        source_namespace: &Namespace,
        source_volume: &str,
        snapshot: &str,
    ) -> Result<(), String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no mesh is running".to_string())?;
        if active
            .mesh
            .store
            .get_volume(namespace, volume)
            .await
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Ok(());
        }
        let driver = self.local_zfs_driver().await?;
        let dataset = volume_dataset(driver.root_dataset(), namespace, volume);
        let mut target_cleanup_error = None;
        if driver
            .dataset_exists(&dataset)
            .await
            .map_err(|error| error.to_string())?
        {
            let destroyed = driver
                .destroy_marked_volume_clone(
                    &dataset,
                    &CloneMetadata {
                        deploy_id: deploy_id.to_string(),
                        namespace: namespace.as_str().to_string(),
                        volume: volume.to_string(),
                        source_namespace: source_namespace.as_str().to_string(),
                        source_volume: source_volume.to_string(),
                        snapshot: snapshot.to_string(),
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            if !destroyed {
                target_cleanup_error = Some(format!(
                    "refusing to clean up uncommitted clone '{}/{}': dataset '{}' exists without matching clone metadata",
                    namespace.as_str(),
                    volume,
                    dataset
                ));
            }
        }
        let source_dataset = volume_dataset(driver.root_dataset(), source_namespace, source_volume);
        driver
            .destroy_snapshot(&source_dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        if let Some(error) = target_cleanup_error {
            return Err(error);
        }
        Ok(())
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
            namespace: namespace.as_str().to_string(),
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
            .ok_or_else(|| "daemon does not have the ZFS storage backend configured".to_string())
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
        match VolumeZfsNodeClient::new(NatsVolumeZfsRpcTransport::new(client))
            .inspect(&machine.id, namespace, volume, None)
            .await
        {
            Ok(response) => volume_zfs_node_response_to_daemon_response(response),
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
        let Some(machine) = self.find_machine(&machine_id.as_str()).await else {
            return self.err(
                "MACHINE_NOT_FOUND",
                format!("machine '{}' not found", machine_id),
            );
        };
        let client = match self.nats_node_rpc_client().await {
            Ok(client) => client,
            Err(error) => return self.err("VOLUME_ZFS_SNAPSHOT_FAILED", error),
        };
        match VolumeZfsNodeClient::new(NatsVolumeZfsRpcTransport::new(client))
            .snapshot(&machine.id, namespace.as_str(), volume, snapshot)
            .await
        {
            Ok(response) => volume_zfs_node_response_to_daemon_response(response),
            Err(error) => self.err("VOLUME_ZFS_SNAPSHOT_FAILED", error.to_string()),
        }
    }

    async fn find_machine(&self, machine: &str) -> Option<ployz_model::MachineMembership> {
        let active = self.active.as_ref()?;
        let machines = active.mesh.store.list_machines().await.ok()?;
        machines
            .into_iter()
            .find(|record| record.id.as_str() == machine)
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

    async fn find_volume_move_source_machine(
        &self,
        machine: &str,
    ) -> Result<MachineMembership, String> {
        let record = self
            .find_machine(machine)
            .await
            .ok_or_else(|| format!("machine '{machine}' not found"))?;
        if !matches!(
            record.lifecycle,
            MachineLifecycle::Active | MachineLifecycle::Draining
        ) {
            return Err(format!(
                "machine '{}' is {}, expected active or draining",
                record.id, record.lifecycle
            ));
        }
        Ok(record)
    }
}

fn volume_zfs_node_response_to_daemon_response(response: VolumeZfsNodeResponse) -> DaemonResponse {
    let success = response.is_ok();
    let code = response.code().to_string();
    let message = response.message().to_string();
    let payload = response
        .into_payload()
        .map(volume_zfs_node_payload_to_daemon_payload);
    if success {
        return DaemonResponse::success(message, payload);
    }
    DaemonResponse::error(code, message, payload)
}

fn volume_zfs_node_payload_to_daemon_payload(payload: VolumeZfsNodePayload) -> DaemonPayload {
    match payload {
        VolumeZfsNodePayload::Inspect(payload) => {
            DaemonPayload::VolumeZfsInspect(volume_zfs_inspect_payload(payload))
        }
        VolumeZfsNodePayload::Snapshot(payload) => {
            DaemonPayload::VolumeZfsSnapshot(volume_zfs_snapshot_payload(payload))
        }
    }
}

fn volume_zfs_inspect_payload(payload: NodeVolumeZfsInspectPayload) -> VolumeZfsInspectPayload {
    VolumeZfsInspectPayload {
        namespace: payload.namespace,
        volume: payload.volume,
        machine_id: payload.machine_id,
        dataset: payload.dataset,
        mountpoint: payload.mountpoint,
        quota: payload.quota,
        used_bytes: payload.used_bytes,
        snapshots: payload
            .snapshots
            .into_iter()
            .map(|snapshot| VolumeZfsSnapshotInfo {
                name: snapshot.name,
                guid: snapshot.guid,
            })
            .collect(),
    }
}

fn volume_zfs_snapshot_payload(payload: NodeVolumeZfsSnapshotPayload) -> VolumeZfsSnapshotPayload {
    VolumeZfsSnapshotPayload {
        namespace: payload.namespace,
        volume: payload.volume,
        machine_id: payload.machine_id,
        dataset: payload.dataset,
        snapshot: payload.snapshot,
        guid: payload.guid,
    }
}

fn volume_dataset(root: &str, namespace: &Namespace, volume: &str) -> String {
    format!("{root}/{}/{}", namespace.as_str(), volume)
}

#[cfg(test)]
mod tests {
    use super::{MoveClaimOutcome, TransferStatus, TransferStore, finalize_zfs_transfer};
    use crate::daemon::DaemonState;
    use ployz_api::VolumeZfsTransferState;
    use ployz_model::MachineId;
    use ployz_runtime_api::Identity;
    use ployz_spec::Namespace;
    use std::path::PathBuf;
    use std::time::Duration;

    fn tmp_root(label: &str) -> PathBuf {
        let id = super::unique_transfer_id(0).expect("unique id");
        std::env::temp_dir().join(format!("ployz-zfs-transfer-test-{label}-{id}"))
    }

    fn begin(store: &TransferStore) -> super::TransferRecord {
        store
            .begin(
                &Namespace::new("default"),
                "data",
                MachineId::new("source"),
                MachineId::new("target"),
                "snap".into(),
                None,
            )
            .expect("begin transfer")
    }

    fn create_claim(store: &TransferStore, transfer_id: &str) -> MoveClaimOutcome {
        store
            .create_move_claim(
                &Namespace::new("default"),
                "data",
                &MachineId::new("source"),
                &MachineId::new("target"),
                "snap",
                None,
                transfer_id,
            )
            .expect("create claim")
    }

    fn claim_path(store: &TransferStore) -> PathBuf {
        let key = super::move_claim_key(
            &Namespace::new("default"),
            "data",
            &MachineId::new("source"),
            &MachineId::new("target"),
            "snap",
            None,
        );
        store.claim_path(&key)
    }

    fn make_state(label: &str) -> DaemonState {
        let data_dir = tmp_root(label);
        let identity = Identity::generate(MachineId::new("founder"), [42; 32]);
        DaemonState::new_for_tests(
            &data_dir,
            identity,
            "10.210.0.0/16".into(),
            24,
            4319,
            "127.0.0.1:0".into(),
            None,
            1,
        )
    }

    #[tokio::test]
    async fn peer_snapshot_rejects_invalid_namespace() {
        let state = make_state("peer-snapshot-invalid-namespace");

        let response = state
            .handle_volume_zfs_peer_snapshot("Prod", "data", "snap")
            .await;

        assert!(!response.is_ok());
        assert_eq!(response.code(), "VOLUME_ZFS_PEER_SNAPSHOT_FAILED");
        assert!(
            response.message().contains("namespace"),
            "got: {}",
            response.message()
        );
    }

    #[tokio::test]
    async fn peer_snapshot_guid_rejects_invalid_namespace() {
        let state = make_state("peer-snapshot-guid-invalid-namespace");

        let response = state
            .handle_volume_zfs_peer_snapshot_guid("bad ns", "data", "snap")
            .await;

        assert!(!response.is_ok());
        assert_eq!(response.code(), "VOLUME_ZFS_PEER_SNAPSHOT_GUID_FAILED");
        assert!(
            response.message().contains("namespace"),
            "got: {}",
            response.message()
        );
    }

    #[tokio::test]
    async fn peer_start_send_rejects_invalid_namespace() {
        let state = make_state("peer-start-send-invalid-namespace");

        let response = state
            .handle_volume_zfs_peer_start_send("bad ns", "data", "snap", "target", 1, None, None)
            .await;

        assert!(!response.is_ok());
        assert_eq!(response.code(), "VOLUME_ZFS_PEER_START_SEND_FAILED");
        assert!(
            response.message().contains("namespace"),
            "got: {}",
            response.message()
        );
    }

    #[test]
    fn startup_recovery_marks_running_transfers_interrupted() {
        let root = tmp_root("startup-recovery");
        let store = TransferStore::new(root.clone());
        let transfer = begin(&store);
        assert!(matches!(
            create_claim(&store, &transfer.id),
            MoveClaimOutcome::Created
        ));
        assert_eq!(transfer.status(), TransferStatus::Running);

        let count = store.recover_startup().expect("recover startup");
        assert_eq!(count, 1);
        let loaded = store
            .load(&transfer.id)
            .expect("load")
            .expect("record exists");
        assert_eq!(loaded.status(), TransferStatus::Interrupted);
        assert!(
            !claim_path(&store).exists(),
            "startup recovery should delete the interrupted transfer claim"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn move_claim_created_then_existing_returns_same_transfer_id() {
        let root = tmp_root("claim-idempotent");
        let store = TransferStore::new(root.clone());

        assert!(matches!(
            create_claim(&store, "transfer-a"),
            MoveClaimOutcome::Created
        ));
        match create_claim(&store, "transfer-b") {
            MoveClaimOutcome::Exists(existing_id) => assert_eq!(existing_id, "transfer-a"),
            MoveClaimOutcome::Created => panic!("claim should already exist"),
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn deleting_move_claim_allows_reclaim_after_stale_claim() {
        let root = tmp_root("claim-delete");
        let store = TransferStore::new(root.clone());

        assert!(matches!(
            create_claim(&store, "missing-transfer"),
            MoveClaimOutcome::Created
        ));
        store.delete_move_claim(
            &Namespace::new("default"),
            "data",
            &MachineId::new("source"),
            &MachineId::new("target"),
            "snap",
            None,
        );
        assert!(matches!(
            create_claim(&store, "transfer-b"),
            MoveClaimOutcome::Created
        ));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn deleting_move_claim_allows_reclaim_after_invalid_claim() {
        let root = tmp_root("claim-invalid");
        let store = TransferStore::new(root.clone());
        std::fs::create_dir_all(store.claim_dir()).expect("claim dir");
        std::fs::write(claim_path(&store), "\n").expect("invalid claim");

        match create_claim(&store, "transfer-a") {
            MoveClaimOutcome::Exists(existing_id) => assert!(existing_id.is_empty()),
            MoveClaimOutcome::Created => panic!("claim should already exist"),
        }
        store.delete_move_claim(
            &Namespace::new("default"),
            "data",
            &MachineId::new("source"),
            &MachineId::new("target"),
            "snap",
            None,
        );
        assert!(matches!(
            create_claim(&store, "transfer-b"),
            MoveClaimOutcome::Created
        ));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn claimed_transfer_waits_for_record_creation() {
        let root = tmp_root("claim-waits-for-record");
        let store = TransferStore::new(root.clone());

        assert!(matches!(
            create_claim(&store, "transfer-a"),
            MoveClaimOutcome::Created
        ));

        let delayed_store = store.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            delayed_store
                .begin_with_id(
                    "transfer-a".into(),
                    &Namespace::new("default"),
                    "data",
                    MachineId::new("source"),
                    MachineId::new("target"),
                    "snap".into(),
                    None,
                    super::now_unix_secs(),
                )
                .expect("begin delayed transfer");
        });

        match super::wait_for_claimed_transfer_record(&store, "transfer-a")
            .await
            .expect("wait for claimed transfer")
        {
            super::ClaimedTransfer::Running(record) => assert_eq!(record.id, "transfer-a"),
            super::ClaimedTransfer::Terminal(record) => {
                panic!("expected running transfer, got {:?}", record.status())
            }
            super::ClaimedTransfer::MissingAfterWait => {
                panic!("claim should wait for newly created transfer record")
            }
        }

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
    fn reusable_transfer_returns_running_matching_move() {
        let root = tmp_root("reusable");
        let store = TransferStore::new(root.clone());
        let transfer = begin(&store);

        let found = store
            .find_reusable(
                &Namespace::new("default"),
                "data",
                &MachineId::new("source"),
                &MachineId::new("target"),
                "snap",
                None,
            )
            .expect("find reusable")
            .expect("running transfer reusable");

        assert_eq!(found.id, transfer.id);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reusable_transfer_ignores_terminal_matching_move() {
        let root = tmp_root("terminal-not-reusable");
        let store = TransferStore::new(root.clone());
        let mut transfer = begin(&store);
        for status in [TransferStatus::Succeeded, TransferStatus::Failed] {
            store
                .update_status(&mut transfer, status, Some("boom".into()))
                .expect("terminal transfer");

            let found = store
                .find_reusable(
                    &Namespace::new("default"),
                    "data",
                    &MachineId::new("source"),
                    &MachineId::new("target"),
                    "snap",
                    None,
                )
                .expect("find reusable");

            assert!(found.is_none());
        }
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
        assert_eq!(loaded.status(), TransferStatus::Succeeded);
        assert_eq!(loaded.stage(), "complete");
        assert!(loaded.last_error().is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finalize_captures_last_error_on_failure() {
        let root = tmp_root("finalize-err");
        let store = TransferStore::new(root.clone());
        let mut transfer = begin(&store);
        assert!(matches!(
            create_claim(&store, &transfer.id),
            MoveClaimOutcome::Created
        ));

        finalize_zfs_transfer(&store, &mut transfer, Err("boom".into()));

        let loaded = store
            .load(&transfer.id)
            .expect("load")
            .expect("record exists");
        assert_eq!(loaded.status(), TransferStatus::Failed);
        assert_eq!(loaded.last_error(), Some("boom"));
        assert!(
            !claim_path(&store).exists(),
            "failed transfer should delete its move claim"
        );
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
        assert_eq!(loaded.status(), TransferStatus::Interrupted);
        assert_eq!(loaded.last_error(), Some("send failed"));
        assert!(
            matches!(
                super::transfer_info(&loaded).state,
                VolumeZfsTransferState::Interrupted {
                    last_error: Some(ref error),
                    ..
                } if error == "send failed"
            ),
            "operator-facing payload should preserve the failure audience"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
