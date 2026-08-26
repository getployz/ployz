use std::collections::{BTreeMap, HashMap};

use bollard::{
    Docker,
    models::{Volume, VolumeCreateRequest},
    query_parameters::RemoveVolumeOptionsBuilder,
};
use futures_util::{StreamExt, stream};
use ployz_core::{
    CreateVolumeReport, CreateVolumeRequest, DockerVolume, DockerVolumeId, DockerVolumeName,
    DockerVolumeStorageObservation, MachineId, ResolvedServiceSpec, VolumeInventory,
    VolumeObservationFailure, VolumeSource,
};
use serde::Deserialize;
use serde_json::Value;

use super::{ContainerRuntime, Error, some_map};
use crate::VolumePluginStatus;

const VOLUME_INSPECTION_CONCURRENCY: usize = 8;

impl ContainerRuntime {
    /// Ensure every mounted Docker Volume exists and every managed source matches exactly.
    ///
    /// # Errors
    ///
    /// Returns when an external Volume is absent or a managed Volume cannot be created or
    /// verified without mutation.
    pub(super) async fn ensure_mounted_volumes(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
    ) -> Result<(), Error> {
        let mut mounted = BTreeMap::<&DockerVolumeName, &VolumeSource>::new();
        for volume in spec.volume_graph.mounted_volumes() {
            let source = &volume.source;
            let Some(name) = source.docker_volume_name() else {
                continue;
            };
            mounted.entry(name).or_insert(source);
        }
        for source in mounted.values() {
            self.ensure_volume_source(machine_id, source).await?;
        }
        Ok(())
    }

    async fn ensure_volume_source(
        &self,
        machine_id: &MachineId,
        source: &VolumeSource,
    ) -> Result<(), Error> {
        let Some(name) = source.docker_volume_name() else {
            return Ok(());
        };
        if matches!(source, VolumeSource::External { .. }) {
            return match ensure_volume_exists(&self.docker.client, name.as_str()).await {
                Err(error) if volume_not_found(&error) => {
                    Err(Error::ExternalVolumeNotFound(name.clone()))
                }
                result => result,
            };
        }

        match self.inspect_volume(machine_id, name).await {
            Ok(volume) => verify_volume(source, &volume),
            Err(error) if volume_not_found(&error) => {
                let request = source
                    .to_create_volume_request()
                    .expect("managed Docker Volume sources have creation requests");
                match self.create_volume(machine_id, request).await {
                    Ok(CreateVolumeReport::Verified { volume }) => verify_volume(source, &volume),
                    Ok(CreateVolumeReport::Unverified { id, error }) => {
                        Err(Error::VolumeCreatedButUnverified {
                            id,
                            error: Box::new(error),
                        })
                    }
                    Err(error) if volume_conflict(&error) => {
                        verify_volume(source, &self.inspect_volume(machine_id, name).await?)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Create a Docker Volume and verify its observable state when possible.
    ///
    /// # Errors
    ///
    /// Returns an error when Docker rejects the create request. A successful create whose
    /// verification fails is returned as [`CreateVolumeReport::Unverified`].
    pub async fn create_volume(
        &self,
        machine_id: &MachineId,
        request: CreateVolumeRequest,
    ) -> Result<CreateVolumeReport, Error> {
        let docker_name = request.name.to_string();
        let id = DockerVolumeId {
            machine_id: *machine_id,
            name: request.name,
        };
        decode_volume(
            self.docker
                .client
                .create_volume(VolumeCreateRequest {
                    name: Some(docker_name),
                    driver: Some(request.driver),
                    driver_opts: some_map(request.options),
                    labels: some_map(request.labels),
                    ..Default::default()
                })
                .await,
        )?;
        Ok(match self.inspect_volume(machine_id, &id.name).await {
            Ok(volume) => CreateVolumeReport::Verified { volume },
            Err(error) => CreateVolumeReport::Unverified {
                id,
                error: (&error).into(),
            },
        })
    }

    /// List Docker Volumes, retaining individual detail failures alongside healthy observations.
    ///
    /// # Errors
    ///
    /// Returns an error when Docker rejects the collection request or listed inventory is invalid.
    pub async fn list_volumes(&self, machine_id: &MachineId) -> Result<VolumeInventory, Error> {
        let listed = decode_volume_list(
            self.docker
                .client
                .list_volumes(None::<bollard::query_parameters::ListVolumesOptions>)
                .await,
        )?;
        let mut inventory = VolumeInventory::default();
        let mut observations = stream::iter(listed.into_iter().map(|volume| async move {
            let id = docker_volume_id(machine_id, &volume.name)?;
            let observation = if volume.driver == "ployz" {
                self.inspect_volume(machine_id, &id.name).await
            } else {
                docker_volume(machine_id, volume)
            };
            Ok::<_, Error>((id, observation))
        }))
        .buffered(VOLUME_INSPECTION_CONCURRENCY);
        while let Some(observation) = observations.next().await {
            let (id, observation) = observation?;
            match observation {
                Ok(volume) => inventory.volumes.push(volume),
                Err(error) => inventory.failures.push(VolumeObservationFailure {
                    id,
                    error: (&error).into(),
                }),
            }
        }
        Ok(inventory)
    }

    /// Observe one named Docker Volume directly.
    ///
    /// # Errors
    ///
    /// Returns an error when Docker inspection or decoding fails, or when Docker
    /// returns a different Volume name than requested.
    pub async fn inspect_volume(
        &self,
        machine_id: &MachineId,
        name: &DockerVolumeName,
    ) -> Result<DockerVolume, Error> {
        let volume = decode_volume(self.docker.client.inspect_volume(name.as_str()).await)?;
        if volume.name != name.as_str() {
            return Err(Error::UnexpectedVolumeName {
                requested: name.clone(),
                actual: volume.name,
            });
        }
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

fn verify_volume(source: &VolumeSource, observed: &DockerVolume) -> Result<(), Error> {
    let expected = source
        .to_create_volume_request()
        .expect("only managed Docker Volume sources are verified");
    if !source.matches_managed_volume(observed) {
        return Err(Error::VolumeShapeMismatch {
            name: expected.name,
            reason: "requested and observed managed shapes differ".into(),
        });
    }
    Ok(())
}

fn volume_not_found(error: &Error) -> bool {
    docker_status(error, 404)
}

fn volume_conflict(error: &Error) -> bool {
    docker_status(error, 409)
}

fn docker_status(error: &Error, expected: u16) -> bool {
    matches!(
        error,
        Error::Docker(bollard::errors::Error::DockerResponseServerError {
            status_code,
            ..
        }) if *status_code == expected
    )
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

/// Confirm that a named Docker Volume exists and is decodable.
///
/// # Errors
///
/// Returns an error when Docker rejects the inspection or its response cannot be decoded.
pub(super) async fn ensure_volume_exists(docker: &Docker, name: &str) -> Result<(), Error> {
    decode_volume(docker.inspect_volume(name).await).map(drop)
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
        id: docker_volume_id(machine_id, &volume.name)?,
        options: volume.options.unwrap_or_default().into_iter().collect(),
        labels: volume.labels.unwrap_or_default().into_iter().collect(),
        storage,
    })
}

fn docker_volume_id(machine_id: &MachineId, name: &str) -> Result<DockerVolumeId, Error> {
    Ok(DockerVolumeId {
        machine_id: *machine_id,
        name: DockerVolumeName::parse(name).map_err(|source| Error::InvalidValue {
            field: "Docker Volume name",
            source,
        })?,
    })
}

#[cfg(test)]
mod tests;
