use napi::bindgen_prelude::*;
use napi_derive::napi;
use ployz_store::tools::{self, Answer, CloudQuery, Place};

use crate::{Client, invalid_argument, rpc_to_napi, to_json};

/// What a model is told: `{ instructions, tools }`.
///
/// # Errors
/// Returns an error when the catalog fails to encode.
#[napi]
pub fn agent_tools() -> Result<serde_json::Value> {
    to_json(&serde_json::json!({
        "instructions": tools::INSTRUCTIONS,
        "tools": tools::catalog(),
    }))
}

/// Start `tool` on `input` at `place`: `{ step, state }`. `call_key` is the
/// transport's ID for this call, stable across its retries.
///
/// # Errors
/// Returns `invalid_argument` when `place` is malformed. A bad tool or input is a
/// refusal in the step, not an error.
#[napi]
pub fn agent_start(
    tool: String,
    input: serde_json::Value,
    place: serde_json::Value,
    call_key: String,
) -> Result<serde_json::Value> {
    let place: Place = ployz_core::decode::decode(&place).map_err(invalid_argument)?;
    let (step, state) = tools::start(&tool, input, &place, &call_key);
    turn(&step, &state)
}

/// Resume a call with the answers to its last step, in order: `{ step, state }`.
///
/// # Errors
/// Returns `invalid_argument` when `state` or `answers` is malformed.
#[napi]
pub fn agent_resume(
    state: serde_json::Value,
    answers: serde_json::Value,
) -> Result<serde_json::Value> {
    let state: tools::State = ployz_core::decode::decode(&state).map_err(invalid_argument)?;
    let answers: Vec<Answer> = ployz_core::decode::decode(&answers).map_err(invalid_argument)?;
    let (step, state) = tools::resume(state, answers.into_iter().map(Into::into).collect());
    turn(&step, &state)
}

fn turn(step: &tools::Step, state: &tools::State) -> Result<serde_json::Value> {
    to_json(&serde_json::json!({ "step": step, "state": state }))
}

#[napi]
impl Client {
    /// Answer a `CloudQuery` from this session's Servers. Servers that do not answer
    /// are named in `seen.unanswered`.
    ///
    /// # Errors
    /// Returns `invalid_argument` for a malformed query, `unsupported` for one only
    /// Cloud answers, or the session's error.
    #[napi]
    pub async fn agent_ask(&self, query: serde_json::Value) -> Result<serde_json::Value> {
        let query: CloudQuery = ployz_core::decode::decode(&query).map_err(invalid_argument)?;
        ployz::sdk::agent::ask(&self.inner, query)
            .await
            .map_err(rpc_to_napi)
    }
}
