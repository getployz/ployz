use bollard::{
    Docker,
    errors::Error as DockerError,
    models::{Mount, MountType},
    query_parameters::{
        CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
    },
};
use ployz_core::{
    ContainerCreated, ContainerId, ContainerKind, MachineGateway, MachineId, ResolvedServiceSpec,
};

use crate::docker_image::prepare_image;

use super::{
    Error, LocalDocker, MachineSpecStore, ManagedLabels, create, docker_error,
    spec_store::ConfigOperation,
};

const CONTAINER_NAME_ATTEMPTS: u8 = 4;

impl LocalDocker {
    pub async fn create(
        &self,
        machine_id: &MachineId,
        gateway: MachineGateway,
        specs: &MachineSpecStore,
        kind: ContainerKind,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerCreated, Error> {
        // TODO(UT-030): direct creation does not validate that an existing Service ID still uses
        // the same Service Name; that requires an observer-relative cluster snapshot.
        let mut body = create::container_create_body(machine_id, gateway, kind, spec)?;
        let mounts = body
            .host_config
            .get_or_insert_default()
            .mounts
            .get_or_insert_default();
        preflight_named_volumes(&self.client, mounts).await?;
        prepare_image(
            &self.client,
            &spec.container.image,
            spec.container.pull_policy,
        )
        .await?;
        let mut config_operation = specs.config_operation().await;
        mounts.extend(docker_config_mounts(&mut config_operation, spec).await?);
        let result = async {
            let mut attempt = 0;
            let (created, display_name) = loop {
                attempt += 1;
                let suffix = MachineId::random().as_str()[..4].to_owned();
                let display_name = match kind {
                    ContainerKind::ServiceContainer => format!("{}-{suffix}", spec.name),
                    ContainerKind::PreDeployHook => format!("{}-pre-deploy-{suffix}", spec.name),
                };
                let options = CreateContainerOptionsBuilder::default()
                    .name(&display_name)
                    .build();
                match self
                    .client
                    .create_container(Some(options), body.clone())
                    .await
                {
                    Ok(created) => break (created, display_name),
                    Err(error) if retry_name_conflict(attempt, &error) => {}
                    Err(error) => return Err(error.into()),
                }
            };
            let container_id =
                ContainerId::parse(created.id).map_err(|source| Error::InvalidValue {
                    field: "container ID",
                    source,
                })?;

            if let Err(error) = config_operation.put(&container_id, spec).await {
                self.force_remove_container(&container_id).await?;
                return Err(error.into());
            }
            Ok(ContainerCreated {
                container_id,
                display_name,
            })
        };
        let result = result.await;
        if result.is_err()
            && let Err(error) = config_operation.garbage_collect_configs().await
        {
            eprintln!("failed to reclaim materialized configs: {error}");
        }
        result
    }

    pub async fn start(
        &self,
        specs: &MachineSpecStore,
        container_id: &ContainerId,
    ) -> Result<(), Error> {
        self.ensure_managed(specs, container_id).await?;
        let result = self
            .client
            .start_container(container_id.as_str(), None)
            .await;
        idempotent_lifecycle_result(container_id, result)
    }

    pub async fn stop(
        &self,
        specs: &MachineSpecStore,
        container_id: &ContainerId,
        signal: Option<&str>,
        grace_period_seconds: Option<i32>,
    ) -> Result<(), Error> {
        self.ensure_managed(specs, container_id).await?;
        let mut options = StopContainerOptionsBuilder::default();
        if let Some(signal) = signal {
            options = options.signal(signal);
        }
        if let Some(seconds) = grace_period_seconds {
            options = options.t(seconds);
        }
        let result = self
            .client
            .stop_container(container_id.as_str(), Some(options.build()))
            .await;
        idempotent_lifecycle_result(container_id, result)
    }

    pub async fn remove(
        &self,
        specs: &MachineSpecStore,
        container_id: &ContainerId,
        remove_volumes: bool,
        force: bool,
    ) -> Result<(), Error> {
        let mut config_operation = specs.config_operation().await;
        match self.ensure_managed(specs, container_id).await {
            Ok(()) => {}
            Err(Error::ContainerNotFound(_)) if config_operation.remove(container_id).await? => {
                return Ok(());
            }
            Err(error) => return Err(error),
        }
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
                config_operation.remove(container_id).await?;
                Ok(())
            }
            Err(Error::ContainerNotFound(_)) if config_operation.remove(container_id).await? => {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub async fn remove_all_managed(&self, specs: &MachineSpecStore) -> Result<(), Error> {
        let mut config_operation = specs.config_operation().await;
        for container_id in self.managed_container_ids().await? {
            match self
                .client
                .stop_container(container_id.as_str(), None)
                .await
            {
                Ok(())
                | Err(DockerError::DockerResponseServerError {
                    status_code: 304 | 404,
                    ..
                }) => {}
                Err(error) => return Err(error.into()),
            }
            self.force_remove_container(&container_id).await?;
            config_operation.remove(&container_id).await?;
        }
        Ok(())
    }

    async fn ensure_managed(
        &self,
        specs: &MachineSpecStore,
        container_id: &ContainerId,
    ) -> Result<(), Error> {
        let inspected = self
            .client
            .inspect_container(container_id.as_str(), None)
            .await
            .map_err(|error| docker_error(container_id, error))?;
        let labels = inspected
            .config
            .and_then(|config| config.labels)
            .ok_or(Error::NotManaged)?;
        ManagedLabels::parse(&labels)?;
        specs
            .get(container_id)
            .await?
            .ok_or_else(|| Error::SpecNotFound(container_id.clone()))?;
        Ok(())
    }

    async fn force_remove_container(&self, container_id: &ContainerId) -> Result<(), Error> {
        let options = RemoveContainerOptionsBuilder::default()
            .v(true)
            .force(true)
            .build();
        match self
            .client
            .remove_container(container_id.as_str(), Some(options))
            .await
            .map_err(|error| docker_error(container_id, error))
        {
            Ok(()) | Err(Error::ContainerNotFound(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

async fn preflight_named_volumes(docker: &Docker, mounts: &[Mount]) -> Result<(), Error> {
    // TODO(UT-127): non-local drivers intentionally receive no special handling.
    for mount in mounts
        .iter()
        .filter(|mount| mount.typ == Some(MountType::VOLUME))
    {
        let name = mount.source.as_deref().ok_or_else(|| {
            Error::InvalidContainerConfig("named Volume source is missing".into())
        })?;
        docker.inspect_volume(name).await?;
        // TODO(UT-128): existence is the only mount-time check; do not recheck driver/options.
    }
    Ok(())
}

async fn docker_config_mounts(
    configs: &mut ConfigOperation<'_>,
    spec: &ResolvedServiceSpec,
) -> Result<Vec<Mount>, Error> {
    let mut mounts = Vec::with_capacity(spec.container.config_mounts.len());
    for mount in &spec.container.config_mounts {
        let config = spec
            .configs
            .iter()
            .find(|config| config.name == mount.config_name)
            .ok_or_else(|| Error::ConfigNotFound(mount.config_name.clone()))?;
        let target = mount
            .target
            .as_ref()
            .map_or_else(|| format!("/{}", mount.config_name), ToString::to_string);
        if target == "/" {
            return Err(Error::InvalidContainerConfig(format!(
                "invalid config target {target:?}"
            )));
        }
        let source = configs.materialize_config(config, mount).await?;
        let source = source.to_str().ok_or_else(|| {
            Error::InvalidContainerConfig("config path is not valid UTF-8".into())
        })?;
        mounts.push(Mount {
            typ: Some(MountType::BIND),
            source: Some(source.into()),
            target: Some(target),
            read_only: Some(true),
            ..Default::default()
        });
    }
    Ok(mounts)
}

fn idempotent_lifecycle_result(
    container_id: &ContainerId,
    result: Result<(), bollard::errors::Error>,
) -> Result<(), Error> {
    match result {
        Ok(())
        | Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 304, ..
        }) => Ok(()),
        Err(error) => Err(docker_error(container_id, error)),
    }
}

fn retry_name_conflict(attempt: u8, error: &bollard::errors::Error) -> bool {
    attempt < CONTAINER_NAME_ATTEMPTS
        && matches!(
            error,
            bollard::errors::Error::DockerResponseServerError {
                status_code: 409,
                ..
            }
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_conflicts_retry_with_a_fresh_four_character_suffix() {
        let conflict = bollard::errors::Error::DockerResponseServerError {
            status_code: 409,
            message: "name already in use".into(),
        };
        let server_error = bollard::errors::Error::DockerResponseServerError {
            status_code: 500,
            message: "failed".into(),
        };

        assert!(retry_name_conflict(1, &conflict));
        assert!(!retry_name_conflict(CONTAINER_NAME_ATTEMPTS, &conflict));
        assert!(!retry_name_conflict(1, &server_error));
    }
}
