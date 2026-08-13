use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use ployz_core::{
    CapabilityName, ContractDescription, DESCRIBE_CONTRACT_CAPABILITY, MachineRpc, OpaquePayload,
    PROTOCOL_MAJOR, RESET_MACHINE_CAPABILITY, RpcError, RpcErrorCode, RpcRequestBody, RpcResponse,
};
use serde_json::Value;
use tokio::sync::watch;
use tonic::{Request, Response, Status};

use crate::machine::{StateError, StateStore};

#[derive(Clone)]
pub struct MachineService {
    state: Arc<Mutex<StateStore>>,
    reset: watch::Sender<bool>,
}

impl MachineService {
    #[must_use]
    pub fn new(state: Arc<Mutex<StateStore>>, reset: watch::Sender<bool>) -> Self {
        Self { state, reset }
    }
}

#[tonic::async_trait]
impl MachineRpc for MachineService {
    async fn describe_contract(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = request
            .into_inner()
            .decode_request()
            .map_err(invalid_request)?;
        if !matches!(request.body, RpcRequestBody::DescribeContract(_)) {
            return Err(Status::invalid_argument(
                "expected describe_contract request",
            ));
        }
        let machine_id = self
            .state
            .lock()
            .map_err(|_| Status::internal("machine state lock poisoned"))?
            .local_state()
            .id
            .clone();
        let capabilities = [DESCRIBE_CONTRACT_CAPABILITY, RESET_MACHINE_CAPABILITY]
            .into_iter()
            .map(|name| CapabilityName::parse(name).expect("static capability name is valid"))
            .collect::<BTreeSet<_>>();
        let response = RpcResponse::contract_description(ContractDescription {
            machine_id,
            protocol_major: PROTOCOL_MAJOR,
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            capabilities,
        });
        Ok(Response::new(response.encode().map_err(internal_response)?))
    }

    async fn reset(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = request
            .into_inner()
            .decode_request()
            .map_err(invalid_request)?;
        if !matches!(request.body, RpcRequestBody::Reset(_)) {
            return Err(Status::invalid_argument("expected reset request"));
        }
        let reset = self
            .state
            .lock()
            .map_err(|_| Status::internal("machine state lock poisoned"))?
            .begin_reset();
        let accepted = reset.is_ok();
        let response = match reset {
            Ok(()) => RpcResponse::reset_accepted(),
            Err(error) => RpcResponse::error(state_error(error)),
        }
        .encode()
        .map_err(internal_response)?;
        if accepted {
            self.reset.send_replace(true);
        }
        Ok(Response::new(response))
    }
}

fn state_error(error: StateError) -> RpcError {
    let code = match error {
        StateError::AlreadyResetting => RpcErrorCode::Conflict,
        StateError::Io(_)
        | StateError::Json(_)
        | StateError::InvalidPhase
        | StateError::NotResetting
        | StateError::UnsafeDataDirectory(_) => RpcErrorCode::Internal,
    };
    RpcError {
        code,
        message: error.to_string(),
        details: Value::Null,
    }
}

fn invalid_request(error: impl std::fmt::Display) -> Status {
    Status::invalid_argument(error.to_string())
}

fn internal_response(error: impl std::fmt::Display) -> Status {
    Status::internal(error.to_string())
}
