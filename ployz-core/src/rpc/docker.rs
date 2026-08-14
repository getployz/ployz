use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{ContainerId, DockerVolume, DockerVolumeName, ResolvedServiceSpec};

pub const CREATE_VOLUME_CAPABILITY: &str = "ployz.volume.create.v1";
pub const LIST_VOLUMES_CAPABILITY: &str = "ployz.volume.list.v1";
pub const INSPECT_VOLUME_CAPABILITY: &str = "ployz.volume.inspect.v1";
pub const REMOVE_VOLUME_CAPABILITY: &str = "ployz.volume.remove.v1";
pub const CREATE_SERVICE_CONTAINER_CAPABILITY: &str = "ployz.container.create.v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateVolumeRequest {
    pub name: DockerVolumeName,
    pub driver: String,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListVolumesRequest {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InspectVolumeRequest {
    pub name: DockerVolumeName,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoveVolumeRequest {
    pub name: DockerVolumeName,
    #[serde(default)]
    pub force: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateServiceContainerRequest {
    pub spec: ResolvedServiceSpec,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VolumeCreated {
    pub volume: DockerVolume,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VolumeList {
    pub volumes: Vec<DockerVolume>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VolumeDetails {
    pub volume: DockerVolume,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct VolumeRemoved {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceContainerCreated {
    pub container_id: ContainerId,
    pub display_name: String,
}
