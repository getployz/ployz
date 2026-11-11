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
