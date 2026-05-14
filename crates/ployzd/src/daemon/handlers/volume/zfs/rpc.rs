use ployz_api::{
    DaemonPayload, DaemonResponse, VolumeZfsInspectPayload, VolumeZfsPeerSendPayload,
    VolumeZfsSnapshotInfo, VolumeZfsSnapshotPayload,
};
use ployz_model::MachineId;
use ployz_node_api::{NodeVolumeZfsInspectPayload, NodeVolumeZfsSnapshotPayload};
use ployz_node_runtime::{VolumeZfsNodeClient, VolumeZfsNodePayload, VolumeZfsNodeResponse};
use ployz_spec::Namespace;

use super::transfer_execution::send_zfs_stream_from_local;
use crate::daemon::DaemonState;
use crate::daemon::node_rpc::NatsVolumeZfsRpcTransport;

impl DaemonState {
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

    pub(super) async fn forward_volume_zfs_inspect(
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

    pub(super) async fn forward_volume_zfs_snapshot(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        volume: &str,
        snapshot: &str,
    ) -> DaemonResponse {
        let Some(machine) = self.find_machine(machine_id.as_str()).await else {
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
