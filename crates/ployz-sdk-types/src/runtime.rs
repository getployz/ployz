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
    #[serde(default)]
    pub public_url: RuntimePublicUrl,
    /// TLS status for custom (bring-your-own) hostnames. Managed auto hostnames
    /// are covered by the wildcard lease cert and are not listed here.
    #[serde(default)]
    pub certificate_statuses: Vec<RouteCertStatus>,
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

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimePublicUrl {
    Auto {
        domain: Option<String>,
    },
    BringYourOwn,
    #[default]
    None,
}

/// TLS lifecycle for a single custom hostname.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RouteCertStatus {
    pub hostname: RouteHostname,
    pub status: RouteCertLifecycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RouteCertLifecycle {
    /// A usable certificate is issued for this hostname.
    Verified,
    /// An ACME challenge is in flight; DNS resolves but the cert is not ready.
    Pending,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_snapshot_defaults_missing_public_url_to_none() {
        let snapshot = serde_json::from_value::<RuntimeSnapshot>(serde_json::json!({
            "machines": [],
            "services": [],
            "routes": [],
            "containers": [],
            "revisions": [],
            "releases": [],
            "instances": [],
            "projection_sources": {
                "intent": { "read_at_unix_seconds": 1 },
                "facts": { "read_at_unix_seconds": 1 },
                "revisions": { "status": "complete", "source_count": 0, "missing_link_count": 0 },
                "releases": { "status": "complete", "source_count": 0, "missing_link_count": 0 },
                "instances": { "status": "complete", "source_count": 0, "missing_link_count": 0 }
            },
            "updated_at_unix_seconds": 1
        }))
        .expect("legacy runtime snapshot decodes");

        assert_eq!(snapshot.public_url, RuntimePublicUrl::None);
    }
}
