//! Machine-local NATS RPC protocol types.

use ployz_core::deploy::{ContainerRuntimeSpec, ImageReference, RegistryCredential, VolumeName};
use ployz_core::ids::{ContainerId, MachineId, NamespaceId, OperationId, StepId};
use ployz_core::image::{IMAGE_MESH_REGISTRY_PORT, ImageRepository, OciDigest};
use ployz_core::install::InstallArtifactVersion;
pub use ployz_core::machine::rpc::{MachineRpcResponder, MachineRpcResponse};
use ployz_core::machine::runtime::{ManagedContainerIdentity, ManagedContainerObservation};
use ployz_core::network::{
    PloyzNativeMeshComponent, WireGuardEbpfPrepareError, WireGuardPublicKey,
};
use ployz_core::operation::{FailureMessage, MachineSubstrateVersions, OperatorHint};
use serde::{Deserialize, Serialize};

mod build;
mod dataplane_status;
mod facts;

pub use build::*;
pub use dataplane_status::*;
pub use facts::*;
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
pub struct MachineContainerInspectRpcRequest {
    pub container_id: ContainerId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineContainerInspectRpcOk {
    pub machine_id: MachineId,
    pub observed_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<ManagedContainerObservation>,
}

impl MachineRpcResponder for MachineContainerInspectRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id, .. } = self;
        machine_id
    }
}

pub type MachineContainerInspectRpcResponse =
    MachineRpcResponse<MachineContainerInspectRpcOk, MachineContainerInspectDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineContainerResolveImageRpcRequest {
    pub reference: ImageReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<RegistryCredential>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineContainerResolveImageRpcOk {
    pub machine_id: MachineId,
    pub digest: OciDigest,
}

impl MachineRpcResponder for MachineContainerResolveImageRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self {
            machine_id,
            digest: _,
        } = self;
        machine_id
    }
}

