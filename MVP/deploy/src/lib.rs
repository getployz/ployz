mod coordinator;
mod domain;
mod error;
mod serving_commit;
mod state_machine;
mod wire;

pub use coordinator::{DeployCoordinator, DeployTimeouts, PendingCleanup, ProjectedPendingCleanup};
pub use domain::{
    CapacityRejectionReason, CapacityReply, CleanupFailureKind, CleanupPendingReason,
    CleanupStatus, DeployCommandResult, DeployId, DeployManifest, DeployOutcome, DnsCommitId,
    DrainStatus, GatewayCommitId, InstanceCapacityRequirement, InstanceId, InstancePlan, PhaseId,
    PhasePlan, PhasePolicy, PhaseReversibility, ProjectionCatchUp, RevisionId, RouteCommitId,
    ServingCommitId, ServingCommitPlan, ServingPublication,
};
pub use error::{DeployError, DeployResult};
pub use serving_commit::{ServingCommitFacts, write_serving_commit};
pub use state_machine::{DeployStateMachine, PhaseState};
pub use wire::{
    CapacityRequest, DrainInstanceRequest, InstanceCommandReply, InstanceCommandRequest,
    InstanceNotReadyReason, InstanceStartOutcome, StopInstanceRequest,
};

#[cfg(test)]
mod tests;
