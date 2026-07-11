//! Failure classification for machine-scoped transport calls.

use ployz_core::ids::MachineId;
use ployz_core::ops::FailureMessage;

/// Transport and responder failures shared by machine-scoped RPC calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineRuntimeUnavailableReason {
    EncodeRequest { message: String },
    RequestTimedOut,
    NoResponders,
    InvalidSubject,
    MaxPayloadExceeded,
    RequestFailed { message: String },
    ServiceBadRequest { message: String },
    ServiceConflict { message: String },
    ServiceUnavailable { message: String },
    ServiceTimedOut { message: String },
    ServiceInternal { message: String },
    MalformedServiceError { message: String },
    DecodeResponse { message: String },
    WrongResponder { actual_machine_id: MachineId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineRequestFailure {
    NoAnswer,
    TimedOut,
    RequestFailed { message: FailureMessage },
    ProtocolFailed { message: FailureMessage },
    DecodeFailed { message: FailureMessage },
    WrongResponder { actual_machine_id: MachineId },
}

impl MachineRuntimeUnavailableReason {
    pub(crate) fn failure_message(&self) -> FailureMessage {
        let message = match self {
            Self::EncodeRequest { message } => {
                format!("machine runtime request could not be encoded: {message}")
            }
            Self::RequestTimedOut => "machine runtime request timed out".to_owned(),
            Self::NoResponders => "machine runtime has no responders".to_owned(),
            Self::InvalidSubject => "machine runtime subject was invalid".to_owned(),
            Self::MaxPayloadExceeded => {
                "machine runtime request exceeded NATS max payload".to_owned()
            }
            Self::RequestFailed { message } => format!("machine runtime request failed: {message}"),
            Self::ServiceBadRequest { message } => {
                format!("machine runtime rejected the request: {message}")
            }
            Self::ServiceConflict { message } => {
                format!("machine runtime reported a conflict: {message}")
            }
            Self::ServiceUnavailable { message } => {
                format!("machine runtime service unavailable: {message}")
            }
            Self::ServiceTimedOut { message } => {
                format!("machine runtime service timed out: {message}")
            }
            Self::ServiceInternal { message } => {
                format!("machine runtime service failed internally: {message}")
            }
            Self::MalformedServiceError { message } => {
                format!("machine runtime returned malformed service error headers: {message}")
            }
            Self::DecodeResponse { message } => {
                format!("machine runtime response could not be decoded: {message}")
            }
            Self::WrongResponder { actual_machine_id } => {
                format!(
                    "machine runtime replied for a different machine: {}",
                    actual_machine_id.as_str()
                )
            }
        };
        FailureMessage::try_new(message).expect("generated runtime failure message is non-empty")
    }

    pub(crate) fn into_request_failure(self) -> MachineRequestFailure {
        match self {
            Self::NoResponders => MachineRequestFailure::NoAnswer,
            Self::RequestTimedOut | Self::ServiceTimedOut { .. } => MachineRequestFailure::TimedOut,
            Self::MalformedServiceError { message } => MachineRequestFailure::ProtocolFailed {
                message: request_failure_message(message),
            },
            Self::DecodeResponse { message } => MachineRequestFailure::DecodeFailed {
                message: request_failure_message(message),
            },
            Self::WrongResponder { actual_machine_id } => {
                MachineRequestFailure::WrongResponder { actual_machine_id }
            }
            Self::EncodeRequest { message } => MachineRequestFailure::RequestFailed {
                message: request_failure_message(format!("failed to encode request: {message}")),
            },
            Self::InvalidSubject => MachineRequestFailure::RequestFailed {
                message: request_failure_message("request failed: invalid subject".to_owned()),
            },
            Self::MaxPayloadExceeded => MachineRequestFailure::RequestFailed {
                message: request_failure_message("request failed: max payload exceeded".to_owned()),
            },
            Self::RequestFailed { message } => MachineRequestFailure::RequestFailed {
                message: request_failure_message(format!("request failed: {message}")),
            },
            Self::ServiceBadRequest { message }
            | Self::ServiceConflict { message }
            | Self::ServiceUnavailable { message }
            | Self::ServiceInternal { message } => MachineRequestFailure::RequestFailed {
                message: request_failure_message(format!("service returned an error: {message}")),
            },
        }
    }
}

fn request_failure_message(message: String) -> FailureMessage {
    FailureMessage::try_new(message).expect("request failure message is non-empty")
}
