use bollard::{
    models::{Volume, VolumeCreateRequest},
    query_parameters::RemoveVolumeOptionsBuilder,
};
use ployz_core::{CreateVolumeRequest, DockerVolume, DockerVolumeId, DockerVolumeName, MachineId};

use super::{ContainerRuntime, Error, some_map};

impl ContainerRuntime {
    pub async fn create_volume(
        &self,
        machine_id: &MachineId,
        request: CreateVolumeRequest,
    ) -> Result<DockerVolume, Error> {
        let volume = self
            .docker
            .client
            .create_volume(VolumeCreateRequest {
                name: Some(request.name.to_string()),
                driver: Some(request.driver),
                driver_opts: some_map(request.options),
                labels: some_map(request.labels),
                ..Default::default()
            })
            .await?;
        docker_volume(machine_id, volume)
    }

    pub async fn list_volumes(&self, machine_id: &MachineId) -> Result<Vec<DockerVolume>, Error> {
        self.docker
            .client
            .list_volumes(None::<bollard::query_parameters::ListVolumesOptions>)
            .await?
            .volumes
            .unwrap_or_default()
            .into_iter()
            .map(|volume| docker_volume(machine_id, volume))
            .collect()
    }

    pub(super) async fn named_volumes(
        &self,
        machine_id: &MachineId,
    ) -> Result<Vec<DockerVolume>, Error> {
        self.docker
            .client
            .list_volumes(None::<bollard::query_parameters::ListVolumesOptions>)
            .await?
            .volumes
            .unwrap_or_default()
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
        let volume = self.docker.client.inspect_volume(name.as_str()).await?;
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

fn docker_volume(machine_id: &MachineId, volume: Volume) -> Result<DockerVolume, Error> {
    Ok(DockerVolume {
        id: DockerVolumeId {
            machine_id: *machine_id,
            name: DockerVolumeName::parse(volume.name).map_err(|source| Error::InvalidValue {
                field: "Docker Volume name",
                source,
            })?,
        },
        driver: volume.driver,
        options: volume.options.into_iter().collect(),
        labels: volume.labels.into_iter().collect(),
    })
}
