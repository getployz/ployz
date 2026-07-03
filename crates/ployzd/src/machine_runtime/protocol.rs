//! Machine-local NATS RPC protocol types.


use ployz_core::dataplane::{
    PloyzNativeMeshComponent, PloyzNativeMeshMachineReady, PloyzNativeMeshPrepareRequest,
    WireGuardEbpfEndpointRoute, WireGuardEbpfPrepareError, WireGuardPeer, WireGuardPublicKey,
};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ContainerId, MachineId, OperationId, StepId};
use ployz_core::install::InstallArtifactVersion;
use ployz_core::machine_runtime::ManagedContainerIdentity;
use ployz_core::ops::{FailureMessage, MachineSubstrateVersions, OperatorHint};
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
    /// The identity the machine stamps onto the created container; the
    /// wire shape is identical to the dissolved per-RPC run spec.
    pub container: ManagedContainerIdentity,
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
        expected: ManagedContainerIdentity,
        actual: ManagedContainerIdentity,
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
pub struct MachineSubstrateUpdateRpcRequest {
    pub operation_id: OperationId,
    pub target_version: InstallArtifactVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineSubstrateUpdateRpcOk {
    pub machine_id: MachineId,
}

impl MachineRpcResponder for MachineSubstrateUpdateRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id, .. } = self;
        machine_id
    }
}

pub type MachineSubstrateUpdateRpcResponse =
    MachineRpcResponse<MachineSubstrateUpdateRpcOk, MachineSubstrateUpdateDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineSubstrateUpdateDomainError {
    UpdateFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineSubstrateReportRpcRequest {
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineSubstrateReportRpcOk {
    pub machine_id: MachineId,
    pub reported: MachineSubstrateVersions,
}

impl MachineRpcResponder for MachineSubstrateReportRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id, .. } = self;
        machine_id
    }
}

pub type MachineSubstrateReportRpcResponse =
    MachineRpcResponse<MachineSubstrateReportRpcOk, MachineSubstrateUpdateDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineDataplanePrepareRpcRequest {
    pub operation_id: OperationId,
    pub machines: Vec<MachineId>,
    pub request: MachinePloyzNativeMeshPrepareRpcRequest,
}

