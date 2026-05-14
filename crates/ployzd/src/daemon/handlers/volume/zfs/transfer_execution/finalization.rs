use ployz_volume_zfs::{TransferRecord, TransferStatus, TransferStore};

pub(in crate::daemon::handlers::volume::zfs) fn finalize_zfs_transfer(
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
            store.delete_claim_for(transfer);
        }
    }
}
