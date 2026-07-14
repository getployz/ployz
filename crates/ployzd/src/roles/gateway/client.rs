//! Request-side adapter for fresh gateway-role status testimony.

use std::time::Duration;

use ployz_core::ids::MachineId;
use ployz_core::machine_rpc::{MachineRpcResponder, MachineRpcResponse};
use ployz_core::state::GatewayStatusObservation;
use ployz_core::subjects::MachineServiceEndpoint;
use serde::{Deserialize, Serialize};

use crate::machine_runtime::MachineRuntimeUnavailableReason;
use crate::roles::machine::client::{DEFAULT_MACHINE_RPC_TIMEOUT, MachineCallError, call_machine};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayStatusGetRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayStatusGetOk {
    pub observation: GatewayStatusObservation,
}

impl MachineRpcResponder for GatewayStatusGetOk {
    fn responder_machine_id(&self) -> &MachineId {
        &self.observation.machine_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum GatewayStatusGetDomainError {}

pub type GatewayStatusGetResponse =
    MachineRpcResponse<GatewayStatusGetOk, GatewayStatusGetDomainError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayStatusReadError {
    pub machine_id: MachineId,
    pub reason: MachineRuntimeUnavailableReason,
}

#[derive(Debug, Clone)]
pub struct NatsGatewayStatusReader {
    client: async_nats::Client,
    request_timeout: Duration,
}

impl NatsGatewayStatusReader {
    #[must_use]
    pub fn new(client: async_nats::Client) -> Self {
        Self {
            client,
            request_timeout: DEFAULT_MACHINE_RPC_TIMEOUT,
        }
    }

    #[must_use]
    pub const fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }

    pub async fn read(
        &self,
        machine_id: &MachineId,
    ) -> Result<GatewayStatusObservation, GatewayStatusReadError> {
        call_machine::<GatewayStatusGetOk, GatewayStatusGetDomainError>(
            &self.client,
            self.request_timeout,
            machine_id,
            MachineServiceEndpoint::GatewayStatusGet,
            &GatewayStatusGetRequest {},
        )
        .await
        .map(|response| response.observation)
        .map_err(|error| match error {
            MachineCallError::Unavailable(reason) => GatewayStatusReadError {
                machine_id: machine_id.clone(),
                reason,
            },
            MachineCallError::Domain(error) => match error {},
        })
    }
}
