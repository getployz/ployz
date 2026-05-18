use mvp_bus::{BusError, FactKey, FactKeyParseError};
use mvp_projection::{CandidateStatus, FactSourceError};
use thiserror::Error;

use crate::{EnvironmentEpoch, EnvironmentId};

pub type EnvironmentResult<T> = Result<T, EnvironmentError>;

#[derive(Debug, Error)]
pub enum EnvironmentError {
    #[error("{label} is invalid: {value:?}")]
    InvalidIdentifier { label: &'static str, value: String },
    #[error("environment epoch must be non-zero, got {value}")]
    InvalidEpoch { value: u64 },
    #[error("environment epoch overflow at {epoch}")]
    EpochOverflow { epoch: EnvironmentEpoch },
    #[error("environment fact key has the wrong shape: {key}")]
    WrongFactKeyShape { key: FactKey },
    #[error("environment fact key {key} does not match payload environment {environment}")]
    FactKeyPayloadMismatch {
        key: FactKey,
        environment: EnvironmentId,
    },
    #[error("environment head candidate {key} is not readable: {status:?}")]
    UnreadableHeadCandidate {
        key: FactKey,
        status: CandidateStatus,
    },
    #[error("environment candidate {key} is missing payload bytes")]
    MissingPayload { key: FactKey },
    #[error("environment fact payload decode failed for {key}: {message}")]
    DecodePayload { key: FactKey, message: String },
    #[error("environment {environment} has no current head")]
    MissingCurrentHead { environment: EnvironmentId },
    #[error(
        "environment {environment} changed before command mutation: expected epoch {expected}, actual epoch {actual:?}"
    )]
    StaleExpectedEpoch {
        environment: EnvironmentId,
        expected: EnvironmentEpoch,
        actual: Option<EnvironmentEpoch>,
    },
    #[error("environment fact already has a conflicting candidate: {key}")]
    FactConflict { key: FactKey },
    #[error("environment head {key} has no rollback target")]
    RollbackTargetMissing { key: FactKey },
    #[error("environment decision does not match head fact {key}")]
    DecisionHeadMismatch { key: FactKey },
    #[error("environment fact serialization failed: {message}")]
    Serialization { message: String },
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error(transparent)]
    FactKeyParse(#[from] FactKeyParseError),
    #[error(transparent)]
    FactSource(#[from] FactSourceError),
}
