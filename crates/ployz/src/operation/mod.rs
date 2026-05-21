//! Product-neutral operation context shared by Ployz domains.

mod authority;
mod claims;
mod command;
mod context;
mod identity;
mod polis_boundary;

pub use authority::{AuthorityContext, AuthorityDecision, AuthorityEpoch, AuthorityPort};
pub use claims::{ClaimGuard, ClaimHash, FenceEpoch, SubmittedFenceToken};
pub use command::{CommandBackend, CommandContext, CommandEnvelope, CommandIssuer, CommandRunner};
pub use context::{CommandKind, FingerprintedResource, MutationContext, MutationIntent};
pub use identity::{
    IdempotencyKey, OperationId, PrincipalId, ResourceId, ScopeId, TypedResourceId,
};
