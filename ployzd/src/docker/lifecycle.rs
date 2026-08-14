use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder,
    StopContainerOptionsBuilder, UploadToContainerOptionsBuilder,
};
use futures_util::TryStreamExt;
use ployz_core::{
    ContainerCreated, ContainerId, ContainerKind, MachineId, PullPolicy, ResolvedServiceSpec,
};

use super::{Error, LocalDocker, MachineSpecStore, ManagedLabels, create, docker_error};

const CONTAINER_NAME_ATTEMPTS: u8 = 4;

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
        let body = create::container_create_body(machine_id, kind, spec)?;
        self.prepare_image(&spec.container.image, spec.container.pull_policy)
            .await?;
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

        if let Err(error) = self.inject_configs(&container_id, spec).await {
            self.cleanup_created(&container_id).await?;
            return Err(error);
        }
        if let Err(error) = specs.put(&container_id, spec).await {
            self.cleanup_created(&container_id).await?;
            return Err(error.into());
        }
        Ok(ContainerCreated {
            container_id,
            display_name,
        })
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
        match self.ensure_managed(specs, container_id).await {
            Ok(()) => {}
            Err(Error::ContainerNotFound(_)) if specs.remove(container_id).await? => return Ok(()),
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
                specs.remove(container_id).await?;
                Ok(())
            }
            Err(Error::ContainerNotFound(_)) if specs.remove(container_id).await? => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn prepare_image(&self, image: &str, policy: PullPolicy) -> Result<(), Error> {
        let present = if policy == PullPolicy::Missing {
            match self.client.inspect_image(image).await {
                Ok(_) => true,
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => false,
                Err(error) => return Err(error.into()),
            }
        } else {
            false
        };
        if should_pull(policy, present) {
            let options = CreateImageOptionsBuilder::default()
                .from_image(image)
                .build();
            self.client
                .create_image(Some(options), None, None)
                .try_for_each(|_| async { Ok(()) })
                .await?;
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

    async fn cleanup_created(&self, container_id: &ContainerId) -> Result<(), Error> {
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

const fn should_pull(policy: PullPolicy, present: bool) -> bool {
    matches!(policy, PullPolicy::Always) || matches!(policy, PullPolicy::Missing) && !present
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
    fn pull_policy_controls_image_refresh() {
        assert!(should_pull(PullPolicy::Always, true));
        assert!(should_pull(PullPolicy::Always, false));
        assert!(!should_pull(PullPolicy::Missing, true));
        assert!(should_pull(PullPolicy::Missing, false));
        assert!(!should_pull(PullPolicy::Never, true));
        assert!(!should_pull(PullPolicy::Never, false));
    }

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
