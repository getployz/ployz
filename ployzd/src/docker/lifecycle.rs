use bollard::query_parameters::{
    CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
    UploadToContainerOptionsBuilder,
};
use ployz_core::{ContainerCreated, ContainerId, ContainerKind, MachineId, ResolvedServiceSpec};

use super::{Error, LocalDocker, MachineSpecStore, create, docker_error};

impl LocalDocker {
    pub async fn create(
        &self,
        machine_id: &MachineId,
        specs: &MachineSpecStore,
        kind: ContainerKind,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerCreated, Error> {
        // TODO(UT-030): direct creation does not validate that an existing Service ID still uses
        // the same Service Name; that requires an observer-relative cluster snapshot.
        let suffix = MachineId::random().as_str()[..4].to_owned();
        let display_name = match kind {
            ContainerKind::ServiceContainer => format!("{}-{suffix}", spec.name),
            ContainerKind::PreDeployHook => format!("{}-pre-deploy-{suffix}", spec.name),
        };
        let body = create::container_create_body(machine_id, kind, spec)?;
        let options = CreateContainerOptionsBuilder::default()
            .name(&display_name)
            .build();
        let created = self.client.create_container(Some(options), body).await?;
        let container_id =
            ContainerId::parse(created.id).map_err(|source| Error::InvalidValue {
                field: "container ID",
                source,
            })?;

        if let Err(error) = self.inject_configs(&container_id, spec).await {
            self.cleanup_created(&container_id).await;
            return Err(error);
        }
        if let Err(error) = specs.put(&container_id, spec).await {
            self.cleanup_created(&container_id).await;
            return Err(error.into());
        }
        Ok(ContainerCreated {
            container_id,
            display_name,
        })
    }

    pub async fn start(&self, container_id: &ContainerId) -> Result<(), Error> {
        self.client
            .start_container(container_id.as_str(), None)
            .await
            .map_err(|error| docker_error(container_id, error))
    }

    pub async fn stop(
        &self,
        container_id: &ContainerId,
        signal: Option<&str>,
        grace_period_seconds: Option<i32>,
    ) -> Result<(), Error> {
        let mut options = StopContainerOptionsBuilder::default();
        if let Some(signal) = signal {
            options = options.signal(signal);
        }
        if let Some(seconds) = grace_period_seconds {
            options = options.t(seconds);
        }
        match self
            .client
            .stop_container(container_id.as_str(), Some(options.build()))
            .await
        {
            Ok(())
            | Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(()),
            Err(error) => Err(docker_error(container_id, error)),
        }
    }

    pub async fn remove(
        &self,
        specs: &MachineSpecStore,
        container_id: &ContainerId,
        remove_volumes: bool,
        force: bool,
    ) -> Result<(), Error> {
        let options = RemoveContainerOptionsBuilder::default()
            .v(remove_volumes)
            .force(force)
            .build();
        match self
            .client
            .remove_container(container_id.as_str(), Some(options))
            .await
            .map_err(|error| docker_error(container_id, error))
        {
            Ok(()) => {
                specs.remove(container_id).await?;
                Ok(())
            }
            Err(Error::ContainerNotFound(_)) if specs.remove(container_id).await? => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn inject_configs(
        &self,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
    ) -> Result<(), Error> {
        if spec.container.config_mounts.is_empty() {
            return Ok(());
        }
        let mut archive = tar::Builder::new(Vec::new());
        for mount in &spec.container.config_mounts {
            let config = spec
                .configs
                .iter()
                .find(|config| config.name == mount.config_name)
                .ok_or_else(|| Error::ConfigNotFound(mount.config_name.clone()))?;
            let target = mount
                .target
                .as_ref()
                .map_or_else(|| format!("/{}", config.name), ToString::to_string);
            let mut header = tar::Header::new_gnu();
            header.set_path(target.trim_start_matches('/'))?;
            header.set_size(config.content.len() as u64);
            header.set_mode(mount.mode.unwrap_or(0o444));
            header.set_uid(mount.uid.unwrap_or(0));
            header.set_gid(mount.gid.unwrap_or(0));
            header.set_cksum();
            archive.append(&header, config.content.as_slice())?;
        }
        let bytes = archive.into_inner()?;
        let options = UploadToContainerOptionsBuilder::default()
            .path("/")
            .copy_uidgid("true")
            .build();
        self.client
            .upload_to_container(
                container_id.as_str(),
                Some(options),
                bollard::body_full(bytes.into()),
            )
            .await?;
        Ok(())
    }

    async fn cleanup_created(&self, container_id: &ContainerId) {
        let options = RemoveContainerOptionsBuilder::default()
            .v(true)
            .force(true)
            .build();
        let _ = self
            .client
            .remove_container(container_id.as_str(), Some(options))
            .await;
    }
}
