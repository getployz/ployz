use mvp_bus::Payload;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use mvp_projection::{BackendEndpoint, ServiceName};

use crate::{CapacityReply, DeployError, DeployId, DeployResult, InstanceId, RevisionId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityRequest {
    pub deploy_id: DeployId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceCommandRequest {
    pub deploy_id: DeployId,
    pub instance_id: InstanceId,
    pub service: ServiceName,
    pub revision: RevisionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceCommandReply {
    pub instance_id: InstanceId,
    pub outcome: InstanceStartOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceStartOutcome {
    Ready,
    NotReady { reason: InstanceNotReadyReason },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceNotReadyReason {
    ReadinessCheckFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrainInstanceRequest {
    pub deploy_id: DeployId,
    pub cleanup_target: BackendEndpoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopInstanceRequest {
    pub deploy_id: DeployId,
    pub cleanup_target: BackendEndpoint,
}

pub fn encode<T: Serialize>(value: &T, context: &'static str) -> DeployResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(|source| DeployError::WirePayload { context, source })
}

pub fn decode<T: DeserializeOwned>(payload: &Payload, context: &'static str) -> DeployResult<T> {
    serde_json::from_slice(payload.as_bytes())
        .map_err(|source| DeployError::WirePayload { context, source })
}

pub fn decode_capacity_reply(payload: &Payload) -> DeployResult<CapacityReply> {
    decode(payload, "capacity reply")
}
