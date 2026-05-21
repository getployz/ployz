use mvp_bus::Payload;
use mvp_identity::NodeId;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{MachineRemoveError, MachineRemoveResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepareRemoveRequest {
    pub target_node_id: NodeId,
    pub reason: String,
    pub intent: PrepareRemoveIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrepareRemoveIntent {
    Probe,
    Drain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepareRemoveReply {
    pub target_node_id: NodeId,
    pub outcome: PrepareRemoveOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrepareRemoveOutcome {
    ResponderReady,
    NoNewWorkAndDrained,
    NotDrained { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopRemovedWorkloadsRequest {
    pub target_node_id: NodeId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopRemovedWorkloadsReply {
    pub target_node_id: NodeId,
    pub outcome: StopRemovedWorkloadsOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopRemovedWorkloadsOutcome {
    Stopped,
    Failed { reason: String },
}

pub(crate) fn encode<T: Serialize>(
    value: &T,
    context: &'static str,
) -> MachineRemoveResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(|source| MachineRemoveError::WirePayload { context, source })
}

pub(crate) fn decode<T: DeserializeOwned>(
    payload: &Payload,
    context: &'static str,
) -> MachineRemoveResult<T> {
    serde_json::from_slice(payload.as_bytes())
        .map_err(|source| MachineRemoveError::WirePayload { context, source })
}
