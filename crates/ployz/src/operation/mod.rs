//! Product-neutral operation context shared by Ployz domains.

mod authority;
mod claims;
mod command;
mod context;
mod identity;
mod polis_boundary;

pub use authority::{AuthorityContext, AuthorityDecision, AuthorityEpoch, AuthorityPort};
pub use claims::{ClaimGuard, ClaimHash, FenceEpoch, SubmittedFenceToken};
pub use command::{
    AttemptBackend, AttemptContext, AttemptFailureDisposition, AttemptIssuer, AttemptLog,
    AttemptProductError,
};
pub(crate) use command::{AttemptCheckpoint, IssuedAttempt};
pub(crate) use context::AttemptSpec;
pub use context::{AttemptIssue, MutationContext};
pub use identity::{
    IdempotencyKey, OperationId, PrincipalId, ResourceId, ScopeId, TypedResourceId,
};
