//! Product-facing Ployz error categories.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum Error {
    #[error("deploy failed: {0}")]
    Deploy(#[from] DeployFailure),

    #[error("certificate operation failed: {0}")]
    Certificate(#[from] CertificateFailure),

    #[error("serving operation failed: {0}")]
    Serving(#[from] ServingFailure),

    #[error("runtime operation failed: {0}")]
    Runtime(#[from] RuntimeFailure),

    #[error("projection operation failed: {0}")]
    Projection(#[from] ProjectionFailure),

    #[error("primitive operation failed: {0}")]
    Primitive(#[from] PrimitiveFailure),
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DeployFailure {
    #[error("request is not authorized")]
    Unauthorized,
    #[error("manifest is invalid")]
    InvalidManifest,
    #[error("preflight failed")]
    PreflightFailed,
    #[error("claim was rejected")]
    ClaimRejected,
    #[error("certificate is unusable")]
    CertificateUnusable,
    #[error("runtime participant failed")]
    RuntimeParticipantFailed,
    #[error("serving activation failed")]
    ServingActivationFailed,
    #[error("operation evidence is stale")]
    StaleEvidence,
    #[error("cleanup is pending")]
    CleanupPending,
    #[error("operation was interrupted")]
    Interrupted,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum CertificateFailure {
    #[error("binding is not authorized")]
    UnauthorizedBinding,
    #[error("challenge failed")]
    ChallengeFailed,
    #[error("issuance failed")]
    IssuanceFailed,
    #[error("material is unsafe")]
    MaterialUnsafe,
    #[error("minimum lifetime safety window failed")]
    SafetyWindowFailed,
    #[error("certificate is known revoked")]
    KnownRevoked,
    #[error("revocation freshness is unknown")]
    FreshnessUnknown,
    #[error("activation was rejected")]
    ActivationRejected,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ServingFailure {
    #[error("snapshot was rejected")]
    SnapshotRejected,
    #[error("projection is stale")]
    ProjectionStale,
    #[error("certificate is unusable")]
    CertificateUnusable,
    #[error("reload failed")]
    ReloadFailed,
    #[error("last-good state is expired")]
    LastGoodExpired,
    #[error("live observation is unknown")]
    LiveObservationUnknown,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RuntimeFailure {
    #[error("target did not respond")]
    NoResponder,
    #[error("operation timed out")]
    Timeout,
    #[error("target is not authorized")]
    UnauthorizedTarget,
    #[error("payload is invalid")]
    PayloadInvalid,
    #[error("fencing token is stale")]
    StaleFence,
    #[error("lost reply conflicts with existing receipt")]
    LostReplyConflict,
    #[error("backend failed")]
    BackendFailed,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ProjectionFailure {
    #[error("view is missing")]
    MissingView,
    #[error("view is stale")]
    StaleView,
    #[error("payload is invalid")]
    InvalidPayload,
    #[error("authority proof is unknown")]
    UnknownAuthority,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum PrimitiveFailure {
    #[error("operation is not authorized")]
    Unauthorized,
    #[error("operation conflicts with existing state")]
    Conflict,
    #[error("operation timed out")]
    Timeout,
    #[error("fencing token is stale")]
    StaleFence,
    #[error("target did not respond")]
    NoResponder,
    #[error("freshness is unknown")]
    FreshnessUnknown,
    #[error("payload is malformed")]
    MalformedPayload,
    #[error("terminal marker already exists")]
    TerminalAlreadyWritten,
}
