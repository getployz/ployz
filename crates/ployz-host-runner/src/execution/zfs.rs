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

#[cfg(test)]
use command::{COMMAND_TIMEOUT, INSTALL_TIMEOUT};
#[cfg(test)]
use ployz_core::storage::{PLOYZ_OWNED_ZFS_BACKING_FILE, PLOYZ_OWNED_ZFS_POOL, ZfsDatasetRoot};
#[cfg(test)]
use state::VOLUME_MOUNTPOINT;

#[cfg(test)]
mod tests;
