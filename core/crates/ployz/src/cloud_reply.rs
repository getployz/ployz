//! Reading what Ployz Cloud answers, in one place: the sentence a refusal carries, the code a
//! route's `PublicError` names, whether a run is still going, and the words an older Cloud
//! sends where a newer one could send a code.
//!
//! Self-hosted Clouds pin old releases, so the word matches under [`checkin_hold`] and
//! [`checkin_refused`] have no end date: a Cloud's words are read exactly as they were.

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
            other => Self::Unknown(other.to_owned()),
        }
    }

    fn rpc_code(&self) -> RpcErrorCode {
        match self {
            Self::Conflict => RpcErrorCode::Conflict,
            Self::NotFound => RpcErrorCode::NotFound,
            Self::ValidationFailed => RpcErrorCode::InvalidArgument,
            Self::Unauthorized | Self::Forbidden => RpcErrorCode::Unauthenticated,
            Self::RateLimited => RpcErrorCode::Unavailable,
            Self::Unknown(_) => RpcErrorCode::Internal,
        }
    }
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

/// Why Cloud held a check-in back: it is busy, or the token has all the waiting servers it allows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hold {
    Busy,
    Full,
}

// Cloud's own words. Only these mean Cloud refused the token or held a check-in back; any other
// 401 or 429 may come from something in between.
const CLOUD_REFUSAL: &str = "Authentication is required.";
const CLOUD_BUSY: [&str; 2] = ["Too many check-ins.", "Busy, retry."];
const CLOUD_FULL: (&str, &str) = ("This Fleet Token already has ", " waiting servers.");

/// Cloud holds a check-in back with `{error}` only, the shape its 429 sends.
pub(crate) fn checkin_hold(body: &[u8]) -> Option<(Hold, String)> {
    let body = serde_json::from_slice::<Value>(body).ok()?;
    let said = body.get("error")?.as_str()?;
    if CLOUD_BUSY.contains(&said) {
        return Some((Hold::Busy, said.to_owned()));
    }
    let cap = said
        .strip_prefix(CLOUD_FULL.0)?
        .strip_suffix(CLOUD_FULL.1)?;
    (!cap.is_empty() && cap.bytes().all(|digit| digit.is_ascii_digit()))
        .then(|| (Hold::Full, said.to_owned()))
}

/// Whether a 401's body is Cloud's own refusal of the Fleet Token.
pub(crate) fn checkin_refused(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .is_ok_and(|body| body.get("error").and_then(Value::as_str) == Some(CLOUD_REFUSAL))
}

#[cfg(test)]
#[path = "cloud_reply_tests.rs"]
mod tests;
