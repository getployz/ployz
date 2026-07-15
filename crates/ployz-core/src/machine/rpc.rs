//! Shared machine-local RPC contracts.

use crate::ids::MachineId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineRpcResponse<T, E> {
    Ok(T),
    DomainError { machine_id: MachineId, error: E },
}

pub trait MachineRpcResponder {
    fn responder_machine_id(&self) -> &MachineId;
}
