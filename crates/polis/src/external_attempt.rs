//! External-state attempt primitive.
//!
//! Use this for operations where local cluster facts cannot fully observe the
//! external state machine, such as ACME certificate issuance. Observable cluster
//! operations should use facts, projections, leases, and bounded calls instead.

pub use crate::operations::{
    AttemptBackend as Backend, AttemptBackendRequest as BackendRequest,
    AttemptBackendStart as BackendStart, AttemptEvidence as Evidence,
    AttemptEvidenceKind as EvidenceKind, AttemptFailureCodec as FailureCodec,
    AttemptFailureDecodeError as FailureDecodeError, AttemptFingerprint as Fingerprint,
    AttemptFingerprintBuilder as FingerprintBuilder, AttemptKind as Kind,
    AttemptRequest as Request, AttemptResource as Resource,
    AttemptTerminalMarker as TerminalMarker, IdempotencyKey, OperationId,
    TypedAttemptError as Error, TypedAttemptReplay as Replay, TypedAttemptRequest as TypedRequest,
    TypedAttemptResult as Result, TypedAttemptStart as Start, TypedOpenAttempt as Open,
    begin_typed_attempt as begin, interrupt_typed_attempt as interrupt,
    replay_typed_attempt as replay,
};
