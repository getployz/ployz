mod error;
mod facts;
mod p2panda;
mod remove;
mod wire;

pub use error::{MachineRemoveError, MachineRemoveResult};
pub use facts::{
    MachineRemoveCleanupDoneFact, MachineRemoveDecisionCandidate, MachineRemoveDecisionFact,
    MachineRemoveFactPayload, MachineRemoveId, decode_machine_remove_cleanup_done_fact,
    decode_machine_remove_decision_fact, machine_remove_cleanup_done_fact_key,
    machine_remove_cleanup_done_fact_payload, machine_remove_decision_fact_key,
    machine_remove_decision_fact_payload, read_machine_remove_cleanup_done,
    read_machine_remove_decision, validate_machine_remove_cleanup_done,
};
pub use p2panda::{PandaMachineFactStore, PandaMachineFactWriter};
pub use remove::{
    BusMachineFactWriter, CleanupFailureKind, MachineFactWriter, MachineRemoveCommandInput,
    MachineRemoveCommandResult, MachineRemoveCoordinator, MachineRemoveOutcome,
    MachineRemoveRequest, MachineRemoveTimeouts, MachineRemoveValidationError,
    RemoveCleanupPendingReason, RemoveCleanupStatus, WrittenMachineFact, prepare_remove_subject,
    stop_removed_workloads_subject,
};
pub use wire::{
    PrepareRemoveIntent, PrepareRemoveOutcome, PrepareRemoveReply, PrepareRemoveRequest,
    StopRemovedWorkloadsOutcome, StopRemovedWorkloadsReply, StopRemovedWorkloadsRequest,
};
