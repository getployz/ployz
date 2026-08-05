//! Machine-local arrival and storage-selection policy for join.

use serde::{Deserialize, Serialize};

use crate::corrosion::{
    MachineStorageIneligibleReason, MachineStorageSelection, MachineStorageSelectionReason,
    StorageMode,
};
use crate::founding::MINIMUM_ZFS_MEMORY_BYTES;

use super::token::JoinDoorCertFingerprint;

/// The repair primitive for a host carrying state from another cluster.
pub const JOIN_RESET_COMMAND: &str = "ployz machine reset";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JoinStorageChoice {
    Automatic,
    Flag { mode: StorageMode },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct JoinStorageFacts {
    pub imported_zfs_pool: bool,
    pub total_memory_bytes: u64,
}

/// Selects the durable machine storage outcome at admission.
#[must_use]
pub fn select_join_storage(
    cluster_default: StorageMode,
    choice: JoinStorageChoice,
    facts: JoinStorageFacts,
) -> MachineStorageSelection {
    match choice {
        JoinStorageChoice::Flag { mode } => MachineStorageSelection {
            mode,
            reason: MachineStorageSelectionReason::Flag,
        },
        JoinStorageChoice::Automatic
            if cluster_default == StorageMode::Zfs
                && facts.imported_zfs_pool
                && facts.total_memory_bytes >= MINIMUM_ZFS_MEMORY_BYTES =>
        {
            MachineStorageSelection {
                mode: StorageMode::Zfs,
                reason: MachineStorageSelectionReason::Default,
            }
        }
        JoinStorageChoice::Automatic
            if cluster_default == StorageMode::Zfs && facts.imported_zfs_pool =>
        {
            MachineStorageSelection {
                mode: StorageMode::Plain,
                reason: MachineStorageSelectionReason::Ineligible {
                    reason: MachineStorageIneligibleReason::LowRam,
                },
            }
        }
        JoinStorageChoice::Automatic => MachineStorageSelection {
            mode: StorageMode::Plain,
            reason: MachineStorageSelectionReason::Default,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinArrival {
    Clean,
    Partial {
        persisted_door_fingerprint: JoinDoorCertFingerprint,
    },
    Complete {
        persisted_door_fingerprint: JoinDoorCertFingerprint,
    },
    /// Founding state has no join fingerprint and belongs to a different journey.
    Founding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinArrivalDisposition {
    Join,
    Resume,
    NoOp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinRepairCommand {
    #[serde(rename = "ployz machine reset")]
    ResetMachine,
}

impl JoinRepairCommand {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResetMachine => JOIN_RESET_COMMAND,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JoinArrivalRefusal {
    #[error("host carries a different cluster door fingerprint; run {repair_command:?}")]
    ForeignState {
        requested_door_fingerprint: JoinDoorCertFingerprint,
        found_door_fingerprint: JoinDoorCertFingerprint,
        repair_command: JoinRepairCommand,
    },
    #[error("host was already used to found a cluster; run {repair_command:?}")]
    FoundingState { repair_command: JoinRepairCommand },
}

impl JoinArrivalRefusal {
    #[must_use]
    pub const fn repair_command(&self) -> JoinRepairCommand {
        match self {
            Self::ForeignState { repair_command, .. } | Self::FoundingState { repair_command } => {
                *repair_command
            }
        }
    }
}

pub fn classify_join_arrival(
    requested_door_fingerprint: &JoinDoorCertFingerprint,
    arrival: JoinArrival,
) -> Result<JoinArrivalDisposition, JoinArrivalRefusal> {
    match arrival {
        JoinArrival::Clean => Ok(JoinArrivalDisposition::Join),
        JoinArrival::Partial {
            persisted_door_fingerprint,
        } if persisted_door_fingerprint == *requested_door_fingerprint => {
            Ok(JoinArrivalDisposition::Resume)
        }
        JoinArrival::Complete {
            persisted_door_fingerprint,
        } if persisted_door_fingerprint == *requested_door_fingerprint => {
            Ok(JoinArrivalDisposition::NoOp)
        }
        JoinArrival::Partial {
            persisted_door_fingerprint,
        }
        | JoinArrival::Complete {
            persisted_door_fingerprint,
        } => Err(JoinArrivalRefusal::ForeignState {
            requested_door_fingerprint: requested_door_fingerprint.clone(),
            found_door_fingerprint: persisted_door_fingerprint,
            repair_command: JoinRepairCommand::ResetMachine,
        }),
        JoinArrival::Founding => Err(JoinArrivalRefusal::FoundingState {
            repair_command: JoinRepairCommand::ResetMachine,
        }),
    }
}
