use std::future::Future;

use bollard::{
    errors::Error as DockerError,
    models::{ContainerCreateBody, Mount, MountType},
    query_parameters::{
        CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
    },
};
use ployz_core::{
    ContainerCreated, ContainerId, ContainerKind, ContainerRuntimeObservation, MachineGateway,
    MachineId, MachineStorageObservation, ProjectName, ResolvedServiceSpec,
};

use crate::docker_image::prepare_image;

use super::{
    ContainerRuntime, Error, ManagedLabels, NetworkAttachment, create, docker_error,
    spec_store::ConfigOperation,
};

const CONTAINER_NAME_ATTEMPTS: u8 = 4;

/// Resolved container inputs shared by every Machine-local creation entry path.
pub(crate) struct ContainerRequest<'spec, Storage> {
    /// Whether this is a long-running Service Container or a Pre-deploy Hook.
    pub(crate) kind: ContainerKind,
    /// Project that owns the resulting container.
    pub(crate) project_name: &'spec ProjectName,
    /// Fully resolved Service specification to persist and execute.
    pub(crate) spec: &'spec ResolvedServiceSpec,
    /// Docker network attachment prepared for this Service kind and backend.
    pub(crate) network: NetworkAttachment,
    /// Fresh local storage observation deferred to final container admission.
    pub(crate) storage: Storage,
}

