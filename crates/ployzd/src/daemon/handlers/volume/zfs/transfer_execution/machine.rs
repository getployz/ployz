use super::local_send::send_zfs_stream_from_local;
use ployz_model::{MachineId, MachineMembership, VolumeRecord};
use ployz_node_api::NodeVolumeZfsSnapshotPayload;
use ployz_node_runtime::{VolumeZfsNodeClient, VolumeZfsRpcTransport};
use ployz_spec::Namespace;
use ployz_volume_zfs::{SendResult, TokioShellRunner, ZfsDriver};

pub(super) async fn snapshot_on_machine<R>(
    machine: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    volume_rpc: Option<&VolumeZfsNodeClient<R>>,
    local_machine_id: &MachineId,
    namespace: &Namespace,
    volume: &str,
    snapshot: &str,
) -> Result<NodeVolumeZfsSnapshotPayload, String>
where
    R: VolumeZfsRpcTransport,
{
    if machine.id == *local_machine_id {
        let driver =
            local_driver.ok_or_else(|| "local zfs driver is not configured".to_string())?;
        let dataset = super::super::volume_dataset(driver.root_dataset(), namespace, volume);
        let snap_info = driver
            .create_snapshot(&dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(NodeVolumeZfsSnapshotPayload {
            namespace: namespace.as_str().to_string(),
            volume: volume.to_string(),
            machine_id: machine.id.clone(),
            dataset,
            snapshot: snap_info.name,
            guid: snap_info.guid,
        });
    }

    let volume_rpc = volume_rpc.ok_or_else(|| "volume RPC client is not configured".to_string())?;
    volume_rpc
        .peer_snapshot(&machine.id, namespace.as_str(), volume, snapshot)
        .await
        .map_err(|error| error.to_string())
}

pub(super) async fn snapshot_guid_on_machine<R>(
    machine: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    volume_rpc: Option<&VolumeZfsNodeClient<R>>,
    local_machine_id: &MachineId,
    namespace: &Namespace,
    volume: &str,
    snapshot: &str,
) -> Result<NodeVolumeZfsSnapshotPayload, String>
where
    R: VolumeZfsRpcTransport,
{
    if machine.id == *local_machine_id {
        let driver =
            local_driver.ok_or_else(|| "local zfs driver is not configured".to_string())?;
        let dataset = super::super::volume_dataset(driver.root_dataset(), namespace, volume);
        let guid = driver
            .snapshot_guid(&dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(NodeVolumeZfsSnapshotPayload {
            namespace: namespace.as_str().to_string(),
            volume: volume.to_string(),
            machine_id: machine.id.clone(),
            dataset,
            snapshot: snapshot.to_string(),
            guid,
        });
    }

    let volume_rpc = volume_rpc.ok_or_else(|| "volume RPC client is not configured".to_string())?;
    volume_rpc
        .peer_snapshot_guid(&machine.id, namespace.as_str(), volume, snapshot)
        .await
        .map_err(|error| error.to_string())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn start_send_on_machine<R>(
    source: &MachineMembership,
    target: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    volume_rpc: Option<&VolumeZfsNodeClient<R>>,
    local_machine_id: &MachineId,
    transfer_port: u16,
    record: &VolumeRecord,
    snapshot: &str,
    expected_guid: u64,
    from_snapshot: Option<&str>,
    from_snapshot_guid: Option<u64>,
) -> Result<SendResult, String>
where
    R: VolumeZfsRpcTransport,
{
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

    let volume_rpc = volume_rpc.ok_or_else(|| "volume RPC client is not configured".to_string())?;
    let payload = volume_rpc
        .peer_start_send(
            &source.id,
            record.namespace.as_str(),
            &record.volume_name,
            snapshot,
            &target.id,
            expected_guid,
            from_snapshot,
            from_snapshot_guid,
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(SendResult {
        bytes_transferred: payload.bytes_transferred,
        snapshot_guid: payload.snapshot_guid,
    })
}
