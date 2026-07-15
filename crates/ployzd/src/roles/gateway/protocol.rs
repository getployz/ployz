//! Gateway-role status RPC wire contract.

use ployz_core::ids::MachineId;
use ployz_core::machine_rpc::{MachineRpcResponder, MachineRpcResponse};
use ployz_core::state::GatewayStatusObservation;
use serde::{Deserialize, Serialize};

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
