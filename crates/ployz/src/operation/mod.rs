//! Product-neutral operation context shared by Ployz domains.

mod authority;
mod claims;
mod context;
mod event;
mod identity;
mod lease;
mod ledger;
mod status;

#[cfg(test)]
mod tests;

pub use authority::{AuthorityContext, AuthorityDecision, AuthorityEpoch, AuthorityPort};
pub use claims::{ClaimGuard, ClaimHash, FenceEpoch, SubmittedFenceToken};
pub(crate) use context::MutationWriteIdentity;
pub use context::{MutationAuthorizer, MutationContext, MutationContextRequest};
pub use event::{OperationEvent, OperationEventCursor, OperationEventKind, OperationEventSequence};
pub use identity::{
    IdempotencyKey, OperationId, OperationKind, OperationOwner, OperationPayloadFingerprint,
    PrincipalId, ResourceId, ScopeId, TypedResourceId,
};
pub use lease::{OperationLease, OperationLeaseEpoch, OperationLiveness};
pub use ledger::{
    OperationLedgerPort, OperationProgressEvent, OperationSubmitOutcome, OperationSubmitResult,
};
pub use status::{
    OperationFailureCode, OperationLifecycleStatus, OperationRecord, OperationStage,
    OperationSubmission, OperationTerminal,
};
