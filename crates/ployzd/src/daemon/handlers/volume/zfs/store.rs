use ployz_spec::Namespace;
use ployz_volume_zfs::TransferStore;

use crate::daemon::DaemonState;

impl DaemonState {
    pub(in crate::daemon::handlers::volume::zfs) fn zfs_transfer_store(&self) -> TransferStore {
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
}

pub(in crate::daemon::handlers::volume::zfs) fn volume_dataset(
    root: &str,
    namespace: &Namespace,
    volume: &str,
) -> String {
    format!("{root}/{}/{}", namespace.as_str(), volume)
}
