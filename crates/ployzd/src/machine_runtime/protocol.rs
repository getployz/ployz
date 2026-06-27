//! Machine-local NATS RPC protocol types.

use crate::docker::labels::{ManagedContainerIdentity, ManagedContainerLabels};
use ployz_core::dataplane::{
    WireGuardEbpfComponent, WireGuardEbpfEndpointRoute, WireGuardEbpfMachineReady,
    WireGuardEbpfPrepareError, WireGuardEbpfPrepareRequest, WireGuardPeer, WireGuardPeerEndpoint,
    WireGuardPublicKey,
};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ContainerId, MachineId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::machine_runtime::ManagedContainerKind;
use ployz_core::ops::{FailureMessage, OperatorHint, RoutePort};
use serde::{Deserialize, Serialize};

/// Shared machine RPC response envelope: every endpoint answers either with its
/// success payload or with `{ machine_id, error }`. The serialized shape is
/// identical to the previous per-endpoint enums.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineRpcResponse<T, E> {
    Ok(T),
    DomainError { machine_id: MachineId, error: E },
}

/// Success payloads carry the responding machine id so the request side can
/// reject answers from the wrong machine.
pub trait MachineRpcResponder {
    fn responder_machine_id(&self) -> &MachineId;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerEndpointRequest {
    pub port: RoutePort,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineContainerRunSpec {
    pub service_id: ServiceId,
    pub revision_id: RevisionId,
    pub operation_id: OperationId,
    pub step_id: StepId,
    pub kind: ManagedContainerKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineRunContainerOutcome {
    Created { container_id: ContainerId },
    ReusedRunning { container_id: ContainerId },
    StartedExisting { container_id: ContainerId },
}

impl MachineRunContainerOutcome {
    #[must_use]
    pub fn container_id(&self) -> &ContainerId {
        match self {
            Self::Created { container_id }
            | Self::ReusedRunning { container_id }
            | Self::StartedExisting { container_id } => container_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineEnsureEndpointNetworkRpcRequest {
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineEnsureEndpointNetworkRpcOk {
    pub machine_id: MachineId,
}

impl MachineRpcResponder for MachineEnsureEndpointNetworkRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id } = self;
        machine_id
    }
}

pub type MachineEnsureEndpointNetworkRpcResponse =
    MachineRpcResponse<MachineEnsureEndpointNetworkRpcOk, MachineEnsureEndpointNetworkDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineEnsureEndpointNetworkDomainError {
    EnsureFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineContainerRunRpcRequest {
    pub image: ImageReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<ContainerEndpointRequest>,
    pub container: MachineContainerRunSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineContainerRunRpcOk {
    pub machine_id: MachineId,
    pub outcome: MachineRunContainerOutcome,
}

impl MachineRpcResponder for MachineContainerRunRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id, .. } = self;
        machine_id
    }
}

pub type MachineContainerRunRpcResponse =
    MachineRpcResponse<MachineContainerRunRpcOk, MachineContainerRunDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineContainerRunDomainError {
    OperationStepConflict {
        container_id: ContainerId,
        expected: ManagedContainerLabels,
        actual: ManagedContainerLabels,
    },
    OperationStepAmbiguous {
        operation_id: OperationId,
        step_id: StepId,
        container_ids: Vec<ContainerId>,
    },
    CreatedContainerStartFailed {
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
    ExistingContainerStartFailed {
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
    OperationStepContainerNotStartable {
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineContainerRemoveRpcRequest {
    pub operation_id: OperationId,
    pub container_id: ContainerId,
    pub expected_identity: ManagedContainerIdentity,
}

/// Shared success payload for container remove/stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineContainerRpcOk {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
}

impl MachineRpcResponder for MachineContainerRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id, .. } = self;
        machine_id
    }
}

pub type MachineContainerRemoveRpcResponse =
    MachineRpcResponse<MachineContainerRpcOk, MachineContainerRemoveDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineContainerRemoveDomainError {
    RemoveFailed {
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineContainerStopRpcRequest {
    pub operation_id: OperationId,
    pub container_id: ContainerId,
    pub expected_identity: ManagedContainerIdentity,
}

pub type MachineContainerStopRpcResponse =
    MachineRpcResponse<MachineContainerRpcOk, MachineContainerStopDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineContainerStopDomainError {
    StopFailed {
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineLogsTailRpcRequest {
    pub container_id: ContainerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_lines: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineLogsTailResult {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineLogsTailRpcOk {
    pub value: MachineLogsTailResult,
}

impl MachineRpcResponder for MachineLogsTailRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { value } = self;
        &value.machine_id
    }
}

pub type MachineLogsTailRpcResponse =
    MachineRpcResponse<MachineLogsTailRpcOk, MachineLogsTailDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineLogsTailDomainError {
    NotFound {
        container_id: ContainerId,
    },
    ReadFailed {
        container_id: ContainerId,
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineWireGuardEbpfPrepareRpcRequest {
    pub phase: MachineWireGuardEbpfPreparePhase,
    pub operation_id: OperationId,
    pub machines: Vec<MachineId>,
    pub endpoint_routes: Vec<WireGuardEbpfEndpointRoute>,
    pub peer_endpoints: Vec<WireGuardPeerEndpoint>,
    pub peers: Vec<WireGuardPeer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineWireGuardEbpfPreparePhase {
    ReadPublicKey,
    PrepareDataplane,
}

impl From<WireGuardEbpfPrepareRequest> for MachineWireGuardEbpfPrepareRpcRequest {
    fn from(value: WireGuardEbpfPrepareRequest) -> Self {
        Self {
            phase: MachineWireGuardEbpfPreparePhase::PrepareDataplane,
            operation_id: value.operation_id,
            machines: value.machines,
            endpoint_routes: value.endpoint_routes,
            peer_endpoints: value.peer_endpoints,
            peers: value.peers,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineWireGuardEbpfPrepareRpcResponse {
    Ok {
        readiness: WireGuardEbpfMachineReady,
    },
    PublicKey {
        machine_id: MachineId,
        public_key: WireGuardPublicKey,
    },
    DomainError {
        machine_id: MachineId,
        error: MachineWireGuardEbpfPrepareDomainError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineWireGuardEbpfPrepareDomainError {
    Unavailable {
        component: WireGuardEbpfComponent,
        message: FailureMessage,
    },
}

impl From<WireGuardEbpfPrepareError> for MachineWireGuardEbpfPrepareDomainError {
    fn from(value: WireGuardEbpfPrepareError) -> Self {
        match value {
            WireGuardEbpfPrepareError::Unavailable {
                component, message, ..
            } => Self::Unavailable { component, message },
            WireGuardEbpfPrepareError::InvalidReport { message } => Self::Unavailable {
                component: WireGuardEbpfComponent::WireGuard,
                message,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    fn container_id(value: &str) -> ContainerId {
        ContainerId::try_new(value).expect("valid container id")
    }

    fn failure_message(value: &str) -> FailureMessage {
        FailureMessage::try_new(value).expect("valid failure message")
    }

    #[test]
    fn ensure_endpoint_network_response_wire_shape_is_pinned() {
        let ok = MachineEnsureEndpointNetworkRpcResponse::Ok(MachineEnsureEndpointNetworkRpcOk {
            machine_id: machine_id("machine_a"),
        });
        let ok_json = json!({ "status": "ok", "machine_id": "machine_a" });
        assert_eq!(
            serde_json::to_value(&ok).expect("response serializes"),
            ok_json
        );
        assert_eq!(
            serde_json::from_value::<MachineEnsureEndpointNetworkRpcResponse>(ok_json)
                .expect("response deserializes"),
            ok
        );

        let domain_error = MachineEnsureEndpointNetworkRpcResponse::DomainError {
            machine_id: machine_id("machine_a"),
            error: MachineEnsureEndpointNetworkDomainError::EnsureFailed {
                message: failure_message("ensure failed"),
            },
        };
        let domain_error_json = json!({
            "status": "domain_error",
            "machine_id": "machine_a",
            "error": { "error": "ensure_failed", "message": "ensure failed" },
        });
        assert_eq!(
            serde_json::to_value(&domain_error).expect("response serializes"),
            domain_error_json
        );
        assert_eq!(
            serde_json::from_value::<MachineEnsureEndpointNetworkRpcResponse>(domain_error_json)
                .expect("response deserializes"),
            domain_error
        );
    }

    #[test]
    fn container_run_response_wire_shape_is_pinned() {
        let ok = MachineContainerRunRpcResponse::Ok(MachineContainerRunRpcOk {
            machine_id: machine_id("machine_a"),
            outcome: MachineRunContainerOutcome::Created {
                container_id: container_id("ctr_123"),
            },
        });
        let ok_json = json!({
            "status": "ok",
            "machine_id": "machine_a",
            "outcome": { "outcome": "created", "container_id": "ctr_123" },
        });
        assert_eq!(
            serde_json::to_value(&ok).expect("response serializes"),
            ok_json
        );
        assert_eq!(
            serde_json::from_value::<MachineContainerRunRpcResponse>(ok_json)
                .expect("response deserializes"),
            ok
        );
    }

    #[test]
    fn container_remove_and_stop_response_wire_shapes_are_pinned() {
        let removed = MachineContainerRemoveRpcResponse::Ok(MachineContainerRpcOk {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_old"),
        });
        let removed_json = json!({
            "status": "ok",
            "machine_id": "machine_a",
            "container_id": "ctr_old",
        });
        assert_eq!(
            serde_json::to_value(&removed).expect("response serializes"),
            removed_json
        );
        assert_eq!(
            serde_json::from_value::<MachineContainerRemoveRpcResponse>(removed_json)
                .expect("response deserializes"),
            removed
        );

        let stop_failed = MachineContainerStopRpcResponse::DomainError {
            machine_id: machine_id("machine_a"),
            error: MachineContainerStopDomainError::StopFailed {
                container_id: container_id("ctr_old"),
                message: failure_message("container stop failed: busy"),
                inspect_hint: OperatorHint::try_new("ployz container inspect ctr_old")
                    .expect("valid hint"),
            },
        };
        let stop_failed_json = json!({
            "status": "domain_error",
            "machine_id": "machine_a",
            "error": {
                "error": "stop_failed",
                "container_id": "ctr_old",
                "message": "container stop failed: busy",
                "inspect_hint": "ployz container inspect ctr_old",
            },
        });
        assert_eq!(
            serde_json::to_value(&stop_failed).expect("response serializes"),
            stop_failed_json
        );
        assert_eq!(
            serde_json::from_value::<MachineContainerStopRpcResponse>(stop_failed_json)
                .expect("response deserializes"),
            stop_failed
        );
    }

    #[test]
    fn logs_tail_response_wire_shape_is_pinned() {
        let ok = MachineLogsTailRpcResponse::Ok(MachineLogsTailRpcOk {
            value: MachineLogsTailResult {
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_failed"),
                text: "panic\n".to_owned(),
                truncated: false,
            },
        });
        let ok_json = json!({
            "status": "ok",
            "value": {
                "machine_id": "machine_a",
                "container_id": "ctr_failed",
                "text": "panic\n",
                "truncated": false,
            },
        });
        assert_eq!(
            serde_json::to_value(&ok).expect("response serializes"),
            ok_json
        );
        assert_eq!(
            serde_json::from_value::<MachineLogsTailRpcResponse>(ok_json)
                .expect("response deserializes"),
            ok
        );
    }
}
