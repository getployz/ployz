use std::future::Future;

use bollard::{
    errors::Error as DockerError,
    models::{ContainerCreateBody, Mount, MountType},
    query_parameters::{
        CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
    },
};
use ployz_core::{
    ContainerCreated, ContainerId, ContainerKind, Machine, MachineId, MachineStorageObservation,
    ProjectName, ResolvedServiceSpec, ServicePlacementEligibility,
    ServicePlacementIneligibleReason, ServicePlacementUnknownReason,
};

#[cfg(test)]
use ployz_core::MachineGateway;

use crate::docker_image::prepare_image;

use super::{
    ContainerRuntime, Error, ManagedLabels, create, docker_error, spec_store::ConfigOperation,
};

const CONTAINER_NAME_ATTEMPTS: u8 = 4;
const LABEL_CREATION_KEY: &str = "ployz.creation.key";

/// Resolved container inputs shared by every Machine-local creation entry path.
pub(crate) struct ContainerRequest<'spec, Storage, Admission> {
    /// Optional identity of this currently existing creation.
    pub(crate) creation_key: Option<&'spec str>,
    /// Whether this is a long-running Service Container or a Pre-deploy Hook.
    pub(crate) kind: ContainerKind,
    /// Project that owns the resulting container.
    pub(crate) project_name: &'spec ProjectName,
    /// Fully resolved Service specification to persist and execute.
    pub(crate) spec: &'spec ResolvedServiceSpec,
    /// Deferred Machine-local admission, awaited only after volume admission.
    pub(crate) admission: Admission,
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
        let machine = test_machine(*machine_id, gateway);
        self.create_with_admission(
            &machine,
            ContainerRequest {
                creation_key: None,
                kind,
                project_name,
                spec,
                admission: std::future::ready(Ok::<_, Error>(())),
                storage: std::future::ready(None),
            },
        )
        .await
    }

    /// Run final admission and create a container.
    ///
    /// # Errors
    ///
    /// Returns when final admission, Volume Ensure, or Docker creation fails.
    pub(crate) async fn create_with_admission<Storage, Admission, E>(
        &self,
        machine: &Machine,
        request: ContainerRequest<'_, Storage, Admission>,
    ) -> Result<ContainerCreated, E>
    where
        Storage: Future<Output = Option<MachineStorageObservation>> + Send,
        Admission: Future<Output = Result<(), E>> + Send,
        E: From<Error>,
    {
        let ContainerRequest {
            creation_key,
            kind,
            project_name,
            spec,
            admission,
            storage,
        } = request;
        let reserved_name =
            creation_key.map(|key| creation_name(&machine.id, project_name, kind, key));
        if let (Some(name), Some(key)) = (&reserved_name, creation_key) {
            // Wait for an in-flight create to persist its spec before comparing a retry.
            let _operation = self.specs.config_operation().await;
            if let Some(existing) = self
                .matching_creation(machine, project_name, kind, spec, name, key)
                .await
                .map_err(E::from)?
            {
                return Ok(existing);
            }
        }
        // TODO: direct creation does not validate that an existing Service ID still uses
        // the same Service Name; that requires an observer-relative cluster snapshot.
        tracing::info!(
            project = project_name.as_str(),
            service = spec.name.as_str(),
            kind = match kind {
                ContainerKind::ServiceContainer => "service_container",
                ContainerKind::PreDeployHook => "pre_deploy_hook",
            },
            "create container"
        );
        require_eligible(
            self.admit_and_ensure_volumes(machine, project_name, spec, storage)
                .await
                .map_err(E::from)?,
        )
        .map_err(E::from)?;
        admission.await?;
        self.prepare_and_create(
            machine,
            kind,
            project_name,
            spec,
            reserved_name,
            creation_key,
        )
        .await
        .map_err(E::from)
    }

    async fn prepare_and_create(
        &self,
        machine: &Machine,
        kind: ContainerKind,
        project_name: &ProjectName,
        spec: &ResolvedServiceSpec,
        reserved_name: Option<String>,
        creation_key: Option<&str>,
    ) -> Result<ContainerCreated, Error> {
        let mut body = create::container_create_body(
            &machine.id,
            machine.subnet.gateway(),
            kind,
            project_name,
            spec,
        )?;
        if let Some(key) = creation_key {
            body.labels
                .get_or_insert_default()
                .insert(LABEL_CREATION_KEY.into(), key.into());
        }
        prepare_image(
            &self.docker.client,
            &spec.container.image,
            spec.container.pull_policy,
        )
        .await?;
        let mut config_operation = self.specs.config_operation().await;
        // Another request may have won while admission and image preparation ran.
        if let (Some(name), Some(key)) = (&reserved_name, creation_key)
            && let Some(existing) = self
                .matching_creation(machine, project_name, kind, spec, name, key)
                .await?
        {
            return Ok(existing);
        }
        let mounts = body
            .host_config
            .get_or_insert_default()
            .mounts
            .get_or_insert_default();
        mounts.extend(docker_config_mounts(&mut config_operation, spec).await?);
        let result = async {
            let (created, display_name) = match reserved_name {
                Some(display_name) => {
                    let options = CreateContainerOptionsBuilder::default()
                        .name(&display_name)
                        .build();
                    match self.docker.create_container(Some(options), body).await {
                        Ok(created) => (created, display_name),
                        Err(Error::Docker(DockerError::DockerResponseServerError {
                            status_code: 409,
                            ..
                        })) => {
                            if let Some(key) = creation_key {
                                return self
                                    .matching_creation(
                                        machine,
                                        project_name,
                                        kind,
                                        spec,
                                        &display_name,
                                        key,
                                    )
                                    .await?
                                    .ok_or(Error::SlotNameOccupied(display_name));
                            }
                            return Err(Error::SlotNameOccupied(display_name));
                        }
                        Err(error) => return Err(error),
                    }
                }
                None => {
                    let mut attempt = 0;
                    loop {
                        attempt += 1;
                        let suffix = MachineId::random().as_str()[..4].to_owned();
                        let display_name = match kind {
                            ContainerKind::ServiceContainer => {
                                format!("{}-{suffix}", spec.name)
                            }
                            ContainerKind::PreDeployHook => {
                                format!("{}-pre-deploy-{suffix}", spec.name)
                            }
                        };
                        let options = CreateContainerOptionsBuilder::default()
                            .name(&display_name)
                            .build();
                        match self
                            .docker
                            .create_container(Some(options), body.clone())
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

    async fn matching_creation(
        &self,
        machine: &Machine,
        project: &ProjectName,
        kind: ContainerKind,
        spec: &ResolvedServiceSpec,
        name: &str,
        key: &str,
    ) -> Result<Option<ContainerCreated>, Error> {
        let inspected = match self.docker.client.inspect_container(name, None).await {
            Ok(inspected) => inspected,
            Err(DockerError::DockerResponseServerError {
                status_code: 404, ..
            }) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if inspected
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .and_then(|labels| labels.get(LABEL_CREATION_KEY))
            .map(String::as_str)
            != Some(key)
        {
            return Err(Error::SlotNameOccupied(name.into()));
        }
        let container_id = ContainerId::parse(
            inspected.id.ok_or(Error::MissingField("container ID"))?,
        )
        .map_err(|source| Error::InvalidValue {
            field: "container ID",
            source,
        })?;
        let existing = self.inspect_managed(&container_id, &machine.id).await?;
        if existing.project_name != *project
            || existing.kind != kind
            || existing.resolved_spec != *spec
        {
            return Err(Error::SlotNameOccupied(name.into()));
        }
        Ok(Some(ContainerCreated {
            container_id: existing.container_id,
            display_name: existing.into_parts().display_name,
        }))
    }

    async fn admit_and_ensure_volumes(
        &self,
        machine: &Machine,
        project: &ProjectName,
        spec: &ResolvedServiceSpec,
        storage: impl Future<Output = Option<MachineStorageObservation>>,
    ) -> Result<ServicePlacementEligibility, Error> {
        let storage = if spec.volume_graph().has_mounted_provisioned_volume() {
            storage.await
        } else {
            None
        };
        let eligibility = spec.placement_eligibility_in_project(project, machine, storage.as_ref());
        if matches!(eligibility, ServicePlacementEligibility::Eligible) {
            self.ensure_mounted_volumes(&machine.id, spec).await?;
        }
        Ok(eligibility)
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
    pub(super) async fn create_container(
        &self,
        options: Option<bollard::query_parameters::CreateContainerOptions>,
        body: ContainerCreateBody,
    ) -> Result<bollard::models::ContainerCreateResponse, Error> {
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
    let mut mounts = Vec::with_capacity(spec.config_graph().mounts().len());
    for mount in spec.config_graph().mounts() {
        let config = spec.config_graph().config_for(mount);
        let target = mount
            .target
            .as_ref()
            .expect("mount admission resolves config destinations")
            .to_string();
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

fn creation_name(
    machine: &MachineId,
    project: &ProjectName,
    kind: ContainerKind,
    key: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let scope =
        serde_json::to_vec(&(machine, project, kind, key)).expect("creation scope serializes");
    format!("ployz-create-{}", hex::encode(Sha256::digest(scope)))
}

/// Refuse unsupported or unobservable placement without conflating the two.
pub(crate) fn require_eligible(eligibility: ServicePlacementEligibility) -> Result<(), Error> {
    match eligibility {
        ServicePlacementEligibility::Eligible => Ok(()),
        ServicePlacementEligibility::Ineligible(reason) => Err(ineligible_error(reason)),
        ServicePlacementEligibility::Unknown(reason) => Err(unknown_error(reason)),
    }
}

fn ineligible_error(reason: ServicePlacementIneligibleReason) -> Error {
    match reason {
        ServicePlacementIneligibleReason::WorkNotAccepted => Error::WorkNotAccepted,
        ServicePlacementIneligibleReason::PlacementMismatch => Error::ServicePlacementMismatch,
        ServicePlacementIneligibleReason::ProvisionedStorageUnsupported => {
            Error::ProvisionedStorageUnsupported
        }
    }
}

fn unknown_error(reason: ServicePlacementUnknownReason) -> Error {
    match reason {
        ServicePlacementUnknownReason::MissingStorageEvidence => Error::StorageUnobservable,
    }
}

#[cfg(test)]
pub(super) fn test_machine(machine_id: MachineId, gateway: MachineGateway) -> Machine {
    use ployz_core::{MachineName, WireGuardPublicKey};

    let [a, b, c, _] = gateway.0.octets();
    Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: machine_id,
        name: MachineName::parse("docker-test").unwrap(),
        subnet: format!("{a}.{b}.{c}.0/24").parse().unwrap(),
        public_key: WireGuardPublicKey([0; 32]),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
    }
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

#[cfg(test)]
mod admission_tests;
