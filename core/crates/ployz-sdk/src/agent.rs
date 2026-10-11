use napi::bindgen_prelude::*;
use napi_derive::napi;
use ployz_core::{ContainerId, MachineId, Namespace, ServiceName};
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

    /// Every Container of `service` in `namespace`: `{ copies, unanswered }`.
    ///
    /// # Errors
    /// Returns `invalid_argument` for a malformed name, or the session's error.
    #[napi]
    pub async fn observe_service(
        &self,
        namespace: String,
        service: String,
    ) -> Result<serde_json::Value> {
        let namespace = Namespace::parse(&namespace).map_err(invalid_argument)?;
        let service = ServiceName::parse(&service).map_err(invalid_argument)?;
        let copies = ployz::sdk::agent::observe_service(&self.inner, &namespace, &service)
            .await
            .map_err(rpc_to_napi)?;
        to_json(&copies)
    }

    /// Stop Container `container` on `machine`.
    ///
    /// # Errors
    /// Returns the Machine's refusal, or the transport's error when it did not answer.
    #[napi]
    pub async fn stop_container(&self, machine: String, container: String) -> Result<()> {
        let (machine, container) = ids(&machine, &container)?;
        ployz::sdk::agent::stop_one(&self.inner, machine, container)
            .await
            .map_err(rpc_to_napi)
    }

    /// Start Container `container` on `machine`. True once it serves; false when it
    /// started but was not seen serving within the bounded wait.
    ///
    /// # Errors
    /// Returns the Machine's refusal, or the transport's error when it did not answer.
    #[napi]
    pub async fn start_container(&self, machine: String, container: String) -> Result<bool> {
        let (machine, container) = ids(&machine, &container)?;
        ployz::sdk::agent::start_one(&self.inner, machine, container)
            .await
            .map(|started| started == ployz::sdk::agent::Started::Serving)
            .map_err(rpc_to_napi)
    }
}

fn ids(machine: &str, container: &str) -> Result<(MachineId, ContainerId)> {
    Ok((
        MachineId::parse(machine).map_err(invalid_argument)?,
        ContainerId::parse(container).map_err(invalid_argument)?,
    ))
}
