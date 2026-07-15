//! Shared NATS Service API wire protocol helpers.

use async_nats::HeaderMap;

pub use async_nats::service::{
    NATS_SERVICE_ERROR as NATS_SERVICE_ERROR_HEADER,
    NATS_SERVICE_ERROR_CODE as NATS_SERVICE_ERROR_CODE_HEADER,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServiceError {
    pub code: NatsServiceErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NatsServiceErrorHeaderDecodeError {
    #[error("missing service error message header")]
    MissingMessage,
    #[error("missing service error code header")]
    MissingCode,
    #[error("invalid service error code header: {value}")]
    InvalidCode { value: String },
    #[error("unknown service error code: {code}")]
    UnknownCode { code: u16 },
}

pub fn decode_nats_service_error(
    headers: Option<&HeaderMap>,
) -> Result<Option<NatsServiceError>, NatsServiceErrorHeaderDecodeError> {
    let Some(headers) = headers else {
        return Ok(None);
    };
    let message = headers.get(NATS_SERVICE_ERROR_HEADER);
    let code = headers.get(NATS_SERVICE_ERROR_CODE_HEADER);
    match (message, code) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(NatsServiceErrorHeaderDecodeError::MissingMessage),
        (Some(_), None) => Err(NatsServiceErrorHeaderDecodeError::MissingCode),
        (Some(message), Some(code)) => {
            let code = code.as_str().parse::<u16>().map_err(|_| {
                NatsServiceErrorHeaderDecodeError::InvalidCode {
                    value: code.as_str().to_owned(),
                }
            })?;
            let Some(code) = NatsServiceErrorCode::from_http_status_code(code) else {
                return Err(NatsServiceErrorHeaderDecodeError::UnknownCode { code });
            };
            Ok(Some(NatsServiceError {
                code,
                message: message.as_str().to_owned(),
            }))
        }
    }
}

impl NatsServiceError {
    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            code: NatsServiceErrorCode::BadRequest,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            code: NatsServiceErrorCode::Conflict,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            code: NatsServiceErrorCode::Unavailable,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: NatsServiceErrorCode::Internal,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn timeout(message: impl Into<String>) -> Self {
        Self {
            code: NatsServiceErrorCode::Timeout,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn response_too_large() -> Self {
        Self {
            code: NatsServiceErrorCode::ResponseTooLarge,
            message: "response_too_large".to_owned(),
        }
    }

    pub(crate) fn into_nats_error(self) -> async_nats::service::error::Error {
        async_nats::service::error::Error {
            status: self.message,
            code: self.code.http_status_code(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NatsServiceErrorCode {
    BadRequest,
    Conflict,
    ResponseTooLarge,
    Unavailable,
    Timeout,
    Internal,
}

impl NatsServiceErrorCode {
    #[must_use]
    pub const fn http_status_code(self) -> usize {
        match self {
            Self::BadRequest => 400,
            Self::Conflict => 409,
            Self::ResponseTooLarge => 413,
            Self::Unavailable => 503,
            Self::Timeout => 504,
            Self::Internal => 500,
        }
    }

    #[must_use]
    pub const fn from_http_status_code(code: u16) -> Option<Self> {
        match code {
            400 => Some(Self::BadRequest),
            409 => Some(Self::Conflict),
            413 => Some(Self::ResponseTooLarge),
            503 => Some(Self::Unavailable),
            504 => Some(Self::Timeout),
            500 => Some(Self::Internal),
            _ => None,
        }
    }
}
