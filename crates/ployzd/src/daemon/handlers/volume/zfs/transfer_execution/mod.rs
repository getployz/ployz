mod finalization;
mod machine;

#[cfg(test)]
mod tests;

pub(super) use finalization::finalize_zfs_transfer;

use ployz_model::{MachineId, MachineMembership, VolumeRecord};
use ployz_node_runtime::{VolumeZfsNodeClient, VolumeZfsRpcTransport};
use ployz_volume_zfs::{TokioShellRunner, TransferRecord, TransferStore, ZfsDriver};

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_coordinated_zfs_transfer_inner<R>(
    store: &TransferStore,
    transfer: &mut TransferRecord,
    record: &VolumeRecord,
    source: &MachineMembership,
    target: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    volume_rpc: Option<VolumeZfsNodeClient<R>>,
    transfer_port: u16,
    local_machine_id: &MachineId,
    snapshot: &str,
    from_snapshot: Option<&str>,
) -> Result<(), String>
where
    R: VolumeZfsRpcTransport,
{
    store.update_stage(transfer, "snapshot")?;
    let snap_info = machine::snapshot_on_machine(
        source,
        local_driver,
        volume_rpc.as_ref(),
        local_machine_id,
        &record.namespace,
        &record.volume_name,
        snapshot,
    )
    .await?;
    transfer.state.with_snapshot_guid(snap_info.guid);
    store.save(transfer)?;

    if let Some(from_snapshot) = from_snapshot {
        store.update_stage(transfer, "verify-base")?;
        let from_guid = machine::snapshot_guid_on_machine(
            source,
            local_driver,
            volume_rpc.as_ref(),
            local_machine_id,
            &record.namespace,
            &record.volume_name,
            from_snapshot,
        )
        .await?;
        let target_from_guid = machine::snapshot_guid_on_machine(
            target,
            local_driver,
            volume_rpc.as_ref(),
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
        transfer.state.with_from_snapshot_guid(from_guid.guid);
        store.save(transfer)?;
    }

    store.update_stage(transfer, "send")?;
    let result = machine::start_send_on_machine(
        source,
        target,
        local_driver,
        volume_rpc.as_ref(),
        local_machine_id,
        transfer_port,
        record,
        snapshot,
        snap_info.guid,
        from_snapshot,
        transfer.from_snapshot_guid(),
    )
    .await?;
    transfer
        .state
        .with_bytes_transferred(result.bytes_transferred);
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
