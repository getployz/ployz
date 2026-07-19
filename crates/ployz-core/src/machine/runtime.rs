//! Machine runtime models and live fact testimony.

use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::time::Duration;

use super::storage::StorageCapability;
use super::testimony::MachineEndpointObservation;
use crate::ids::{
    ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId, StepId,
};
use crate::image::OciPlatform;

/// How often each machine refreshes machine-owned observations. Operation
/// planning gathers fresh facts by RPC.
pub const OBSERVATION_PUBLISH_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineFactsRefreshConfirmation {
    pub machine_id: MachineId,
    pub observed_at_unix_ms: u64,
}

impl From<&MachineFactsSnapshot> for MachineFactsRefreshConfirmation {
    fn from(facts: &MachineFactsSnapshot) -> Self {
        let MachineFactsSnapshot {
            machine_id,
            containers: _,
            endpoints: _,
            disk_space: _,
            storage: _,
            platform: _,
            observed_at_unix_ms,
        } = facts;
        Self {
            machine_id: machine_id.clone(),
            observed_at_unix_ms: *observed_at_unix_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    try_from = "MachineFactsSnapshotWire",
    into = "MachineFactsSnapshotWire"
)]
pub struct MachineFactsSnapshot {
    machine_id: MachineId,
    containers: MachineContainerObservationSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoints: Option<MachineEndpointObservation>,
    disk_space: MachineDiskSpace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    storage: Option<StorageCapability>,
    platform: OciPlatform,
    observed_at_unix_ms: u64,
}

/// Point-of-use machine testimony whose independently observed axes remain
/// available when Docker cannot answer container discovery. Fanout projections
/// continue to use [`MachineFactsSnapshot`], which is always container-complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    try_from = "MachineFactsTestimonyWire",
    into = "MachineFactsTestimonyWire"
)]
pub struct MachineFactsTestimony {
    machine_id: MachineId,
    containers: MachineContainerTestimony,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoints: Option<MachineEndpointObservation>,
    disk_space: MachineDiskSpace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    storage: Option<StorageCapability>,
    platform: OciPlatform,
    observed_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MachineFactsTestimonyWire {
    machine_id: MachineId,
    containers: MachineContainerTestimony,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoints: Option<MachineEndpointObservation>,
    disk_space: MachineDiskSpace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    storage: Option<StorageCapability>,
    platform: OciPlatform,
    observed_at_unix_ms: u64,
}

impl TryFrom<MachineFactsTestimonyWire> for MachineFactsTestimony {
    type Error = MachineFactsSnapshotError;

