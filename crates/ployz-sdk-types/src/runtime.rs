use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core_types::*;
use crate::machine::MachineSnapshot;
use crate::ops::OperationApiResponse;
use crate::service::ServiceSnapshot;

pub type RuntimeSnapshotResponse =
    OperationApiResponse<RuntimeSnapshotResult, RuntimeSnapshotError>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSnapshotRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSnapshotResult {
    pub snapshot: RuntimeSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSnapshot {
    pub machines: Vec<MachineSnapshot>,
    pub services: Vec<ServiceSnapshot>,
    pub routes: Vec<RouteBindingState>,
    pub containers: Vec<ManagedContainerObservation>,
    pub revisions: Vec<RuntimeServiceRevision>,
    pub releases: Vec<RuntimeServiceRelease>,
    pub instances: Vec<RuntimeServiceInstance>,
    pub projection_sources: RuntimeProjectionSources,
    pub updated_at_unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeServiceRevision {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeServiceRelease {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub routes: Vec<RouteTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeServiceInstance {
    pub namespace_id: NamespaceId,
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub operation_id: OperationId,
    pub step_id: StepId,
    pub state: ContainerRuntimeState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProjectionSources {
    pub intent: RuntimeProjectionSource,
    pub facts: RuntimeProjectionSource,
    pub revisions: RuntimeDerivedCollectionSource,
    pub releases: RuntimeDerivedCollectionSource,
    pub instances: RuntimeDerivedCollectionSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProjectionSource {
    pub read_at_unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDerivedCollectionSource {
    pub status: RuntimeDerivedCollectionStatus,
    pub source_count: usize,
    pub missing_link_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDerivedCollectionStatus {
    Complete,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum RuntimeSnapshotError {
    #[error("runtime snapshot unavailable: {message}")]
    Unavailable { message: String },
}
