//! The Store's errors: the RPC error vocabulary. An invalid value is never echoed back.

use ployz_core::{RpcError, RpcErrorCode};
use serde_json::Value;

fn error(code: RpcErrorCode, message: impl Into<String>, details: Value) -> RpcError {
    RpcError {
        code,
        message: message.into(),
        details,
    }
}

pub(crate) fn invalid(message: impl Into<String>, details: Value) -> RpcError {
    error(RpcErrorCode::InvalidArgument, message, details)
}

pub(crate) fn not_found(message: impl Into<String>, details: Value) -> RpcError {
    error(RpcErrorCode::NotFound, message, details)
}

pub(crate) fn ambiguous(message: impl Into<String>, details: Value) -> RpcError {
    error(RpcErrorCode::Ambiguous, message, details)
}

pub(crate) fn conflict(message: impl Into<String>, details: Value) -> RpcError {
    error(RpcErrorCode::Conflict, message, details)
}

pub(crate) fn unavailable(message: impl Into<String>) -> RpcError {
    error(RpcErrorCode::Unavailable, message, Value::Null)
}

pub(crate) fn internal(message: impl Into<String>) -> RpcError {
    error(RpcErrorCode::Internal, message, Value::Null)
}

/// Stored data the Store cannot read back: a bug or a hand edit, never user input.
pub(crate) fn corrupt(what: &str) -> RpcError {
    internal(format!("The Config Store holds an unreadable {what}"))
}