    fn try_from(value: MachineFactsTestimonyWire) -> Result<Self, Self::Error> {
        Self::try_new(
            value.machine_id,
            value.containers,
            value.endpoints,
            value.disk_space,
            value.storage,
            value.platform,
            value.observed_at_unix_ms,
        )
    }
}

impl From<MachineFactsTestimony> for MachineFactsTestimonyWire {
    fn from(value: MachineFactsTestimony) -> Self {
        Self {
            machine_id: value.machine_id,
            containers: value.containers,
            endpoints: value.endpoints,
            disk_space: value.disk_space,
            storage: value.storage,
            platform: value.platform,
            observed_at_unix_ms: value.observed_at_unix_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineContainerTestimony {
    Answered {
        snapshot: MachineContainerObservationSnapshot,
    },
    Unavailable {
        reason: MachineContainerUnavailableReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MachineContainerUnavailableReason {
    DockerUnavailable,
}

impl MachineFactsTestimony {
    pub fn try_new(
        machine_id: MachineId,
        containers: MachineContainerTestimony,
        endpoints: Option<MachineEndpointObservation>,
        disk_space: MachineDiskSpace,
        storage: Option<StorageCapability>,
        platform: OciPlatform,
        observed_at_unix_ms: u64,
    ) -> Result<Self, MachineFactsSnapshotError> {
        if let MachineContainerTestimony::Answered { snapshot } = &containers
            && snapshot.machine_id() != &machine_id
        {
            return Err(MachineFactsSnapshotError::ContainerMachineMismatch {
                expected: machine_id,
                actual: snapshot.machine_id().clone(),
            });
        }
        validate_machine_endpoints(&machine_id, endpoints.as_ref())?;
        Ok(Self {
            machine_id,
            containers,
            endpoints,
            disk_space,
            storage,
            platform,
            observed_at_unix_ms,
        })
    }

    #[must_use]
    pub fn machine_id(&self) -> &MachineId {
        &self.machine_id
    }

    #[must_use]
    pub const fn containers(&self) -> &MachineContainerTestimony {
        &self.containers
    }

    #[must_use]
    pub fn endpoints(&self) -> Option<&MachineEndpointObservation> {
        self.endpoints.as_ref()
    }

    #[must_use]
    pub const fn disk_space(&self) -> MachineDiskSpace {
        self.disk_space
    }

    #[must_use]
    pub const fn storage(&self) -> Option<&StorageCapability> {
        self.storage.as_ref()
    }

    #[must_use]
    pub const fn platform(&self) -> &OciPlatform {
        &self.platform
    }

    #[must_use]
    pub const fn observed_at_unix_ms(&self) -> u64 {
        self.observed_at_unix_ms
    }
}

impl From<MachineFactsSnapshot> for MachineFactsTestimony {
    fn from(facts: MachineFactsSnapshot) -> Self {
        let MachineFactsSnapshotWire {
            machine_id,
            containers,
            endpoints,
            disk_space,
            storage,
            platform,
            observed_at_unix_ms,
        } = facts.into();
        Self {
            machine_id,
            containers: MachineContainerTestimony::Answered {
                snapshot: containers,
            },
            endpoints,
            disk_space,
            storage,
            platform,
            observed_at_unix_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineFactsCompletionError {
    #[error("container testimony unavailable: {reason:?}")]
    ContainersUnavailable {
        reason: MachineContainerUnavailableReason,
    },
    #[error("invalid complete machine facts: {0}")]
    Invalid(MachineFactsSnapshotError),
}

impl TryFrom<MachineFactsTestimony> for MachineFactsSnapshot {
    type Error = MachineFactsCompletionError;

    fn try_from(testimony: MachineFactsTestimony) -> Result<Self, Self::Error> {
        let containers = match testimony.containers {
            MachineContainerTestimony::Answered { snapshot } => snapshot,
            MachineContainerTestimony::Unavailable { reason } => {
                return Err(MachineFactsCompletionError::ContainersUnavailable { reason });
            }
        };
        Self::try_new(
            testimony.machine_id,
            containers,
            testimony.endpoints,
            testimony.disk_space,
            testimony.storage,
            testimony.platform,
            testimony.observed_at_unix_ms,
        )
        .map_err(MachineFactsCompletionError::Invalid)
    }
}

impl MachineFactsSnapshot {
    pub fn try_new(
        machine_id: MachineId,
        containers: MachineContainerObservationSnapshot,
        endpoints: Option<MachineEndpointObservation>,
        disk_space: MachineDiskSpace,
        storage: Option<StorageCapability>,
        platform: OciPlatform,
        observed_at_unix_ms: u64,
    ) -> Result<Self, MachineFactsSnapshotError> {
        if containers.machine_id() != &machine_id {
            return Err(MachineFactsSnapshotError::ContainerMachineMismatch {
                expected: machine_id,
                actual: containers.machine_id().clone(),
            });
        }
        validate_machine_endpoints(&machine_id, endpoints.as_ref())?;

        Ok(Self {
            machine_id,
            containers,
            endpoints,
            disk_space,
            storage,
            platform,
            observed_at_unix_ms,
        })
    }

    #[must_use]
    pub fn machine_id(&self) -> &MachineId {
        &self.machine_id
    }

    #[must_use]
    pub fn containers(&self) -> &MachineContainerObservationSnapshot {
        &self.containers
    }

    #[must_use]
    pub fn endpoints(&self) -> Option<&MachineEndpointObservation> {
        self.endpoints.as_ref()
    }

    #[must_use]
    pub const fn disk_space(&self) -> MachineDiskSpace {
        self.disk_space
    }

    #[must_use]
    pub const fn storage(&self) -> Option<&StorageCapability> {
        self.storage.as_ref()
    }

    #[must_use]
    pub const fn platform(&self) -> &OciPlatform {
        &self.platform
    }

    #[must_use]
    pub const fn observed_at_unix_ms(&self) -> u64 {
        self.observed_at_unix_ms
    }

    pub fn with_container_replaced(
        &self,
        observation: ManagedContainerObservation,
        observed_at_unix_ms: u64,
    ) -> Result<Self, MachineFactsSnapshotError> {
        Self::try_new(
            self.machine_id.clone(),
            self.containers
                .with_container_replaced(observation)
                .map_err(MachineFactsSnapshotError::BuildContainers)?,
            self.endpoints.clone(),
            self.disk_space,
            self.storage.clone(),
            self.platform.clone(),
            observed_at_unix_ms,
        )
    }

    pub fn with_container_removed(
        &self,
        container_id: &ContainerId,
        observed_at_unix_ms: u64,
    ) -> Result<Self, MachineFactsSnapshotError> {
        Self::try_new(
            self.machine_id.clone(),
            self.containers
                .with_container_removed(container_id)
                .map_err(MachineFactsSnapshotError::BuildContainers)?,
            self.endpoints.clone(),
            self.disk_space,
            self.storage.clone(),
            self.platform.clone(),
            observed_at_unix_ms,
        )
    }
}

fn validate_machine_endpoints(
    machine_id: &MachineId,
    endpoints: Option<&MachineEndpointObservation>,
) -> Result<(), MachineFactsSnapshotError> {
    if let Some(endpoints) = endpoints
        && endpoints.machine_id != *machine_id
    {
        return Err(MachineFactsSnapshotError::EndpointMachineMismatch {
            expected: machine_id.clone(),
            actual: endpoints.machine_id.clone(),
        });
    }
    if let Some(endpoints) = endpoints
        && endpoints.control_endpoints.is_empty()
        && endpoints.mesh_endpoints.is_empty()
    {
        return Err(MachineFactsSnapshotError::EmptyEndpoints);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineDiskSpace {
    pub available_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineFactsSnapshotError {
    #[error(
        "container snapshot belongs to machine {}, not facts machine {}",
        actual.as_str(),
        expected.as_str()
    )]
    ContainerMachineMismatch {
        expected: MachineId,
        actual: MachineId,
    },
    #[error(
        "endpoints belong to machine {}, not facts machine {}",
        actual.as_str(),
        expected.as_str()
    )]
    EndpointMachineMismatch {
        expected: MachineId,
        actual: MachineId,
    },
    #[error("endpoint observation is empty")]
    EmptyEndpoints,
    #[error("failed to build container snapshot: {0}")]
    BuildContainers(MachineContainerObservationSnapshotError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "delta", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineContainerFactDelta {
    ContainerObserved {
        observed_at_unix_ms: u64,
        observation: Box<ManagedContainerObservation>,
    },
    ContainerRemoved {
        machine_id: MachineId,
        container_id: ContainerId,
        observed_at_unix_ms: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MachineFactsSnapshotWire {
    machine_id: MachineId,
    containers: MachineContainerObservationSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoints: Option<MachineEndpointObservation>,
    disk_space: MachineDiskSpace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    storage: Option<StorageCapability>,
    platform: OciPlatform,
    observed_at_unix_ms: u64,
}

impl TryFrom<MachineFactsSnapshotWire> for MachineFactsSnapshot {
    type Error = MachineFactsSnapshotError;

    fn try_from(value: MachineFactsSnapshotWire) -> Result<Self, Self::Error> {
        Self::try_new(
            value.machine_id,
            value.containers,
            value.endpoints,
            value.disk_space,
            value.storage,
            value.platform,
            value.observed_at_unix_ms,
        )
    }
}

impl From<MachineFactsSnapshot> for MachineFactsSnapshotWire {
    fn from(value: MachineFactsSnapshot) -> Self {
        Self {
            machine_id: value.machine_id,
            containers: value.containers,
            endpoints: value.endpoints,
            disk_space: value.disk_space,
            storage: value.storage,
            platform: value.platform,
            observed_at_unix_ms: value.observed_at_unix_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ManagedContainerKind {
    Service,
    Predeploy,
    Job,
}

impl ManagedContainerKind {
    #[must_use]
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Predeploy => "predeploy",
            Self::Job => "job",
        }
    }

    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "service" => Some(Self::Service),
            "predeploy" => Some(Self::Predeploy),
            "job" => Some(Self::Job),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContainerRuntimeState {
    Running {
        /// Endpoint-network address of the running container. The routed
        /// port is route state, not container state (ADR 0023), so the
        /// observation carries only the IP gateways dial.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ip: Option<IpAddr>,
        #[serde(default)]
        health: ContainerHealth,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at_unix_ms: Option<u64>,
    },
    Exited,
}

impl ContainerRuntimeState {
    #[must_use]
    pub const fn running_unroutable() -> Self {
        Self::Running {
            ip: None,
            health: ContainerHealth::None,
            started_at_unix_ms: None,
        }
    }

    #[must_use]
    pub const fn running_at(ip: IpAddr) -> Self {
        Self::Running {
            ip: Some(ip),
            health: ContainerHealth::None,
            started_at_unix_ms: None,
        }
    }

    #[must_use]
    pub const fn running_at_with_health(ip: Option<IpAddr>, health: ContainerHealth) -> Self {
        Self::Running {
            ip,
            health,
            started_at_unix_ms: None,
        }
    }

    #[must_use]
    pub const fn is_running(&self) -> bool {
        match self {
            Self::Running { .. } => true,
            Self::Exited => false,
        }
    }

    #[must_use]
    pub const fn health(&self) -> ContainerHealth {
        match self {
            Self::Running { health, .. } => *health,
            Self::Exited => ContainerHealth::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ContainerHealth {
    #[default]
    None,
    Starting,
    Healthy,
    Unhealthy,
}

/// The single record of what a managed container is and where it came from:
/// namespace, service, and namespace revision entry identity, plus the
/// provenance (operation, step) and kind stamped by the operation that
/// created it. Rendered into Docker labels as recovery evidence, reported in
/// machine observations, sent in machine run commands, and compared for
/// cleanup fencing - one struct everywhere, so the copies cannot drift.
///
/// This is persisted in Docker labels and reported in machine facts. Changing
/// this shape intentionally breaks existing clusters unless paired with
/// container cleanup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ManagedContainerIdentity {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub operation_id: OperationId,
    pub step_id: StepId,
    pub kind: ManagedContainerKind,
}

impl ManagedContainerIdentity {
    /// Whether this identity is a service container instance of the given
    /// namespace revision entry - the subset the deploy planner and gateway
    /// match on; provenance never participates in entry matching.
    #[must_use]
    pub fn is_service_entry(
        &self,
        namespace_id: &NamespaceId,
        service_id: &ServiceId,
        namespace_revision_entry_id: &NamespaceRevisionEntryId,
    ) -> bool {
        self.kind == ManagedContainerKind::Service
            && self.namespace_id == *namespace_id
            && self.service_id == *service_id
            && self.namespace_revision_entry_id == *namespace_revision_entry_id
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ManagedContainerHealthStatus {
    Starting,
    Healthy,
    Unhealthy,
}

/// Machine-owned container fact payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ManagedContainerObservation {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub identity: ManagedContainerIdentity,
    pub state: ContainerRuntimeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_status: Option<ManagedContainerHealthStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_image_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_unix_seconds: Option<i64>,
}

impl ManagedContainerObservation {
    #[must_use]
    pub fn is_service(&self) -> bool {
        self.identity.kind == ManagedContainerKind::Service
    }

    #[must_use]
    pub fn is_running_service(&self) -> bool {
        self.is_service() && self.state.is_running()
    }

    #[must_use]
    pub fn running_service_ip(&self) -> Option<IpAddr> {
        if self.identity.kind != ManagedContainerKind::Service {
            return None;
        }

        match &self.state {
            ContainerRuntimeState::Running { ip, .. } => *ip,
            ContainerRuntimeState::Exited => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    try_from = "MachineContainerObservationSnapshotWire",
    into = "MachineContainerObservationSnapshotWire"
)]
pub struct MachineContainerObservationSnapshot {
    machine_id: MachineId,
    containers: Vec<ManagedContainerObservation>,
}

impl MachineContainerObservationSnapshot {
    pub fn try_new(
        machine_id: MachineId,
        containers: impl IntoIterator<Item = ManagedContainerObservation>,
    ) -> Result<Self, MachineContainerObservationSnapshotError> {
        let containers: Vec<_> = containers.into_iter().collect();
        if let Some(container) = containers
            .iter()
            .find(|container| container.machine_id != machine_id)
        {
            return Err(MachineContainerObservationSnapshotError::MachineMismatch {
                expected: machine_id,
                actual: container.machine_id.clone(),
                container_id: container.container_id.clone(),
            });
        }

        Ok(Self {
            machine_id,
            containers,
        })
    }

    #[must_use]
    pub fn machine_id(&self) -> &MachineId {
        &self.machine_id
    }

    #[must_use]
    pub fn containers(&self) -> &[ManagedContainerObservation] {
        &self.containers
    }

    pub fn with_container_replaced(
        &self,
        observation: ManagedContainerObservation,
    ) -> Result<Self, MachineContainerObservationSnapshotError> {
        if observation.machine_id != self.machine_id {
            return Err(MachineContainerObservationSnapshotError::MachineMismatch {
                expected: self.machine_id.clone(),
                actual: observation.machine_id,
                container_id: observation.container_id,
            });
        }

        let mut containers = self.containers.clone();
        containers.retain(|container| container.container_id != observation.container_id);
        containers.push(observation);

        Self::try_new(self.machine_id.clone(), containers)
    }

    pub fn with_container_removed(
        &self,
        container_id: &ContainerId,
    ) -> Result<Self, MachineContainerObservationSnapshotError> {
        let mut containers = self.containers.clone();
        containers.retain(|container| &container.container_id != container_id);

        Self::try_new(self.machine_id.clone(), containers)
    }

    #[must_use]
    pub fn container(&self, container_id: &ContainerId) -> Option<&ManagedContainerObservation> {
        self.containers
            .iter()
            .find(|container| &container.container_id == container_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineContainerObservationSnapshotError {
    #[error(
        "container {} belongs to machine {}, not snapshot machine {}",
        container_id.as_str(),
        actual.as_str(),
        expected.as_str()
    )]
    MachineMismatch {
        expected: MachineId,
        actual: MachineId,
        container_id: ContainerId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MachineContainerObservationSnapshotWire {
    machine_id: MachineId,
    containers: Vec<ManagedContainerObservation>,
}

impl TryFrom<MachineContainerObservationSnapshotWire> for MachineContainerObservationSnapshot {
    type Error = MachineContainerObservationSnapshotError;

    fn try_from(value: MachineContainerObservationSnapshotWire) -> Result<Self, Self::Error> {
        Self::try_new(value.machine_id, value.containers)
    }
}

impl From<MachineContainerObservationSnapshot> for MachineContainerObservationSnapshotWire {
    fn from(value: MachineContainerObservationSnapshot) -> Self {
        Self {
            machine_id: value.machine_id,
            containers: value.containers,
        }
    }
}
