mod coordinator;
mod domain;
mod error;
mod facts;
mod serving_commit;
mod state_machine;
mod wire;

pub use coordinator::{DeployCoordinator, DeployTimeouts, PendingCleanup, ProjectedPendingCleanup};
pub use domain::{
    CapacityRejectionReason, CapacityReply, CleanupFailureKind, CleanupPendingReason,
    CleanupStatus, DeployCommandResult, DeployId, DeployManifest, DeployOutcome, DrainStatus,
    InstanceCapacityRequirement, InstanceId, InstancePlan, PhaseId, PhasePlan, PhasePolicy,
    PhaseReversibility, RevisionId, ServingPublication,
};
pub use error::{DeployError, DeployResult};
pub use facts::{
    BusDeployFactWriter, DeployCleanupDoneFact, DeployDecisionCandidate, DeployDecisionFact,
    DeployDecisionSelection, DeployFactPayload, DeployFactWriteStatus, DeployFactWriter,
    WrittenDeployFact, decode_deploy_cleanup_done_fact, decode_deploy_decision_fact,
    deploy_cleanup_done_fact_key, deploy_cleanup_done_fact_payload, deploy_decision_fact_key,
    deploy_decision_fact_payload, read_deploy_decision, select_deploy_decision,
};
pub use mvp_routing::{
    DnsCommitId, GatewayCommitId, ProjectionCatchUp, RouteCommitId, RoutingError,
    ServingCommitFacts, ServingCommitId, ServingCommitPlan,
};
pub use serving_commit::{
    BusServingFactWriter, ServingFactWriteStatus, ServingFactWriter, WrittenServingFact,
    write_serving_commit,
};
pub use state_machine::{DeployStateMachine, PhaseState};
pub use wire::{
    CapacityRequest, DrainInstanceRequest, InstanceCommandReply, InstanceCommandRequest,
    InstanceNotReadyReason, InstanceStartOutcome, StopInstanceRequest,
};

#[cfg(test)]
mod tests;
