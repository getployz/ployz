use ployz_core::dataplane::{MachineDataplaneStatus, NetworkStatusMode};
use ployz_core::ids::MachineId;
use ployz_core::ops::FailureMessage;
use serde::{Deserialize, Serialize};

use super::{MachineRpcResponder, MachineRpcResponse};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineDataplaneStatusRpcRequest {
    pub mode: NetworkStatusMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineDataplaneStatusRpcOk {
    pub machine_id: MachineId,
    pub value: MachineDataplaneStatus,
}

impl MachineRpcResponder for MachineDataplaneStatusRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self {
            machine_id,
            value: _,
        } = self;
        machine_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineDataplaneStatusDomainError {
    ReadFailed { message: FailureMessage },
}

pub type MachineDataplaneStatusRpcResponse =
    MachineRpcResponse<MachineDataplaneStatusRpcOk, MachineDataplaneStatusDomainError>;
