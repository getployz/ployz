//! Reading what Ployz Cloud answers, in one place: the sentence a refusal carries, the code a
//! route's `PublicError` names, whether a run is still going, and the words an older Cloud
//! sends where a newer one could send a code.

use ployz_core::{RpcError, RpcErrorCode};
use serde::Deserialize;
use serde_json::Value;

/// The sentence a Cloud refusal carries: a route's `PublicError`, the `{error}` a
/// public endpoint sends, or the `{error: {code, message}}` a command refuses with.
pub(crate) fn message(body: &[u8]) -> Option<String> {
    let body = serde_json::from_slice::<Value>(body).ok()?;
    let text =
        |value: &Value, field: &str| value.get(field).and_then(Value::as_str).map(str::to_owned);
    match text(&body, "_tag").as_deref() {
        Some("PublicError") => text(&body, "code").and(text(&body, "message")),
        _ => {
            let error = body.get("error")?;
            if let Some(said) = error.as_str() {
                return Some(said.to_owned());
            }
            text(error, "code").and(text(error, "message"))
        }
    }
}

/// Cloud's own sentence for a failed call, else its status and any readable body.
pub(crate) fn answered(prefix: &str, status: u16, body: &str) -> String {
    message(body.as_bytes()).unwrap_or_else(|| format!("{prefix} HTTP {status}{}", readable(body)))
}

/// `body` as the tail of a status line: a short text or JSON answer is worth
/// printing, a proxy's HTML error page is not.
fn readable(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() || body.starts_with('<') {
        String::new()
    } else {
        format!(": {body}")
    }
}

/// The `--json` code of a Cloud HTTP refusal.
pub(crate) fn status_code(status: u16) -> RpcErrorCode {
    match status {
        401 | 403 => RpcErrorCode::Unauthenticated,
        404 => RpcErrorCode::NotFound,
        409 => RpcErrorCode::Conflict,
        408 | 429 => RpcErrorCode::Unavailable,
        400..=499 => RpcErrorCode::InvalidArgument,
        _ => RpcErrorCode::Unavailable,
    }
}

/// The `code` of a route's `PublicError`. Cloud may name one this build does not know.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PublicCode {
    Conflict,
    NotFound,
    ValidationFailed,
    Unauthorized,
    Forbidden,
    RateLimited,
    Unavailable,
    Unknown(String),
}

impl PublicCode {
    fn from_wire(code: &str) -> Self {
        match code {
            "CONFLICT" => Self::Conflict,
            "NOT_FOUND" => Self::NotFound,
            "VALIDATION_FAILED" => Self::ValidationFailed,
            "UNAUTHORIZED" => Self::Unauthorized,
            "FORBIDDEN" => Self::Forbidden,
            "RATE_LIMITED" => Self::RateLimited,
            "UNAVAILABLE" => Self::Unavailable,
            other => Self::Unknown(other.to_owned()),
        }
    }

    fn rpc_code(&self) -> RpcErrorCode {
        match self {
            Self::Conflict => RpcErrorCode::Conflict,
            Self::NotFound => RpcErrorCode::NotFound,
            Self::ValidationFailed => RpcErrorCode::InvalidArgument,
            Self::Unauthorized | Self::Forbidden => RpcErrorCode::Unauthenticated,
            Self::RateLimited | Self::Unavailable => RpcErrorCode::Unavailable,
            Self::Unknown(_) => RpcErrorCode::Internal,
        }
    }
}

/// The `{error: {code, message, details}}` a Cloud command refuses with, as the refusal it is.
pub(crate) fn command_refusal(body: &[u8]) -> Option<RpcError> {
    #[derive(Deserialize)]
    struct Refusal {
        error: Refused,
    }
    /// A Cloud command's refusal also names the input field, what fits there and
    /// whether it wrote. They ride in `details`, where a write's `writes` already does.
    #[derive(Deserialize)]
    struct Refused {
        #[serde(flatten)]
        error: RpcError,
        field: Option<Value>,
        expected: Option<Value>,
        writes: Option<Value>,
    }
    let Refused {
        mut error,
        field,
        expected,
        writes,
    } = serde_json::from_slice::<Refusal>(body).ok()?.error;
    let told = [("field", field), ("expected", expected), ("writes", writes)]
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value)));
    for (key, value) in told {
        if !error.details.is_object() {
            error.details = serde_json::json!({});
        }
        if let Value::Object(details) = &mut error.details {
            details.insert(key, value);
        }
    }
    Some(error)
}

/// A Cloud route's `PublicError`, as the refusal it stands for.
pub(crate) fn public_error(body: &[u8]) -> Option<RpcError> {
    #[derive(Deserialize)]
    #[serde(tag = "_tag")]
    enum Public {
        PublicError { code: String, message: String },
    }
    let Public::PublicError { code, message } = serde_json::from_slice(body).ok()?;
    Some(RpcError::new(
        PublicCode::from_wire(&code).rpc_code(),
        message,
    ))
}

/// Where a Cloud run of an operation stands in its `state`: going, or settled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunState {
    Pending,
    Running,
    /// Any other `state`, or none: the run's answer is what the caller decodes.
    Settled,
}

impl RunState {
    pub(crate) fn of(run: &Value) -> Self {
        match run.get("state").and_then(Value::as_str) {
            Some("pending") => Self::Pending,
            Some("running") => Self::Running,
            _ => Self::Settled,
        }
    }

    pub(crate) fn going(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
}

/// The `code` of a run that ended without a result. Cloud may name one this build does not know.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum EndedCode {
    Refused,
    /// The Server was busy and took nothing; the run ended with its words.
    Busy,
    Unknown(String),
}

impl EndedCode {
    pub(crate) fn from_wire(code: &str) -> Self {
        match code {
            "refused" => Self::Refused,
            "busy" => Self::Busy,
            other => Self::Unknown(other.to_owned()),
        }
    }

    pub(crate) fn rpc_code(&self) -> RpcErrorCode {
        match self {
            Self::Refused => RpcErrorCode::Conflict,
            Self::Busy | Self::Unknown(_) => RpcErrorCode::Unavailable,
        }
    }
}

/// Whether a refused start of a Cloud upgrade names the Release Channel the Organization
/// follows, which `RpcErrorCode` has no word for. Its `details.channel` says which.
pub(crate) fn channel_mismatch(refused: &RpcError) -> bool {
    refused.code.as_str() == "channel_mismatch"
}

#[cfg(test)]
#[path = "reply_tests.rs"]
mod tests;
