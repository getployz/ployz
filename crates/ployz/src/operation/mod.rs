//! Product-neutral operation context shared by Ployz domains.

mod authority;
mod claims;
mod context;
mod identity;

pub use authority::{AuthorityContext, AuthorityDecision, AuthorityEpoch, AuthorityPort};
pub use claims::{ClaimGuard, ClaimHash, FenceEpoch, SubmittedFenceToken};
pub use context::{MutationAuthorizer, MutationContext, MutationContextRequest};
pub use identity::{
    IdempotencyKey, OperationId, PrincipalId, ResourceId, ScopeId, TypedResourceId,
};
