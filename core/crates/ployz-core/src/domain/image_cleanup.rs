//! Image Cleanup: remove superseded build images from the Machines a Deploy delivered to.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{ImageRemoval, MachineId};

/// One repository whose build images one Machine received. Plain data, so a caller
/// may persist it and clean up after its Deploy ends.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
pub struct PruneTarget {
    pub machine_id: MachineId,
    /// Docker's short repository name, as `docker image ls` prints it.
    pub repository: String,
}

/// Per-Machine Image Cleanup results. Cleanup never changes a Deploy Outcome.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ImageCleanupReport {
    pub machines: Vec<MachineImageCleanup>,
}

/// One Machine's Image Cleanup result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineImageCleanup {
    pub machine_id: MachineId,
    pub result: MachineCleanupResult,
}

/// Whether a Machine cleaned up, could not, or did not answer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MachineCleanupResult {
    /// Each attempted removal; empty when nothing was superseded.
    Cleaned { removals: Vec<ImageRemoval> },
    /// The Machine's daemon cannot remove images.
    Unsupported,
    /// The Machine did not answer; some removals may have happened.
    Unknown { message: String },
}
