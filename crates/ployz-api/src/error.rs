use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type ProtocolResult<T> = Result<T, ProtocolError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolErrorCode {
    Parse,
    MalformedPayload,
    UnsupportedMethod,
    UnsupportedOperation,
    OperationNotFound,
    OperationConflict,
    OperationInterrupted,
    Unauthorized,
    Timeout,
    NoResponder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{code:?}: {message}")]
pub struct ProtocolError {
    code: ProtocolErrorCode,
    message: String,
    method: Option<String>,
    operation: Option<String>,
}

impl ProtocolError {
    #[must_use]
    pub fn new(code: ProtocolErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            method: None,
            operation: None,
        }
    }

    #[must_use]
    pub fn parse(message: impl Into<String>) -> Self {
        Self::new(ProtocolErrorCode::Parse, message)
    }

    #[must_use]
    pub fn unsupported_method(method: impl Into<String>) -> Self {
        let method = method.into();
        Self::new(
            ProtocolErrorCode::UnsupportedMethod,
            format!("method {method} is not supported"),
        )
        .with_method(method)
    }

    #[must_use]
    pub fn malformed_payload(message: impl Into<String>) -> Self {
        Self::new(ProtocolErrorCode::MalformedPayload, message)
    }

    #[must_use]
    pub fn with_method(mut self, method: impl Into<String>) -> Self {
        self.method = Some(method.into());
        self
    }

    #[must_use]
    pub fn with_operation(mut self, operation: impl Into<String>) -> Self {
        self.operation = Some(operation.into());
        self
    }

    #[must_use]
    pub fn code(&self) -> ProtocolErrorCode {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub fn method(&self) -> Option<&str> {
        self.method.as_deref()
    }

    #[must_use]
    pub fn operation(&self) -> Option<&str> {
        self.operation.as_deref()
    }
}

impl From<ployz::PrimitiveFailure> for ProtocolError {
    fn from(value: ployz::PrimitiveFailure) -> Self {
        match value {
            ployz::PrimitiveFailure::Unauthorized => {
                Self::new(ProtocolErrorCode::Unauthorized, value.to_string())
            }
            ployz::PrimitiveFailure::Conflict
            | ployz::PrimitiveFailure::OperationStateConflict
            | ployz::PrimitiveFailure::OperationAlreadySucceeded
            | ployz::PrimitiveFailure::OperationInProgress
            | ployz::PrimitiveFailure::OperationAlreadyFailed => {
                Self::new(ProtocolErrorCode::OperationConflict, value.to_string())
            }
            ployz::PrimitiveFailure::Timeout => {
                Self::new(ProtocolErrorCode::Timeout, value.to_string())
            }
            ployz::PrimitiveFailure::NoResponder => {
                Self::new(ProtocolErrorCode::NoResponder, value.to_string())
            }
            ployz::PrimitiveFailure::MalformedPayload => {
                Self::new(ProtocolErrorCode::MalformedPayload, value.to_string())
            }
            ployz::PrimitiveFailure::OperationInterrupted
            | ployz::PrimitiveFailure::StaleFence
            | ployz::PrimitiveFailure::FreshnessUnknown => {
                Self::new(ProtocolErrorCode::OperationInterrupted, value.to_string())
            }
        }
    }
}
