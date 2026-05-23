use thiserror::Error;

use crate::model::ServingFailure;

pub type ServingResult<T> = Result<T, ServingError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ServingError {
    #[error("serving snapshot load failed: {failure}")]
    SnapshotLoad { failure: ServingFailure },
    #[error("serving actor unavailable during {operation}: {reason}")]
    ActorUnavailable {
        operation: &'static str,
        reason: String,
    },
}

impl ServingError {
    #[must_use]
    pub fn failure(&self) -> ServingFailure {
        match self {
            Self::SnapshotLoad { failure } => failure.clone(),
            Self::ActorUnavailable { operation, reason } => {
                ServingFailure::actor(operation, reason.clone())
            }
        }
    }
}
