mod command;
mod domain;
mod error;
mod facts;

pub use command::{
    BranchEnvironmentCommand, BranchEnvironmentRequest, BranchEnvironmentResult,
    BusEnvironmentFactWriter, EnvironmentCommandResult, EnvironmentFactWriter,
    EnvironmentServingPendingReason, EnvironmentVolumeForkEvidence,
    EnvironmentVolumeForkParticipant, EnvironmentVolumeForkReply, EnvironmentVolumeForkRequest,
    PromoteEnvironmentCommand, PromoteEnvironmentRequest, RollbackEnvironmentCommand,
    RollbackEnvironmentRequest, WrittenEnvironmentFact,
};
pub use domain::{
    EnvironmentBranchId, EnvironmentCommandId, EnvironmentEpoch, EnvironmentHeadId, EnvironmentId,
    EnvironmentRouteRef, EnvironmentVolumeRef,
};
pub use error::{EnvironmentError, EnvironmentResult};
pub use facts::{
    EnvironmentBranchFact, EnvironmentFactPayload, EnvironmentHeadCandidate, EnvironmentHeadFact,
    EnvironmentHeadReadModel, EnvironmentHeadReference, EnvironmentPromoteDecisionFact,
    EnvironmentRollbackDecisionFact, EnvironmentSupersededHead, current_environment_head,
    decode_environment_branch_fact, decode_environment_head_fact, environment_branch_fact_key,
    environment_branch_fact_payload, environment_head_fact_key, environment_head_fact_payload,
    environment_promote_decision_fact_key, environment_promote_decision_fact_payload,
    environment_rollback_decision_fact_key, environment_rollback_decision_fact_payload,
    read_environment_heads, require_expected_environment_epoch,
};

#[cfg(test)]
mod tests;
