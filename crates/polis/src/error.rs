//! Shared primitive error categories for Polis support APIs.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum Error {
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
