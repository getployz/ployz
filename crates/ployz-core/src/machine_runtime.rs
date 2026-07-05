//! Machine-facing domain models.

use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::time::Duration;

use crate::ids::{
    ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId, StepId,
};
use crate::install::{AbsoluteInstallPath, InstallSha256Digest};
use crate::ops::MachineSubstrateVersions;
use crate::state::{MachineLifecycle, MachinePublicIpObservation};

/// How often each machine publishes its own facts. Operation planning gathers
/// fresh facts by RPC; periodic broadcasts feed passive readers.
pub const OBSERVATION_PUBLISH_INTERVAL: Duration = Duration::from_secs(30);

pub const MACHINE_FACTS_PUBLISH_INTERVAL: Duration = OBSERVATION_PUBLISH_INTERVAL;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MachineFactsRole {
    Control,
    Machine,
    Gateway,
    Dns,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineCertificateRef {
    pub artifact_digest: InstallSha256Digest,
    pub machine_path: AbsoluteInstallPath,
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
    public_ip: Option<MachinePublicIpObservation>,
    roles: Vec<MachineFactsRole>,
    lifecycle: MachineLifecycle,
    substrate_versions: MachineSubstrateVersions,
    certificates: Vec<MachineCertificateRef>,
    observed_at_unix_ms: u64,
}

impl MachineFactsSnapshot {
    pub fn try_new(
        machine_id: MachineId,
        containers: MachineContainerObservationSnapshot,
        public_ip: Option<MachinePublicIpObservation>,
        roles: Vec<MachineFactsRole>,
        lifecycle: MachineLifecycle,
        substrate_versions: MachineSubstrateVersions,
        certificates: Vec<MachineCertificateRef>,
        observed_at_unix_ms: u64,
    ) -> Result<Self, MachineFactsSnapshotError> {
        if containers.machine_id() != &machine_id {
            return Err(MachineFactsSnapshotError::ContainerMachineMismatch {
                expected: machine_id,
                actual: containers.machine_id().clone(),
            });
        }
        if let Some(public_ip) = &public_ip
            && public_ip.machine_id != machine_id
        {
            return Err(MachineFactsSnapshotError::PublicIpMachineMismatch {
                expected: machine_id,
                actual: public_ip.machine_id.clone(),
            });
        }

        Ok(Self {
            machine_id,
            containers,
            public_ip,
            roles,
            lifecycle,
            substrate_versions,
            certificates,
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
    pub fn public_ip(&self) -> Option<&MachinePublicIpObservation> {
        self.public_ip.as_ref()
    }

    #[must_use]
    pub fn roles(&self) -> &[MachineFactsRole] {
        &self.roles
    }

    #[must_use]
    pub const fn lifecycle(&self) -> MachineLifecycle {
        self.lifecycle
    }

    #[must_use]
    pub const fn substrate_versions(&self) -> &MachineSubstrateVersions {
        &self.substrate_versions
    }

    #[must_use]
    pub fn certificates(&self) -> &[MachineCertificateRef] {
        &self.certificates
    }

    #[must_use]
    pub const fn observed_at_unix_ms(&self) -> u64 {
        self.observed_at_unix_ms
    }
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
        "public ip belongs to machine {}, not facts machine {}",
        actual.as_str(),
        expected.as_str()
    )]
    PublicIpMachineMismatch {
        expected: MachineId,
        actual: MachineId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MachineFactsSnapshotWire {
    machine_id: MachineId,
    containers: MachineContainerObservationSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    public_ip: Option<MachinePublicIpObservation>,
    roles: Vec<MachineFactsRole>,
    lifecycle: MachineLifecycle,
    substrate_versions: MachineSubstrateVersions,
    certificates: Vec<MachineCertificateRef>,
    observed_at_unix_ms: u64,
}

impl TryFrom<MachineFactsSnapshotWire> for MachineFactsSnapshot {
    type Error = MachineFactsSnapshotError;

    fn try_from(value: MachineFactsSnapshotWire) -> Result<Self, Self::Error> {
        Self::try_new(
            value.machine_id,
            value.containers,
            value.public_ip,
            value.roles,
            value.lifecycle,
            value.substrate_versions,
            value.certificates,
            value.observed_at_unix_ms,
        )
    }
}

impl From<MachineFactsSnapshot> for MachineFactsSnapshotWire {
    fn from(value: MachineFactsSnapshot) -> Self {
        Self {
            machine_id: value.machine_id,
            containers: value.containers,
            public_ip: value.public_ip,
            roles: value.roles,
            lifecycle: value.lifecycle,
            substrate_versions: value.substrate_versions,
            certificates: value.certificates,
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
    },
    Exited,
}

impl ContainerRuntimeState {
    #[must_use]
    pub const fn running_unroutable() -> Self {
        Self::Running { ip: None }
    }

    #[must_use]
    pub const fn running_at(ip: IpAddr) -> Self {
        Self::Running { ip: Some(ip) }
    }

    #[must_use]
    pub const fn is_running(&self) -> bool {
        match self {
            Self::Running { .. } => true,
            Self::Exited => false,
        }
    }
}

/// The single record of what a managed container is and where it came from:
/// namespace, service, and namespace revision entry identity, plus the
/// provenance (operation, step) and kind stamped by the operation that
/// created it. Rendered into Docker labels as recovery evidence, reported in
/// machine observations, sent in machine run commands, and compared for
/// cleanup fencing - one struct everywhere, so the copies cannot drift.
///
/// This is persisted in Docker labels and `KV_OBS.containers.*`. Changing this
/// shape intentionally breaks existing clusters unless paired with container
/// cleanup and/or KV cleanup.
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

/// Persisted `KV_OBS.containers.*` entry payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ManagedContainerObservation {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub identity: ManagedContainerIdentity,
    pub state: ContainerRuntimeState,
}

impl ManagedContainerObservation {
    #[must_use]
    pub fn is_running_service(&self) -> bool {
        self.identity.kind == ManagedContainerKind::Service && self.state.is_running()
    }

    #[must_use]
    pub fn running_service_ip(&self) -> Option<IpAddr> {
        if self.identity.kind != ManagedContainerKind::Service {
            return None;
        }

        match &self.state {
            ContainerRuntimeState::Running { ip } => *ip,
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
