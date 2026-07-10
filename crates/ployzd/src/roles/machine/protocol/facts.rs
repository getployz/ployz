use ployz_core::ids::MachineId;
use ployz_core::machine_runtime::MachineFactsSnapshot;
use ployz_core::ops::FailureMessage;
use serde::{Deserialize, Serialize};

use super::{MachineRpcResponder, MachineRpcResponse};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineFactsGetRpcRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineFactsGetRpcOk {
    pub facts: MachineFactsSnapshot,
}

impl MachineRpcResponder for MachineFactsGetRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { facts } = self;
        facts.machine_id()
    }
}

pub type MachineFactsGetRpcResponse =
    MachineRpcResponse<MachineFactsGetRpcOk, MachineFactsGetDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineFactsGetDomainError {
    GatherFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineFactsRefreshRpcRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineFactsRefreshRpcOk {
    pub machine_id: MachineId,
    pub observed_at_unix_ms: u64,
}

impl MachineRpcResponder for MachineFactsRefreshRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self {
            machine_id,
            observed_at_unix_ms: _,
        } = self;
        machine_id
    }
}

pub type MachineFactsRefreshRpcResponse =
    MachineRpcResponse<MachineFactsRefreshRpcOk, MachineFactsRefreshDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineFactsRefreshDomainError {
    RefreshFailed { message: FailureMessage },
}
