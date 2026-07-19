//! Bounded machine-local ZFS preparation and Provisioned Volume effects.

mod command;
mod dataset;
mod preparation;
mod state;

pub use dataset::*;
#[cfg(test)]
use ployz_core::storage::{
    PreparedStorageOrigin, PreparedStorageState, StorageEffectFailure as ZfsEffectError,
};
pub use preparation::*;
#[cfg(test)]
use state::load_prepared_storage_state;
pub use state::{observe_storage_capability, recover_owned_storage};

#[cfg(test)]
use command::{COMMAND_TIMEOUT, INSTALL_TIMEOUT};
#[cfg(test)]
use ployz_core::storage::{
    DATASET_DESTROY_INNER_COMMAND_TIMEOUT, PLOYZ_OWNED_ZFS_BACKING_FILE, PLOYZ_OWNED_ZFS_POOL,
    PROVISIONED_VOLUME_MOUNTPOINT as VOLUME_MOUNTPOINT, ZfsDatasetRoot,
};

#[cfg(test)]
mod tests;
