//! ployzd's own metrics, Volume IO and the disk breakdown, sent to the Collector.

mod disk;
pub(crate) mod emit;
mod zfs_io;
mod zfs_usage;

use std::path::Path;

use ployz_core::MachineId;

use crate::machine::store;

/// Read every time: a reset gives the Machine a new id.
pub(in crate::observe) fn machine_id(data_dir: &Path) -> Option<MachineId> {
    store::identity(data_dir).map(|identity| identity.id)
}