impl MachineDataplanePrepareRpcRequest {
    #[must_use]
    pub fn ployz_native_mesh(
        operation_id: OperationId,
        machines: Vec<MachineId>,
        request: MachinePloyzNativeMeshPrepareRpcRequest,
    ) -> Self {
        Self {
            operation_id,
            machines,
            request,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachinePloyzNativeMeshPrepareRpcRequest {
    ReadPublicKey,
    PrepareDataplane {
        endpoint_routes: Vec<WireGuardEbpfEndpointRoute>,
        peers: Vec<WireGuardPeer>,
    },
}

impl From<PloyzNativeMeshPrepareRequest> for MachineDataplanePrepareRpcRequest {
    fn from(value: PloyzNativeMeshPrepareRequest) -> Self {
        let PloyzNativeMeshPrepareRequest {
            operation_id,
            machines,
            endpoint_routes,
            peer_endpoints: _,
            peers,
        } = value;
        Self::ployz_native_mesh(
            operation_id,
            machines,
            MachinePloyzNativeMeshPrepareRpcRequest::PrepareDataplane {
                endpoint_routes,
                peers,
            },
        )
    }
}

pub type MachineDataplanePrepareRpcResponse = MachineRpcResponse<
    MachinePloyzNativeMeshPrepareRpcOk,
    MachinePloyzNativeMeshPrepareDomainError,
>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachinePloyzNativeMeshPrepareRpcOk {
    PublicKey {
        machine_id: MachineId,
        public_key: WireGuardPublicKey,
    },
    Ready {
        readiness: PloyzNativeMeshMachineReady,
    },
}

impl MachinePloyzNativeMeshPrepareRpcOk {
    #[must_use]
    pub fn responder_machine_id(&self) -> &MachineId {
        match self {
            Self::PublicKey { machine_id, .. } => machine_id,
            Self::Ready { readiness } => &readiness.machine_id,
        }
    }
}

impl MachineRpcResponder for MachinePloyzNativeMeshPrepareRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        match self {
            Self::PublicKey { machine_id, .. } => machine_id,
            Self::Ready { readiness } => &readiness.machine_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachinePloyzNativeMeshPrepareDomainError {
    Unavailable {
        component: PloyzNativeMeshComponent,
        message: FailureMessage,
    },
    InvalidReport {
        message: FailureMessage,
    },
}

impl From<WireGuardEbpfPrepareError> for MachinePloyzNativeMeshPrepareDomainError {
    fn from(value: WireGuardEbpfPrepareError) -> Self {
        match value {
            WireGuardEbpfPrepareError::Unavailable {
                component, message, ..
            } => Self::Unavailable { component, message },
            WireGuardEbpfPrepareError::InvalidReport { message } => Self::InvalidReport { message },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::dataplane::{
        EbpfForwardingReady, EbpfForwardingReadyEvidence, PloyzNativeMeshReady, WireGuardReady,
        WireGuardReadyEvidence,
    };
    use serde_json::json;

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    fn operation_id(value: &str) -> OperationId {
        OperationId::try_new(value).expect("valid operation id")
    }

    fn container_id(value: &str) -> ContainerId {
        ContainerId::try_new(value).expect("valid container id")
    }

    fn failure_message(value: &str) -> FailureMessage {
        FailureMessage::try_new(value).expect("valid failure message")
    }

    fn wireguard_public_key(value: &str) -> WireGuardPublicKey {
        WireGuardPublicKey::try_new(value).expect("valid wireguard public key")
    }

    #[test]
    fn dataplane_prepare_request_wire_shape_is_pinned() {
        let request = MachineDataplanePrepareRpcRequest::ployz_native_mesh(
            operation_id("op_123"),
            vec![machine_id("machine_a")],
            MachinePloyzNativeMeshPrepareRpcRequest::PrepareDataplane {
                endpoint_routes: vec![WireGuardEbpfEndpointRoute {
                    machine_id: machine_id("machine_a"),
                    endpoint_subnet: "10.42.1.0/24".to_owned(),
                }],
                peers: vec![WireGuardPeer {
                    machine_id: machine_id("machine_a"),
                    endpoint_subnet: "10.42.1.0/24".to_owned(),
                    public_endpoint: "203.0.113.1:51820".parse().expect("valid socket address"),
                    public_key: wireguard_public_key("public-key"),
                }],
            },
        );
        let request_json = json!({
            "operation_id": "op_123",
            "machines": ["machine_a"],
            "request": {
                "phase": "prepare_dataplane",
                "endpoint_routes": [
                    { "machine_id": "machine_a", "endpoint_subnet": "10.42.1.0/24" },
                ],
                "peers": [
                    {
                        "machine_id": "machine_a",
                        "endpoint_subnet": "10.42.1.0/24",
                        "public_endpoint": "203.0.113.1:51820",
                        "public_key": "public-key",
                    },
                ],
            },
        });

        assert_eq!(
            serde_json::to_value(&request).expect("request serializes"),
            request_json
        );
        assert_eq!(
            serde_json::from_value::<MachineDataplanePrepareRpcRequest>(request_json)
                .expect("request deserializes"),
            request
        );
    }

    #[test]
    fn dataplane_prepare_response_wire_shape_is_pinned() {
        let public_key =
            MachineDataplanePrepareRpcResponse::Ok(MachinePloyzNativeMeshPrepareRpcOk::PublicKey {
                machine_id: machine_id("machine_a"),
                public_key: wireguard_public_key("public-key"),
            });
        let public_key_json = json!({
            "status": "ok",
            "response": "public_key",
            "machine_id": "machine_a",
            "public_key": "public-key",
        });
        assert_eq!(
            serde_json::to_value(&public_key).expect("response serializes"),
            public_key_json
        );
        assert_eq!(
            serde_json::from_value::<MachineDataplanePrepareRpcResponse>(public_key_json)
                .expect("response deserializes"),
            public_key
        );

        let ok =
            MachineDataplanePrepareRpcResponse::Ok(MachinePloyzNativeMeshPrepareRpcOk::Ready {
                readiness: PloyzNativeMeshMachineReady {
                    machine_id: machine_id("machine_a"),
                    ready: PloyzNativeMeshReady {
                        wireguard: WireGuardReady {
                            public_key: wireguard_public_key("public-key"),
                            evidence: vec![WireGuardReadyEvidence::HostPath {
                                path: "/dev/net/tun".to_owned(),
                            }],
                        },
                        ebpf_forwarding: EbpfForwardingReady {
                            evidence: vec![EbpfForwardingReadyEvidence::Command {
                                program: "tc".to_owned(),
                                args: vec!["-V".to_owned()],
                            }],
                        },
                    },
                },
            });
        let ok_json = json!({
            "status": "ok",
            "response": "ready",
            "readiness": {
                "machine_id": "machine_a",
                "wireguard": {
                    "public_key": "public-key",
                    "evidence": [{ "kind": "host_path", "path": "/dev/net/tun" }],
                },
                "ebpf_forwarding": {
                    "evidence": [{ "kind": "command", "program": "tc", "args": ["-V"] }],
                },
            },
        });
        assert_eq!(
            serde_json::to_value(&ok).expect("response serializes"),
            ok_json
        );
        assert_eq!(
            serde_json::from_value::<MachineDataplanePrepareRpcResponse>(ok_json)
                .expect("response deserializes"),
            ok
        );

        let invalid_report = MachineDataplanePrepareRpcResponse::DomainError {
            machine_id: machine_id("machine_a"),
            error: MachinePloyzNativeMeshPrepareDomainError::InvalidReport {
                message: failure_message("invalid report"),
            },
        };
        let invalid_report_json = json!({
            "status": "domain_error",
            "machine_id": "machine_a",
            "error": { "error": "invalid_report", "message": "invalid report" },
        });
        assert_eq!(
            serde_json::to_value(&invalid_report).expect("response serializes"),
            invalid_report_json
        );
        assert_eq!(
            serde_json::from_value::<MachineDataplanePrepareRpcResponse>(invalid_report_json)
                .expect("response deserializes"),
            invalid_report
        );
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
