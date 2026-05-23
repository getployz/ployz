mod coordinator;
mod domain;
mod error;
mod facts;
mod p2panda;
mod state_machine;
mod wire;

pub use coordinator::{
    DeployCoordinator, DeployRecovery, DeployTimeouts, PendingCleanup, PreCommitIncompleteRecovery,
    ProjectedPendingCleanup, RecoveredPendingCleanup,
};
pub use domain::{
    CandidateCleanupFailure, CandidateCleanupState, CandidateCleanupStatus, CandidateCleanupTarget,
    CapacityRejectionReason, CapacityReply, CleanupFailureKind, CleanupPendingReason,
    CleanupStatus, DeployCommandResult, DeployId, DeployManifest, DeployOutcome, DrainStatus,
    InstanceCapacityRequirement, InstanceId, InstancePlan, NodeCandidateCleanup, PhaseId,
    PhasePlan, PhasePolicy, PhaseReversibility, PreCommitCleanupReport, RevisionId,
    ServingPublication,
};
pub use error::{DeployError, DeployResult};
pub use facts::{
    BusDeployFactWriter, DeployCleanupDoneFact, DeployDecisionCandidate, DeployDecisionFact,
    DeployDecisionSelection, DeployEpochReservationFact, DeployFactPayload, DeployFactWriteStatus,
    DeployFactWriter, DeployStatusFact, DeployStatusPhase, WrittenDeployFact,
    decode_deploy_cleanup_done_fact, decode_deploy_decision_fact,
    decode_deploy_epoch_reservation_fact, decode_deploy_status_fact, deploy_cleanup_done_fact_key,
    deploy_cleanup_done_fact_payload, deploy_decision_fact_key, deploy_decision_fact_payload,
    deploy_epoch_reservation_fact_key, deploy_epoch_reservation_fact_payload,
    deploy_status_fact_key, deploy_status_fact_payload, read_deploy_cleanup_done,
    read_deploy_decision, read_deploy_statuses, select_deploy_decision,
};
pub use mvp_routing::{
    DnsCommitId, GatewayCommitId, ProjectionCatchUp, RouteCommitId, RoutingError,
    ServingCommitFacts, ServingCommitId, ServingCommitPlan,
};
pub use p2panda::PandaDeployFactWriter;
pub use state_machine::{DeployStateMachine, PhaseState};
pub use wire::{
    CapacityRequest, CleanupDeployCandidatesRequest, DrainInstanceRequest, InstanceCommandReply,
    InstanceCommandRequest, InstanceNotReadyReason, InstanceStartOutcome, StopInstanceRequest,
    decode, decode_capacity_reply, encode,
};

#[cfg(test)]
mod tests;
