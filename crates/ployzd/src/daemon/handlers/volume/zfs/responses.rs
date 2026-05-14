use ployz_api::{VolumeZfsTransferInfo, VolumeZfsTransferState};
use ployz_volume_zfs::TransferRecord;

pub(super) fn transfer_info(record: &TransferRecord) -> VolumeZfsTransferInfo {
    VolumeZfsTransferInfo {
        id: record.id.clone(),
        namespace: record.namespace.clone(),
        volume: record.volume.clone(),
        source_machine: record.source_machine.clone(),
        target_machine: record.target_machine.clone(),
        snapshot_name: record.snapshot_name.clone(),
        from_snapshot_name: record.from_snapshot_name.clone(),
        started_at: record.started_at,
        updated_at: record.updated_at,
        state: transfer_state(&record.state),
    }
}

fn transfer_state(state: &ployz_volume_zfs::TransferState) -> VolumeZfsTransferState {
    match state {
        ployz_volume_zfs::TransferState::Running {
            stage,
            snapshot_guid,
            from_snapshot_guid,
            bytes_transferred,
            last_error,
        } => VolumeZfsTransferState::Running {
            stage: stage.clone(),
            snapshot_guid: *snapshot_guid,
            from_snapshot_guid: *from_snapshot_guid,
            bytes_transferred: *bytes_transferred,
            last_error: last_error.clone(),
        },
        ployz_volume_zfs::TransferState::Succeeded {
            stage,
            snapshot_guid,
            from_snapshot_guid,
            bytes_transferred,
        } => VolumeZfsTransferState::Succeeded {
            stage: stage.clone(),
            snapshot_guid: *snapshot_guid,
            from_snapshot_guid: *from_snapshot_guid,
            bytes_transferred: *bytes_transferred,
        },
        ployz_volume_zfs::TransferState::Failed {
            stage,
            last_error,
            snapshot_guid,
            from_snapshot_guid,
            bytes_transferred,
        } => VolumeZfsTransferState::Failed {
            stage: stage.clone(),
            last_error: last_error.clone(),
            snapshot_guid: *snapshot_guid,
            from_snapshot_guid: *from_snapshot_guid,
            bytes_transferred: *bytes_transferred,
        },
        ployz_volume_zfs::TransferState::Interrupted {
            stage,
            last_error,
            snapshot_guid,
            from_snapshot_guid,
            bytes_transferred,
        } => VolumeZfsTransferState::Interrupted {
            stage: stage.clone(),
            last_error: last_error.clone(),
            snapshot_guid: *snapshot_guid,
            from_snapshot_guid: *from_snapshot_guid,
            bytes_transferred: *bytes_transferred,
        },
    }
}