impl ContainerRuntime {
    #[cfg(test)]
    pub(crate) async fn create_for_test(
        &self,
        machine_id: &MachineId,
        gateway: MachineGateway,
        kind: ContainerKind,
        project_name: &ProjectName,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerCreated, Error> {
        self.create_with_network(
            machine_id,
            gateway,
            ContainerRequest {
                kind,
                project_name,
                spec,
                network: NetworkAttachment::Bridge,
                storage: std::future::ready(None),
            },
        )
        .await
    }

    /// Prepare a container, run final storage admission, and create it.
    ///
    /// # Errors
    ///
    /// Returns when preparation, final admission, Volume Ensure, or Docker creation fails.
    pub(crate) async fn create_with_network(
        &self,
        machine_id: &MachineId,
        gateway: MachineGateway,
        request: ContainerRequest<
            '_,
            impl Future<Output = Option<MachineStorageObservation>> + Send,
        >,
    ) -> Result<ContainerCreated, Error> {
        // TODO(UT-030): direct creation does not validate that an existing Service ID still uses
        // the same Service Name; that requires an observer-relative cluster snapshot.
        tracing::info!(
            project = request.project_name.as_str(),
            service = request.spec.name.as_str(),
            kind = match request.kind {
                ContainerKind::ServiceContainer => "service_container",
                ContainerKind::PreDeployHook => "pre_deploy_hook",
            },
            "create container"
        );
        self.prepare_and_create(machine_id, gateway, request, None)
            .await
    }

    /// One running Service Container for this Global on this Machine. No loop after return.
    ///
    /// # Errors
    ///
    /// Returns when listing managed Containers, ensuring mounted Volumes, pulling
    /// the image, creating the Container, or starting it fails.
    pub(crate) async fn ensure_global_slot(
        &self,
        machine_id: &MachineId,
        gateway: MachineGateway,
        request: ContainerRequest<
            '_,
            impl Future<Output = Option<MachineStorageObservation>> + Send,
        >,
    ) -> Result<ContainerCreated, Error> {
        if let Some(existing) = self.existing_global_slot(machine_id, request.spec).await? {
            self.admit_storage_and_ensure_volumes(machine_id, request.spec, request.storage)
                .await?;
            if !runtime_is_running(&existing.runtime) {
                self.start(&existing.container_id).await?;
            }
            return Ok(ContainerCreated {
                container_id: existing.container_id,
                display_name: existing.display_name,
            });
        }
        let name = global_slot_name(request.spec);
        let created = self
            .prepare_and_create(machine_id, gateway, request, Some(name))
            .await?;
        self.start(&created.container_id).await?;
        Ok(created)
    }

    async fn existing_global_slot(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
    ) -> Result<Option<ployz_core::ContainerObservation>, Error> {
        let mut found = None;
        for observation in self.list_managed(machine_id).await? {
            if observation.kind != ContainerKind::ServiceContainer
                || observation.service_id != spec.service_id
            {
                continue;
            }
            let running = runtime_is_running(&observation.runtime);
            match found {
                Some(_) if !running => {}
                _ => found = Some(observation),
            }
            if running {
                break;
            }
        }
        Ok(found)
    }

    async fn prepare_and_create(
        &self,
        machine_id: &MachineId,
        gateway: MachineGateway,
        request: ContainerRequest<
            '_,
            impl Future<Output = Option<MachineStorageObservation>> + Send,
        >,
        reserved_name: Option<String>,
    ) -> Result<ContainerCreated, Error> {
        let mut body = create::container_create_body(
            machine_id,
            gateway,
            request.kind,
            request.project_name,
            request.spec,
            request.network,
        )?;
        let mounts = body
            .host_config
            .get_or_insert_default()
            .mounts
            .get_or_insert_default();
        prepare_image(
            &self.docker.client,
            &request.spec.container.image,
            request.spec.container.pull_policy,
        )
        .await?;
        let mut config_operation = self.specs.config_operation().await;
        mounts.extend(docker_config_mounts(&mut config_operation, request.spec).await?);
        self.finish_create(machine_id, request, body, config_operation, reserved_name)
            .await
    }

    async fn finish_create(
        &self,
        machine_id: &MachineId,
        request: ContainerRequest<
            '_,
            impl Future<Output = Option<MachineStorageObservation>> + Send,
        >,
        body: bollard::models::ContainerCreateBody,
        mut config_operation: ConfigOperation<'_>,
        reserved_name: Option<String>,
    ) -> Result<ContainerCreated, Error> {
        let result = async {
            self.admit_storage_and_ensure_volumes(machine_id, request.spec, request.storage)
                .await?;
            let (created, display_name) = match reserved_name {
                Some(display_name) => {
                    let options = CreateContainerOptionsBuilder::default()
                        .name(&display_name)
                        .build();
                    match self
                        .docker
                        .create_container(Some(options), body, request.network)
                        .await
                    {
                        Ok(created) => (created, display_name),
                        Err(Error::Docker(DockerError::DockerResponseServerError {
                            status_code: 409,
                            ..
                        })) => {
                            let existing = self
                                .inspect_managed_by_name(machine_id, &display_name)
                                .await?;
                            return Ok(ContainerCreated {
                                container_id: existing.container_id,
                                display_name: existing.display_name,
                            });
                        }
                        Err(error) => return Err(error),
                    }
                }
                None => {
                    let mut attempt = 0;
                    loop {
                        attempt += 1;
                        let suffix = MachineId::random().as_str()[..4].to_owned();
                        let display_name = match request.kind {
                            ContainerKind::ServiceContainer => {
                                format!("{}-{suffix}", request.spec.name)
                            }
                            ContainerKind::PreDeployHook => {
                                format!("{}-pre-deploy-{suffix}", request.spec.name)
                            }
                        };
                        let options = CreateContainerOptionsBuilder::default()
                            .name(&display_name)
                            .build();
                        match self
                            .docker
                            .create_container(Some(options), body.clone(), request.network)
                            .await
                        {
                            Ok(created) => break (created, display_name),
                            Err(Error::Docker(error)) if retry_name_conflict(attempt, &error) => {}
                            Err(error) => return Err(error),
                        }
                    }
                }
            };
            let container_id =
                ContainerId::parse(created.id).map_err(|source| Error::InvalidValue {
                    field: "container ID",
                    source,
                })?;
            if let Err(error) = config_operation.put(&container_id, request.spec).await {
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

    async fn admit_storage_and_ensure_volumes(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
        storage: impl Future<Output = Option<MachineStorageObservation>>,
    ) -> Result<(), Error> {
        if spec.volume_graph.has_mounted_provisioned_volume() {
            match storage.await {
                Some(MachineStorageObservation::Ready | MachineStorageObservation::Pool { .. }) => {
                }
                Some(MachineStorageObservation::Stateless) => {
                    return Err(Error::ProvisionedStorageUnsupported);
                }
                None => return Err(Error::StorageUnobservable),
            }
        }
        self.ensure_mounted_volumes(machine_id, spec).await
    }

    async fn inspect_managed_by_name(
        &self,
        machine_id: &MachineId,
        name: &str,
    ) -> Result<ployz_core::ContainerObservation, Error> {
        let inspected = self.docker.client.inspect_container(name, None).await?;
        let container_id = ContainerId::parse(
            inspected.id.ok_or(Error::MissingField("container ID"))?,
        )
        .map_err(|source| Error::InvalidValue {
            field: "container ID",
            source,
        })?;
        self.inspect_managed(&container_id, machine_id).await
    }

    pub async fn start(&self, container_id: &ContainerId) -> Result<(), Error> {
        self.ensure_managed(container_id).await?;
        let result = self
            .docker
            .client
            .start_container(container_id.as_str(), None)
            .await;
        idempotent_lifecycle_result(container_id, result)
    }

    pub async fn stop(
        &self,
        container_id: &ContainerId,
        signal: Option<&str>,
        grace_period_seconds: Option<i32>,
    ) -> Result<(), Error> {
        self.ensure_managed(container_id).await?;
        let mut options = StopContainerOptionsBuilder::default();
        if let Some(signal) = signal {
            options = options.signal(signal);
        }
        if let Some(seconds) = grace_period_seconds {
            options = options.t(seconds);
        }
        let result = self
            .docker
            .client
            .stop_container(container_id.as_str(), Some(options.build()))
            .await;
        idempotent_lifecycle_result(container_id, result)
    }

    pub async fn remove(
        &self,
        container_id: &ContainerId,
        remove_volumes: bool,
        force: bool,
    ) -> Result<(), Error> {
        let mut config_operation = self.specs.config_operation().await;
        match self.ensure_managed(container_id).await {
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
            .docker
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

    pub async fn remove_all_managed(&self) -> Result<(), Error> {
        let mut config_operation = self.specs.config_operation().await;
        for container_id in self.docker.managed_container_ids().await? {
            match self
                .docker
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

    async fn ensure_managed(&self, container_id: &ContainerId) -> Result<(), Error> {
        let inspected = self
            .docker
            .client
            .inspect_container(container_id.as_str(), None)
            .await
            .map_err(|error| docker_error(container_id, error))?;
        let labels = inspected
            .config
            .and_then(|config| config.labels)
            .ok_or(Error::NotManaged)?;
        ManagedLabels::parse(&labels)?;
        self.specs
            .get(container_id)
            .await?
            .ok_or_else(|| Error::SpecNotFound(*container_id))?;
        Ok(())
    }

    async fn force_remove_container(&self, container_id: &ContainerId) -> Result<(), Error> {
        let options = RemoveContainerOptionsBuilder::default()
            .v(true)
            .force(true)
            .build();
        match self
            .docker
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

impl super::LocalDocker {
    pub(super) async fn create_endpoint_container(
        &self,
        options: Option<bollard::query_parameters::CreateContainerOptions>,
        body: ContainerCreateBody,
    ) -> Result<bollard::models::ContainerCreateResponse, Error> {
        self.create_container(options, body, NetworkAttachment::Bridge)
            .await
    }

    pub(super) async fn create_container(
        &self,
        options: Option<bollard::query_parameters::CreateContainerOptions>,
        body: ContainerCreateBody,
        network: NetworkAttachment,
    ) -> Result<bollard::models::ContainerCreateResponse, Error> {
        if matches!(network, NetworkAttachment::Host) {
            return Ok(self.client.create_container(options, body).await?);
        }
        let _gate = self.endpoint_creates.lock().await;
        if self.bridge_capacity().await?.free_endpoints() == 0 {
            return Err(Error::EndpointCapacity);
        }
        Ok(self.client.create_container(options, body).await?)
    }
}

async fn docker_config_mounts(
    configs: &mut ConfigOperation<'_>,
    spec: &ResolvedServiceSpec,
) -> Result<Vec<Mount>, Error> {
    let mut mounts = Vec::with_capacity(spec.config_graph.mounts().len());
    for mount in spec.config_graph.mounts() {
        let config = spec.config_graph.config_for(mount);
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

fn runtime_is_running(runtime: &ContainerRuntimeObservation) -> bool {
    matches!(runtime, ContainerRuntimeObservation::Running { .. })
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

fn global_slot_name(spec: &ResolvedServiceSpec) -> String {
    let id = spec.service_id.as_str();
    let suffix = id.get(..8).unwrap_or(id);
    format!("{}-{suffix}", spec.name)
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
