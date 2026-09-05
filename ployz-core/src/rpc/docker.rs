use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    DockerVolume, DockerVolumeId, DockerVolumeName, PROVISIONED_VOLUME_DRIVER, RpcError,
    VolumeSource,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateVolumeRequest {
    pub name: DockerVolumeName,
    pub driver: String,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

impl VolumeSource {
    /// Build the exact Docker creation request for a managed Volume source.
    ///
    /// External Volumes, Bind Mounts, and Tmpfs Mounts have no creation
    /// request because Ployz does not create them as Docker Volumes. Managed
    /// declarations have no creation request until Project scoping establishes ownership.
    #[must_use]
    pub fn to_create_volume_request(&self) -> Option<CreateVolumeRequest> {
        if !self.is_resolved() {
            return None;
        }
        match self.kind() {
            crate::RawVolumeSource::Ordinary {
                name,
                driver,
                labels: _,
            } => Some(CreateVolumeRequest {
                name: name.clone(),
                driver: driver.name().into(),
                options: driver.options().clone(),
                labels: self.creation_labels(),
            }),
            crate::RawVolumeSource::Provisioned {
                name,
                maximum_bytes,
                labels: _,
            } => Some(CreateVolumeRequest {
                name: name.clone(),
                driver: PROVISIONED_VOLUME_DRIVER.into(),
                options: BTreeMap::from([("size".into(), format!("{}b", maximum_bytes.get()))]),
                labels: self.creation_labels(),
            }),
            crate::RawVolumeSource::External { .. }
            | crate::RawVolumeSource::Bind { .. }
            | crate::RawVolumeSource::Tmpfs { .. } => None,
        }
    }
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

/// One Docker Volume whose name is present but whose current detail is unavailable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("Docker Volume observation failed: {error}")]
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

/// Successful Docker mutation followed by its independent verification outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verification", rename_all = "snake_case")]
pub enum CreateVolumeReport {
    Verified { volume: DockerVolume },
    Unverified { id: DockerVolumeId, error: RpcError },
}

impl CreateVolumeReport {
    /// Return the verified observation or the created identity and verification failure.
    ///
    /// # Errors
    ///
    /// Returns the created identity and verification error when creation succeeded but
    /// its follow-up observation failed.
    #[expect(
        clippy::result_large_err,
        reason = "both outcomes are public wire values; boxing would leak allocation into the contract"
    )]
    pub fn into_observation(self) -> Result<DockerVolume, VolumeObservationFailure> {
        match self {
            Self::Verified { volume } => Ok(volume),
            Self::Unverified { id, error } => Err(VolumeObservationFailure { id, error }),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::{MachineId, RpcErrorCode};

    #[test]
    fn unverified_create_returns_named_observation_failure() {
        let id = DockerVolumeId {
            machine_id: MachineId::random(),
            name: DockerVolumeName::parse("data").unwrap(),
        };
        let error = RpcError {
            code: RpcErrorCode::Unavailable,
            message: "inspect failed".into(),
            details: Value::Null,
        };

        assert_eq!(
            CreateVolumeReport::Unverified {
                id: id.clone(),
                error: error.clone(),
            }
            .into_observation(),
            Err(VolumeObservationFailure { id, error })
        );
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct VolumeRemoved {}
