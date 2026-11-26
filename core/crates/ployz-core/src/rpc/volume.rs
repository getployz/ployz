//! Docker Volume DTOs: inventory, removal, storage inspection and preparation.

use serde::{Deserialize, Serialize};

use super::error::RpcError;
use crate::{DockerVolume, DockerVolumeId, DockerVolumeName};

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

/// List this Machine's Kept Volumes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListKeptVolumesRequest {}

/// This Machine's Kept Volumes that Docker still has, oldest first.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeptVolumes {
    pub volumes: Vec<KeptVolume>,
}

/// An anonymous Docker Volume this Machine kept when it removed a Container, with
/// what that Container was.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeptVolume {
    pub name: DockerVolumeName,
    pub namespace: crate::Namespace,
    pub service: crate::ServiceName,
    /// The removed Container, by the ID this Machine gave it.
    pub container: crate::ContainerId,
    /// Where that Container mounted it.
    pub path: crate::ContainerPath,
    /// Unix seconds when this Machine removed the Container and kept the Volume.
    pub kept_at: i64,
    /// Bytes its files take on disk. `None` when this Machine couldn't measure it in time.
    #[serde(default)]
    pub size_bytes: Option<u64>,
}

/// Remove these Kept Volumes. The Machine removes each only while its own record says
/// it kept it and no Container, running or not, mounts it, and never forces Docker.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoveKeptVolumesRequest {
    pub volumes: Vec<DockerVolumeName>,
}

/// One result per requested Volume, in the order asked.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeptVolumesRemoved {
    pub results: Vec<KeptVolumeRemoval>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeptVolumeRemoval {
    pub volume: DockerVolumeName,
    pub outcome: KeptVolumeRemovalOutcome,
}

/// What happened to one requested Volume. Open: a status this build does not know
/// decodes as `unrecognized`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum KeptVolumeRemovalOutcome {
    /// The Volume no longer exists on this Machine.
    Removed,
    /// These Containers, running or not, mount it, so it stays. Docker's IDs, which
    /// may name a Container Ployz didn't make.
    InUse {
        #[serde(default)]
        containers: Vec<crate::ContainerId>,
    },
    /// This Machine has no record of keeping it, so it stays.
    NotKept,
    /// Docker refused for another reason; the Volume may remain.
    Failed { message: String },
    /// A status introduced after this build.
    #[serde(other)]
    Unrecognized,
}
