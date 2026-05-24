//! Internal distributed-control support primitives for Ployz.
//!
//! Polis owns product-neutral mechanics such as identity, authority, facts,
//! projections, claims, and external-state attempts. It must not know Ployz
//! product domains such as deploys, certificates, routes, runtime participants,
//! or volumes.
//!
//! Production rules:
//! - primitives expose typed failures, not display-string contracts;
//! - attempt evidence is only for external state that cluster facts cannot
//!   fully observe;
//! - claims are advisory until a product resource enforces the fence;
//! - external I/O must be deadline-bounded by the adapter using the primitive.

pub mod authority;
pub mod claims;
pub mod error;
pub mod external_attempt;
pub mod facts;
pub mod identity;
pub mod membership;
mod operations;
pub mod peers;
pub mod projection;
pub mod store;

pub use authority::{
    Authority, AuthorityContext, AuthorityDecision, AuthorityService, Authorized, GrantEpoch,
};
pub use claims::{
    ClaimEpoch, ClaimGuard, ClaimHash, ClaimLease, FenceCheck, FenceToken, HolderId, ResourceId,
};
pub use error::{Error, Result};
pub use facts::{
    CandidateStatus, FactAppendFingerprint, FactAppendOutcome, FactAppendRequest, FactAppendScope,
    FactAppendValidation, FactCandidate, FactCandidateSet, FactConflict, FactConflictPolicy,
    FactCursor, FactGrantAuthority, FactGrantDecision, FactGrantPurpose, FactGrantService, FactId,
    FactKey, FactKind, FactPayload, FactPayloadBatch, FactPayloadDigest, FactPayloadReadFailure,
    FactQuery, FactReceipt, FactRejection, FactReplayKey, FactStore, FactTarget, FactWriteGrant,
    MemoryFactStore, ValidatedFactAppend,
};
pub use identity::{IrohEndpointId, PrincipalId, ScopeId, SourceWatermark};
pub use membership::{
    CapabilitiesJson, IslandId, MachineRow, MembershipLifecycle, NamespaceId,
    NamespaceMembershipRow, NamespaceRole, NamespaceRow, OverlayIp, RowEpoch, StoreMachineId,
    WireGuardPublicKey, active_machine_statement, machine_row_from_store_row,
    membership_schema_statements, select_machine_statement, upsert_machine_statement,
    upsert_namespace_membership_statement, upsert_namespace_statement,
    wireguard_peers_for_machine_statement,
};
pub use operations::{IdempotencyKey, OperationId, SubmittedFenceFingerprint};
pub use peers::{
    FakePeerProbe, PLOYZ_PEER_ALPN, PeerEndpoint, PeerError, PeerIdentity, PeerProbe,
    PeerProbeDeadline, PeerProbeReceipt, PeerProbeResult, PeerRpcProbe, PeerRpcService, PeerTicket,
    PeerTicketPath, import_ticket, issue_ticket, load_or_create_identity, preflight_membership,
};
pub use projection::{
    FactReducer, MemoryProjectionSource, ProjectionCatchUp, ProjectionCatchUpRequest,
    ProjectionError, ProjectionFreshness, ProjectionHealth, ProjectionKey, ProjectionRequest,
    ProjectionRequestIdentity, ProjectionSnapshot, ProjectionSource, ProjectionView, VerifiedFact,
};
pub use store::{
    CorrosionStore, StoreChangeId, StoreChangeType, StoreError, StoreParam, StoreQueryEvent,
    StoreQueryRows, StoreResult, StoreRow, StoreStatement, StoreSubscription,
    StoreSubscriptionEvent, StoreTimeout, StoreUpdate, StoreUpdateEvent, StoreValue,
    TransactionReceipt,
};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_has_no_product_modules() {
        let public_modules = [
            "authority",
            "claims",
            "error",
            "facts",
            "identity",
            "projection",
        ];

        assert!(!public_modules.contains(&"deploy"));
        assert!(!public_modules.contains(&"acme"));
        assert!(!public_modules.contains(&"serving"));
        assert!(!public_modules.contains(&"runtime"));
        assert!(!public_modules.contains(&"volume"));
    }
}
