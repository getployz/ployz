use serde::{Deserialize, Serialize};

use crate::{
    ContainerId, ContainerLimits, DockerVolume, DockerVolumeId, DockerVolumeName,
    ResolvedServiceSpec, RpcError,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListVolumesRequest {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoveVolumeRequest {
    pub name: DockerVolumeName,
    #[serde(default)]
    pub force: bool,
}

/// One Docker Volume whose name is present but whose current detail is unavailable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("Docker Volume {} on Machine {} could not be inspected: {error}; resolve the reported Docker or storage error on that Machine, then inspect the Volume again", id.name, id.machine_id)]
pub struct VolumeObservationFailure {
    pub id: DockerVolumeId,
    pub error: RpcError,
}

/// Partial live inventory for one Machine.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VolumeInventory {
    #[serde(default)]
    pub volumes: Vec<DockerVolume>,
    #[serde(default)]
    pub failures: Vec<VolumeObservationFailure>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct VolumeRemoved {}

/// Ask a Machine to apply `spec`'s CPU and memory limits to the running Service
/// Container `container_id` in place, and record `spec` as its resolved spec. The
/// Machine refuses any other difference between `spec` and the Container's spec.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateContainerLimitsRequest {
    pub container_id: ContainerId,
    pub spec: ResolvedServiceSpec,
}

/// The limits a Machine read back from Docker after an in-place update.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContainerLimitsUpdated {
    pub container_id: ContainerId,
    pub limits: ContainerLimits,
}

/// Why a Machine refused to change a Container's limits in place, before it
/// asked Docker for anything. Carried in `RpcError.details.reason`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitsRefusal {
    /// The spec names another Service than the Container runs.
    OtherService,
    /// The spec equals the Container's; there is nothing to apply.
    UpToDate,
    /// The change needs a new Container.
    NeedsRecreate,
    /// The Container is not running, so there is nothing serving to keep.
    NotRunning,
    /// The Container runs with more memory than the spec asks for.
    WouldLowerMemory,
    /// The Container runs with no memory limit, and the spec would set one.
    MemoryUnlimited,
}

impl LimitsRefusal {
    /// The `reason` of a limits update RPC error, if it is one of the frozen names.
    #[must_use]
    pub fn from_details(details: &serde_json::Value) -> Option<Self> {
        details
            .get("reason")
            .and_then(|reason| Self::deserialize(reason).ok())
    }
}

impl std::fmt::Display for LimitsRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::OtherService => "the spec names another Service",
            Self::UpToDate => "the spec is already current",
            Self::NeedsRecreate => "the change needs a new Container",
            Self::NotRunning => "the Container is not running",
            Self::WouldLowerMemory => "the Container runs with a higher memory limit",
            Self::MemoryUnlimited => "the Container runs without a memory limit",
        })
    }
}

/// What a Machine saw when it answered `NotFound` for a Container.
/// Carried in `RpcError.details.reason`; an answer without it claims nothing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerNotFoundReason {
    /// Docker answered an inspect of this Container ID, and the Machine holds no
    /// Resolved Service Spec under this ID.
    NoResolvedSpec,
}

impl ContainerNotFoundReason {
    /// The `reason` of a Container's `NotFound` RPC error, if it is one of the frozen names.
    #[must_use]
    pub fn from_details(details: &serde_json::Value) -> Option<Self> {
        details
            .get("reason")
            .and_then(|reason| Self::deserialize(reason).ok())
    }
}

/// Request fresh storage commitments and physical backing capacity.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct InspectStorageRequest {}

/// Secure mounted provisioned storage for the complete set of local placements.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PrepareVolumesRequest {
    /// Complete assigned storage requirements, including reused Volumes.
    pub specs: Vec<crate::ServiceStorageSpec>,
}

/// Provisioned Volumes whose datasets and Docker metadata are ready for use.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreparedVolumes {
    /// Durable names ready to be reused by the upcoming Container creations.
    pub names: Vec<DockerVolumeName>,
}
