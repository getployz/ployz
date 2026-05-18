mod error;
mod remove;
mod wire;

pub use error::{MachineRemoveError, MachineRemoveResult};
pub use remove::{
    BusMachineFactWriter, CleanupFailureKind, MachineFactWriter, MachineRemoveCommandResult,
    MachineRemoveCoordinator, MachineRemoveOutcome, MachineRemoveRequest, MachineRemoveTimeouts,
    MachineRemoveValidationError, PendingMachineRemove, RemoveCleanupPendingReason,
    RemoveCleanupStatus, WrittenMachineFact,
};
pub use wire::{
    PrepareRemoveIntent, PrepareRemoveOutcome, PrepareRemoveReply, PrepareRemoveRequest,
    StopRemovedWorkloadsOutcome, StopRemovedWorkloadsReply, StopRemovedWorkloadsRequest,
};
