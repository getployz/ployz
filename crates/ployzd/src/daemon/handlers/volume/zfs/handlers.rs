use ployz_api::{
    DaemonPayload, DaemonResponse, VolumeZfsTransferListPayload, VolumeZfsTransferPayload,
};
use ployz_spec::Namespace;

use crate::daemon::DaemonState;

use super::responses::transfer_info;

impl DaemonState {
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
                .forward_volume_zfs_inspect(namespace.as_str(), volume, &machine)
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
}
