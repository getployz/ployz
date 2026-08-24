use std::collections::HashMap;

use bollard::{
    models::{Volume, VolumeCreateRequest},
    query_parameters::RemoveVolumeOptionsBuilder,
};
use ployz_core::{
    CreateVolumeRequest, DockerVolume, DockerVolumeId, DockerVolumeName,
    DockerVolumeStorageObservation, MachineId,
};
use serde::Deserialize;
use serde_json::Value;

use super::{ContainerRuntime, Error, some_map};
use crate::VolumePluginStatus;

impl ContainerRuntime {
    pub async fn create_volume(
        &self,
        machine_id: &MachineId,
        request: CreateVolumeRequest,
    ) -> Result<DockerVolume, Error> {
        let volume = decode_volume(
            self.docker
                .client
                .create_volume(VolumeCreateRequest {
                    name: Some(request.name.to_string()),
                    driver: Some(request.driver),
                    driver_opts: some_map(request.options),
                    labels: some_map(request.labels),
                    ..Default::default()
                })
                .await,
        )?;
        docker_volume(machine_id, volume)
    }

    pub async fn list_volumes(&self, machine_id: &MachineId) -> Result<Vec<DockerVolume>, Error> {
        decode_volume_list(
            self.docker
                .client
                .list_volumes(None::<bollard::query_parameters::ListVolumesOptions>)
                .await,
        )?
        .into_iter()
        .map(|volume| docker_volume(machine_id, volume))
        .collect()
    }

    pub(super) async fn named_volumes(
        &self,
        machine_id: &MachineId,
    ) -> Result<Vec<DockerVolume>, Error> {
        decode_volume_list(
            self.docker
                .client
                .list_volumes(None::<bollard::query_parameters::ListVolumesOptions>)
                .await,
        )?
        .into_iter()
        .filter(|volume| !volume.name.is_empty())
        .map(|volume| docker_volume(machine_id, volume))
        .collect()
    }

    pub async fn inspect_volume(
        &self,
        machine_id: &MachineId,
        name: &DockerVolumeName,
    ) -> Result<DockerVolume, Error> {
        let volume = decode_volume(self.docker.client.inspect_volume(name.as_str()).await)?;
        docker_volume(machine_id, volume)
    }

    pub async fn remove_volume(&self, name: &DockerVolumeName, force: bool) -> Result<(), Error> {
        self.docker
            .client
            .remove_volume(
                name.as_str(),
                Some(RemoveVolumeOptionsBuilder::default().force(force).build()),
            )
            .await
            .map_err(Into::into)
    }
}

// Bollard models Volume Status as key-only data, so valid plugin values use raw JSON recovery.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawVolume {
    name: String,
    driver: String,
    #[serde(default)]
    mountpoint: String,
    status: Option<Value>,
    options: Option<HashMap<String, String>>,
    labels: Option<HashMap<String, String>>,
}

impl From<Volume> for RawVolume {
    fn from(volume: Volume) -> Self {
        Self {
            name: volume.name,
            driver: volume.driver,
            mountpoint: volume.mountpoint,
            status: None,
            options: Some(volume.options),
            labels: Some(volume.labels),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawVolumeList {
    volumes: Option<Vec<RawVolume>>,
}

fn decode_volume(result: Result<Volume, bollard::errors::Error>) -> Result<RawVolume, Error> {
    match result {
        Ok(volume) => Ok(volume.into()),
        Err(bollard::errors::Error::JsonDataError { contents, .. }) => {
            Ok(serde_json::from_str(&contents)?)
        }
        Err(error) => Err(error.into()),
    }
}

fn decode_volume_list(
    result: Result<bollard::models::VolumeListResponse, bollard::errors::Error>,
) -> Result<Vec<RawVolume>, Error> {
    match result {
        Ok(response) => Ok(response
            .volumes
            .unwrap_or_default()
            .into_iter()
            .map(Into::into)
            .collect()),
        Err(bollard::errors::Error::JsonDataError { contents, .. }) => {
            Ok(serde_json::from_str::<RawVolumeList>(&contents)?
                .volumes
                .unwrap_or_default())
        }
        Err(error) => Err(error.into()),
    }
}

fn docker_volume(machine_id: &MachineId, volume: RawVolume) -> Result<DockerVolume, Error> {
    let storage = if volume.driver == "ployz" {
        let status = volume
            .status
            .ok_or_else(|| Error::InvalidVolumeStatus("Provisioned Volume status is missing"))?;
        let status = serde_json::from_value::<VolumePluginStatus>(status)
            .map_err(|_| Error::InvalidVolumeStatus("Provisioned Volume status is malformed"))?;
        let bound_bytes = std::num::NonZeroU64::new(status.bound_bytes).ok_or_else(|| {
            Error::InvalidVolumeStatus("Provisioned Volume status has no positive bound_bytes")
        })?;
        let mountpoint = ployz_core::MachinePath::parse(volume.mountpoint).map_err(|_| {
            Error::InvalidVolumeStatus("Provisioned Volume status has an invalid mountpoint")
        })?;
        DockerVolumeStorageObservation::Provisioned {
            mountpoint,
            bound_bytes,
            used_bytes: status.used_bytes,
        }
    } else {
        DockerVolumeStorageObservation::Plain {
            driver: volume.driver,
        }
    };
    Ok(DockerVolume {
        id: DockerVolumeId {
            machine_id: *machine_id,
            name: DockerVolumeName::parse(volume.name).map_err(|source| Error::InvalidValue {
                field: "Docker Volume name",
                source,
            })?,
        },
        options: volume.options.unwrap_or_default().into_iter().collect(),
        labels: volume.labels.unwrap_or_default().into_iter().collect(),
        storage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_volume_preserves_provisioned_usage_at_alert_threshold() {
        let contents = serde_json::json!({"Volumes":[{
            "Name":"data",
            "Driver":"ployz",
            "Mountpoint":"/var/lib/ployz-volumes/data",
            "Status":{"bound_bytes":1073741824,"used_bytes":966367642},
            "Options":{"size":"1g"}
        }]})
        .to_string();
        let mut volumes = decode_volume_list(Err(bollard::errors::Error::JsonDataError {
            message: "generated Volume status cannot represent numeric values".into(),
            contents,
            column: 0,
        }))
        .unwrap();
        let observed = docker_volume(&MachineId::random(), volumes.remove(0)).unwrap();

        assert_eq!(
            observed.storage,
            DockerVolumeStorageObservation::Provisioned {
                mountpoint: ployz_core::MachinePath::parse("/var/lib/ployz-volumes/data").unwrap(),
                bound_bytes: std::num::NonZeroU64::new(1_073_741_824).unwrap(),
                used_bytes: 966_367_642,
            }
        );
    }

    #[test]
    fn provisioned_volume_without_complete_plugin_evidence_is_an_error() {
        let error = docker_volume(
            &MachineId::random(),
            serde_json::from_value(serde_json::json!({
                "Name":"data",
                "Driver":"ployz",
                "Mountpoint":"/var/lib/ployz-volumes/data"
            }))
            .unwrap(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("Provisioned Volume status"));
    }

    #[test]
    fn provisioned_volume_rejects_a_relative_mountpoint() {
        let error = docker_volume(
            &MachineId::random(),
            serde_json::from_value(serde_json::json!({
                "Name":"data",
                "Driver":"ployz",
                "Mountpoint":"relative/data",
                "Status":{"bound_bytes":1073741824,"used_bytes":0}
            }))
            .unwrap(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("invalid mountpoint"));
    }
}
