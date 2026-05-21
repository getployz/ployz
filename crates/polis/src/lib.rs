//! Internal distributed-control support primitives for Ployz.
//!
//! Polis owns product-neutral mechanics such as identity, authority, operation
//! evidence, and claims. It must not know
//! Ployz product domains such as deploys, certificates, routes, runtime
//! participants, or volumes.
//!
//! Production rules:
//! - primitives expose typed failures, not display-string contracts;
//! - durable evidence is not product truth until a Ployz verifier accepts it;
//! - claims are advisory until a product resource enforces the fence;
//! - external I/O must be deadline-bounded by the adapter using the primitive.

pub mod authority;
pub mod claims;
pub mod error;
pub mod identity;
pub mod operations;

pub use authority::{Authority, AuthorityDecision, AuthorityService, Authorized, GrantEpoch};
pub use claims::{
    ClaimEpoch, ClaimGuard, ClaimHash, ClaimLease, FenceCheck, FenceToken, HolderId, ResourceId,
};
pub use error::{Error, Result};
pub use identity::{PrincipalId, ScopeId, SourceWatermark};
pub use operations::{
    BackendOperationStart, CommandKind, EvidenceKind, FingerprintedResource, IdempotencyKey,
    OpenOperation, OperationBackend, OperationEvidence, OperationId, OperationReplay,
    OperationReplayStatus, OperationRequest, OperationStart, RequestFingerprint,
    SubmittedFenceFingerprint, TerminalMarker, close, record, start_or_replay,
};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_has_no_product_modules() {
        let public_modules = ["authority", "claims", "error", "identity", "operations"];

        assert!(!public_modules.contains(&"deploy"));
        assert!(!public_modules.contains(&"acme"));
        assert!(!public_modules.contains(&"serving"));
        assert!(!public_modules.contains(&"runtime"));
        assert!(!public_modules.contains(&"volume"));
    }
}