pub type MachineContainerResolveImageRpcResponse =
    MachineRpcResponse<MachineContainerResolveImageRpcOk, MachineContainerResolveImageDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineContainerResolveImageDomainError {
    ResolveFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineContainerInspectDomainError {
    InspectFailed {
        container_id: ContainerId,
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineContainerRunRpcRequest {
    pub pull: MachineImagePull,
    pub runtime: ContainerRuntimeSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provisioned_volumes: Vec<ployz_core::deploy::VolumeName>,
    /// The identity the machine stamps onto the created container; the
    /// wire shape is identical to the dissolved per-RPC run spec.
    pub container: ManagedContainerIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineImagePull {
    Registry {
        reference: ImageReference,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential: Option<RegistryCredential>,
    },
    MeshSeed {
        seed_host: std::net::Ipv4Addr,
        repository: ImageRepository,
        manifest_digest: OciDigest,
    },
}

impl MachineImagePull {
    #[must_use]
    pub fn reference(&self) -> String {
        match self {
            Self::Registry {
                reference,
                credential: _,
            } => reference.as_str().to_owned(),
            Self::MeshSeed {
                seed_host,
                repository,
                manifest_digest,
            } => format!("{seed_host}:{IMAGE_MESH_REGISTRY_PORT}/{repository}@{manifest_digest}"),
        }
    }
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
#[serde(deny_unknown_fields)]
pub struct MachineContainerRunHookRpcRequest {
    pub pull: MachineImagePull,
    pub runtime: ContainerRuntimeSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provisioned_volumes: Vec<ployz_core::deploy::VolumeName>,
    pub container: ManagedContainerIdentity,
    pub timeout_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineContainerRunHookRpcOk {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub exit_code: i64,
}

impl MachineRpcResponder for MachineContainerRunHookRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id, .. } = self;
        machine_id
    }
}

pub type MachineContainerRunHookRpcResponse =
    MachineRpcResponse<MachineContainerRunHookRpcOk, MachineContainerRunHookDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineContainerRunHookDomainError {
    OperationStepAmbiguous {
        operation_id: OperationId,
        step_id: StepId,
        container_ids: Vec<ContainerId>,
    },
    CreateFailed {
        message: FailureMessage,
    },
    StartFailed {
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
    WaitFailed {
        container_id: ContainerId,
        message: FailureMessage,
        log_hint: OperatorHint,
    },
    TimedOut {
        container_id: ContainerId,
        timeout_millis: u64,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineContainerRunDomainError {
    ImagePullFailed {
        service_id: ployz_core::ids::ServiceId,
        namespace_revision_entry_id: ployz_core::ids::NamespaceRevisionEntryId,
        message: FailureMessage,
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
pub struct MachineVolumeRemoveRpcRequest {
    pub operation_id: OperationId,
    pub namespace_id: NamespaceId,
    pub volume_name: VolumeName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineVolumeRemoveRpcOk {
    pub machine_id: MachineId,
}

impl MachineRpcResponder for MachineVolumeRemoveRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id } = self;
        machine_id
    }
}

pub type MachineVolumeRemoveRpcResponse =
    MachineRpcResponse<MachineVolumeRemoveRpcOk, MachineVolumeRemoveDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineVolumeRemoveDomainError {
    RemoveFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineContainerRestartRpcRequest {
    pub operation_id: OperationId,
    pub container_id: ContainerId,
    pub expected_identity: ManagedContainerIdentity,
}

pub type MachineContainerRestartRpcResponse =
    MachineRpcResponse<MachineContainerRpcOk, MachineContainerRestartDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineContainerRestartDomainError {
    RestartFailed {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_unix_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "omit_false")]
    pub timestamps: bool,
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

fn omit_false(value: &bool) -> bool {
    !*value
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
pub struct MachineStoragePrepareRpcRequest {
    pub operation_id: OperationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<ployz_core::deploy::ZfsPoolName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineStoragePrepareReportRpcRequest {
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineStoragePrepareRpcOk {
    pub machine_id: MachineId,
    pub pool: ployz_core::deploy::ZfsPoolName,
}

impl MachineRpcResponder for MachineStoragePrepareRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        &self.machine_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineStoragePrepareReportRpcOk {
    pub machine_id: MachineId,
    pub pool: Option<ployz_core::deploy::ZfsPoolName>,
}

impl MachineRpcResponder for MachineStoragePrepareReportRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        &self.machine_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineStoragePrepareDomainError {
    PreparationFailed {
        failure: ployz_core::storage::StorageEffectFailure,
    },
}

pub type MachineStoragePrepareRpcResponse =
    MachineRpcResponse<MachineStoragePrepareRpcOk, MachineStoragePrepareDomainError>;
pub type MachineStoragePrepareReportRpcResponse =
    MachineRpcResponse<MachineStoragePrepareReportRpcOk, MachineStoragePrepareDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineDataplanePublicKeyRpcRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineDataplanePublicKeyRpcOk {
    pub machine_id: MachineId,
    pub public_key: WireGuardPublicKey,
}

impl MachineRpcResponder for MachineDataplanePublicKeyRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id, .. } = self;
        machine_id
    }
}

pub type MachineDataplanePublicKeyRpcResponse =
    MachineRpcResponse<MachineDataplanePublicKeyRpcOk, MachineDataplanePublicKeyDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineDataplanePublicKeyDomainError {
    Unavailable {
        component: PloyzNativeMeshComponent,
        message: FailureMessage,
    },
    InvalidReport {
        message: FailureMessage,
    },
}

impl From<WireGuardEbpfPrepareError> for MachineDataplanePublicKeyDomainError {
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
    fn dataplane_public_key_wire_shape_is_pinned() {
        let request = MachineDataplanePublicKeyRpcRequest {};
        let request_json = json!({});

        assert_eq!(
            serde_json::to_value(&request).expect("request serializes"),
            request_json
        );
        assert_eq!(
            serde_json::from_value::<MachineDataplanePublicKeyRpcRequest>(request_json)
                .expect("request deserializes"),
            request
        );
        let public_key = MachineDataplanePublicKeyRpcResponse::Ok(MachineDataplanePublicKeyRpcOk {
            machine_id: machine_id("machine_a"),
            public_key: wireguard_public_key("public-key"),
        });
        let public_key_json = json!({
            "status": "ok",
            "machine_id": "machine_a",
            "public_key": "public-key",
        });
        assert_eq!(
            serde_json::to_value(&public_key).expect("response serializes"),
            public_key_json
        );
        assert_eq!(
            serde_json::from_value::<MachineDataplanePublicKeyRpcResponse>(public_key_json)
                .expect("response deserializes"),
            public_key
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
    fn container_run_hook_wire_shape_is_pinned() {
        let request = MachineContainerRunHookRpcRequest {
            pull: MachineImagePull::Registry {
                credential: None,
                reference: ImageReference::try_new("registry.example/api:rev_2")
                    .expect("valid image"),
            },
            runtime: ContainerRuntimeSpec::image_defaults(),
            provisioned_volumes: Vec::new(),
            container: ManagedContainerIdentity {
                namespace_id: ployz_core::ids::NamespaceId::try_new("default")
                    .expect("valid namespace id"),
                service_id: ployz_core::ids::ServiceId::try_new("svc_api")
                    .expect("valid service id"),
                namespace_revision_entry_id: ployz_core::ids::NamespaceRevisionEntryId::try_new(
                    "entry_2",
                )
                .expect("valid entry id"),
                operation_id: operation_id("op_123"),
                step_id: StepId::try_new("pre_start").expect("valid step id"),
                kind: ployz_core::machine::runtime::ManagedContainerKind::Predeploy,
            },
            timeout_millis: 1_000,
        };
        let request_json = json!({
            "pull": {
                "source": "registry",
                "reference": "registry.example/api:rev_2",
            },
            "runtime": {
                "command": null,
                "entrypoint": null,
                "environment": {},
                "stop_grace_period": 10,
            },
            "container": {
                "namespace_id": "default",
                "service_id": "svc_api",
                "namespace_revision_entry_id": "entry_2",
                "operation_id": "op_123",
                "step_id": "pre_start",
                "kind": "predeploy",
            },
            "timeout_millis": 1000,
        });
        assert_eq!(
            serde_json::to_value(&request).expect("request serializes"),
            request_json
        );
        assert_eq!(
            serde_json::from_value::<MachineContainerRunHookRpcRequest>(request_json)
                .expect("request deserializes"),
            request
        );

        let response = MachineContainerRunHookRpcResponse::Ok(MachineContainerRunHookRpcOk {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_hook"),
            exit_code: 7,
        });
        let response_json = json!({
            "status": "ok",
            "machine_id": "machine_a",
            "container_id": "ctr_hook",
            "exit_code": 7,
        });

        assert_eq!(
            serde_json::to_value(&response).expect("response serializes"),
            response_json
        );
        assert_eq!(
            serde_json::from_value::<MachineContainerRunHookRpcResponse>(response_json)
                .expect("response deserializes"),
            response
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
