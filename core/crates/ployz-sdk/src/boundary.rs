//! What crosses the addon's napi boundary: the one decode of a JavaScript value, the
//! Connection list and expected Cluster it hands over, and the errors JavaScript sees.
//!
//! `unsafe_code` is forbidden here, so the boundary cannot implement napi's value traits:
//! a method still takes and returns `serde_json::Value`, and decodes through these.

use napi::bindgen_prelude::*;
use ployz::sdk;
use ployz_core::RpcError;

/// The Cluster a connect must reach, as JavaScript names it.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClusterExpectationInput {
    sealing_public_key: ployz_core::SealingPublicKey,
    #[serde(default)]
    allow_unverified: bool,
}

/// The Cluster a dial expects its entry in, as JavaScript names it; None or null expects none.
pub(crate) fn parse_expect(
    expect: Option<serde_json::Value>,
) -> Result<Option<ployz::connect::ClusterExpectation>> {
    expect
        .filter(|value| !value.is_null())
        .map(|value| {
            from_js::<ClusterExpectationInput>(value)
                .map(|input| ployz::connect::ClusterExpectation {
                    sealing_public_key: input.sealing_public_key,
                    allow_unverified: input.allow_unverified,
                })
                .map_err(|_| invalid_argument("expect must name a base64 Sealing public key"))
        })
        .transpose()
}

/// Connection descriptors from JavaScript. A unix socket is refused unless this addon
/// was built with the `eval` feature: only the eval's fake fleet serves one, and
/// Cloud must never dial a path on its own host. The refusal reads like any malformed
/// descriptor, so it does not reveal the build's capabilities.
///
/// # Errors
/// Returns InvalidArgument for a malformed descriptor or, outside `eval`, a unix one.
pub(crate) fn parse_connections(
    connections: serde_json::Value,
) -> Result<Vec<ployz::context::Connection>> {
    let connections: Vec<ployz::context::Connection> =
        from_js(connections).map_err(|_| invalid_argument("invalid management connections"))?;
    if !cfg!(feature = "eval")
        && connections
            .iter()
            .any(|connection| matches!(connection.transport(), ployz::context::Transport::Unix(_)))
    {
        return Err(invalid_argument("invalid management connections"));
    }
    Ok(connections)
}

pub(crate) fn to_json(value: &impl serde::Serialize) -> Result<serde_json::Value> {
    serde_json::to_value(value).map_err(|error| Error::from_reason(error.to_string()))
}

/// Decode JSON JavaScript sent. Napi hands a JS whole number above `u32::MAX` over
/// as a float, which no integer field accepts, so every decode of a JS value goes
/// through here.
pub(crate) fn from_js<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
) -> std::result::Result<T, ployz_core::decode::DecodeError> {
    #[expect(clippy::disallowed_methods, reason = "the one decode of a JS value")]
    ployz_core::decode::decode(&whole_numbers(value))
}

/// Turn every float in `value` that holds a JS-safe whole number back into an integer.
pub(crate) fn whole_numbers(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    match value {
        Value::Number(number) => match number.as_f64() {
            Some(float)
                if number.is_f64() && float.fract() == 0.0 && float.abs() <= MAX_SAFE_INTEGER =>
            {
                if float >= 0.0 {
                    Value::from(float as u64)
                } else {
                    Value::from(float as i64)
                }
            }
            _ => Value::Number(number),
        },
        Value::Array(items) => Value::Array(items.into_iter().map(whole_numbers).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key, whole_numbers(value)))
                .collect(),
        ),
        other @ (Value::Null | Value::Bool(_) | Value::String(_)) => other,
    }
}

pub(crate) fn invalid_argument(error: impl std::fmt::Display) -> Error {
    rpc_to_napi(RpcError::invalid_argument(error.to_string()))
}

const RPC_ERROR_PREFIX: &str = "PLOYZ_RPC_ERROR:";

pub(crate) fn rpc_to_napi(error: RpcError) -> Error {
    match serde_json::to_string(&error) {
        Ok(json) => Error::from_reason(format!("{RPC_ERROR_PREFIX}{json}")),
        Err(_) => Error::from_reason(error.to_string()),
    }
}

/// [`rpc_to_napi`], marking with `unanswered` a failure no Machine sent, and with
/// `targetUnreached` one whose entry Machine could not dial the target.
pub(crate) fn call_to_napi(failure: sdk::CallError) -> Error {
    if !failure.unanswered && !failure.target_unreached {
        return rpc_to_napi(failure.error);
    }
    match serde_json::to_value(&failure.error) {
        Ok(serde_json::Value::Object(mut fields)) => {
            for (field, set) in [
                ("unanswered", failure.unanswered),
                ("targetUnreached", failure.target_unreached),
            ] {
                if set {
                    fields.insert(field.into(), serde_json::Value::Bool(true));
                }
            }
            let json = serde_json::Value::Object(fields);
            Error::from_reason(format!("{RPC_ERROR_PREFIX}{json}"))
        }
        _ => Error::from_reason(failure.error.to_string()),
    }
}
