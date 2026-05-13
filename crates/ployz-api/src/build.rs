use ployz_model::{
    BuildMethod, BuildOperationRecord, ImageArtifact, ImageAvailabilityRecord, ImagePlatform,
    MachineId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BuildEnvValue {
    Plain {
        value: String,
    },
    Secret {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fingerprint: Option<String>,
    },
}

impl fmt::Debug for BuildEnvValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plain { .. } => f
                .debug_struct("Plain")
                .field("value", &"<redacted>")
                .finish(),
            Self::Secret { fingerprint, .. } => f
                .debug_struct("Secret")
                .field("value", &"<redacted>")
                .field("fingerprint", fingerprint)
                .finish(),
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInputs {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, BuildEnvValue>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub docker_build_args: BTreeMap<String, String>,
}

impl BuildInputs {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.env.is_empty() && self.docker_build_args.is_empty()
    }
}

impl fmt::Debug for BuildInputs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let docker_build_args = self
            .docker_build_args
            .keys()
            .map(|key| (key, "<redacted>"))
            .collect::<Vec<_>>();
        f.debug_struct("BuildInputs")
            .field("env", &self.env)
            .field("docker_build_args", &docker_build_args)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildLocalRequest {
    pub method: BuildMethod,
    pub context_dir: String,
    pub image_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<ImagePlatform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_target: Option<MachineId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub distribute_targets: Vec<MachineId>,
    #[serde(default, skip_serializing_if = "BuildInputs::is_empty")]
    pub inputs: BuildInputs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildMachineRequest {
    pub method: BuildMethod,
    pub context_path: String,
    pub image_name: String,
    pub machine_id: MachineId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<ImagePlatform>,
    #[serde(default, skip_serializing_if = "BuildInputs::is_empty")]
    pub inputs: BuildInputs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildResultPayload {
    pub operation_id: String,
    pub artifact: ImageArtifact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<ImageAvailabilityRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildOperationPayload {
    pub operation: BuildOperationRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildOperationListPayload {
    pub operations: Vec<BuildOperationRecord>,
}
