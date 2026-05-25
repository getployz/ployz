//! Product-facing Ployz error categories.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum Error {
    #[error("deploy failed: {0}")]
    Deploy(#[from] DeployFailure),

    #[error("domain operation failed: {0}")]
    Domain(#[from] crate::domain::DomainFailure),

    #[error("certificate operation failed: {0}")]
    Certificate(#[from] CertificateFailure),

    #[error("serving operation failed: {0}")]
    Serving(#[from] ServingFailure),

    #[error("runtime operation failed: {0}")]
    Runtime(#[from] RuntimeFailure),

    #[error("machine operation failed: {0}")]
    Machine(#[from] MachineFailure),

    #[error("volume operation failed: {0}")]
    Volume(#[from] VolumeFailure),

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
    #[error("domain readiness failed")]
    DomainReadinessFailed,
    #[error("runtime participant failed")]
    RuntimeParticipantFailed,
    #[error("serving activation failed")]
    ServingActivationFailed,
    #[error("serving operation failed: {0}")]
    ServingFailed(ServingFailure),
    #[error("operation evidence is stale")]
    StaleEvidence,
    #[error("operation already succeeded")]
    OperationAlreadySucceeded,
    #[error("operation is still in progress")]
    OperationInProgress,
    #[error("operation already failed")]
    OperationAlreadyFailed,
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
pub enum MachineFailure {
    #[error("payload is invalid")]
    InvalidPayload,
    #[error("membership projection is unavailable")]
    ProjectionUnavailable,
    #[error("membership projection timed out")]
    ProjectionTimeout,
    #[error("membership projection payload is invalid")]
    ProjectionPayloadInvalid,
    #[error("membership projection stream was interrupted")]
    ProjectionStreamInterrupted,
    #[error("membership projection missed changes")]
    ProjectionMissedChanges,
    #[error("machine membership conflicts for {machine:?} at epoch {epoch:?}")]
    MembershipConflict {
        machine: crate::machine::MachineId,
        epoch: Option<crate::machine::MachineEpoch>,
    },
    #[error("machine {machine:?} is tombstoned at epoch {epoch:?}")]
    MachineTombstoned {
        machine: crate::machine::MachineId,
        epoch: crate::machine::MachineEpoch,
    },
    #[error("machine {machine:?} is currently being removed at epoch {epoch:?}")]
    MachineRemoving {
        machine: crate::machine::MachineId,
        epoch: crate::machine::MachineEpoch,
    },
    #[error("membership mutation was rejected")]
    MutationRejected,
    #[error("machine {machine:?} failed peer preflight")]
    PeerPreflightFailed { machine: crate::machine::MachineId },
    #[error("membership mutation result did not match desired machine {machine:?}")]
    MutationMismatch { machine: crate::machine::MachineId },
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum VolumeFailure {
    #[error("source is not the current owner")]
    SourceNotOwner,
    #[error("fencing token is stale")]
    StaleFence,
    #[error("transfer is already in progress")]
    TransferInProgress,
    #[error("ownership observation is stale")]
    StaleOwnership,
    #[error("source writes are still open")]
    SourceWriteStillOpen,
    #[error("snapshot failed")]
    SnapshotFailed,
    #[error("receive failed")]
    ReceiveFailed,
    #[error("ownership commit was rejected")]
    OwnershipCommitRejected,
    #[error("cleanup failed")]
    CleanupFailed,
    #[error("operation already succeeded")]
    OperationAlreadySucceeded,
    #[error("operation is still in progress")]
    OperationInProgress,
    #[error("operation already failed")]
    OperationAlreadyFailed,
    #[error("operation was interrupted")]
    Interrupted,
    #[error("payload is invalid")]
    InvalidPayload,
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
    #[error("operation state conflicts with existing result")]
    OperationStateConflict,
    #[error("operation already succeeded")]
    OperationAlreadySucceeded,
    #[error("operation is still in progress")]
    OperationInProgress,
    #[error("operation already failed")]
    OperationAlreadyFailed,
    #[error("operation was interrupted")]
    OperationInterrupted,
}
