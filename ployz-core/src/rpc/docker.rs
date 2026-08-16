use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{DockerVolume, DockerVolumeName};

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
pub struct VolumeList {
    pub volumes: Vec<DockerVolume>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct VolumeRemoved {}
