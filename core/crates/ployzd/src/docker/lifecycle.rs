use std::future::Future;

use bollard::{
    errors::Error as DockerError,
    models::{
        ContainerCreateBody, ContainerInspectResponse, ContainerStateStatusEnum, Mount, MountType,
        NetworkConnectRequest,
    },
    query_parameters::{
        CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
    },
};
use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, Machine, MachineId,
    MachineStorageObservation, Namespace, ResolvedServiceSpec, ServicePlacementEligibility,
    ServicePlacementIneligibleReason, ServicePlacementUnknownReason, SlotKey,
};

#[cfg(test)]
use ployz_core::MachineGateway;

use crate::{docker_image::prepare_image, network::DOCKER_NETWORK_NAME};

use super::{
    ContainerRuntime, Error, ManagedLabels, create, docker_error, spec_store::ConfigOperation,
};

const CONTAINER_NAME_ATTEMPTS: u8 = 4;

/// The Container one creation landed on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ContainerCreated {
    pub(crate) container_id: ContainerId,
    pub(crate) display_name: String,
}
const LABEL_CREATION_KEY: &str = "ployz.creation.key";

/// Resolved container inputs shared by every Machine-local creation entry path.
pub(crate) struct ContainerRequest<'spec, Storage, Admission> {
    /// Optional identity of this currently existing creation.
    pub(crate) creation_key: Option<&'spec str>,
    pub(crate) deployment_id: Option<&'spec ployz_core::DeploymentLogId>,
    /// Credentials for pulling a private image, used for this pull only.
    pub(crate) registry_auth: Option<&'spec ployz_core::RegistryAuth>,
    /// Whether this is a long-running Service Container or a Pre-deploy Hook.
    pub(crate) kind: ContainerKind,
    /// Namespace that owns the resulting container.
    pub(crate) namespace: &'spec Namespace,
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
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerCreated, Error> {
        let machine = test_machine(*machine_id, gateway);
        self.create_with_admission(
            &machine,
            ContainerRequest {
                creation_key: None,
                deployment_id: None,
                registry_auth: None,
                kind,
                namespace,
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
            deployment_id,
            registry_auth,
            kind,
            namespace,
            spec,
            admission,
            storage,
        } = request;
        let reserved_name =
            creation_key.map(|key| creation_name(&machine.id, namespace, kind, key));
        if let (Some(name), Some(key)) = (&reserved_name, creation_key) {
            // Wait for an in-flight create to persist its spec before comparing a retry.
            let _operation = self.specs.config_operation().await;
            if let Some(existing) = self
                .matching_creation(machine, namespace, kind, spec, name, key)
                .await
                .map_err(E::from)?
            {
                return Ok(existing);
            }
        }
        // TODO: direct creation does not validate that an existing Service ID still uses
        // the same Service Name; that requires an observer-relative cluster snapshot.
        tracing::info!(
            namespace = namespace.as_str(),
            service = spec.name.as_str(),
            kind = match kind {
                ContainerKind::ServiceContainer => "service_container",
                ContainerKind::PreDeployHook => "pre_deploy_hook",
            },
            "create container"
        );
        require_eligible(
            self.admit_and_ensure_volumes(machine, namespace, spec, storage)
                .await
                .map_err(E::from)?,
        )
        .map_err(E::from)?;
        admission.await?;
        self.prepare_and_create(
            machine,
            kind,
            namespace,
            spec,
            creation_key,
            deployment_id,
            registry_auth,
        )
        .await
        .map_err(E::from)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "The request's parts, split after admission consumed its futures."
    )]
    async fn prepare_and_create(
        &self,
        machine: &Machine,
        kind: ContainerKind,
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
        creation_key: Option<&str>,
        deployment_id: Option<&ployz_core::DeploymentLogId>,
        registry_auth: Option<&ployz_core::RegistryAuth>,
    ) -> Result<ContainerCreated, Error> {
        let reserved_name =
            creation_key.map(|key| creation_name(&machine.id, namespace, kind, key));
        let mut body = create::container_create_body(
            &machine.id,
            machine.subnet.gateway(),
            kind,
            namespace,
            spec,
        )?;
        create::migrate_ingress_requests(
            &mut body,
            namespace,
            spec,
            std::path::Path::new("/proc/sys")
                .join(create::TCP_MIGRATE_REQ.replace('.', "/"))
                .exists(),
        );
        if let Some(id) = deployment_id {
            body.labels
                .get_or_insert_default()
                .insert(super::LABEL_DEPLOYMENT.into(), id.to_string());
        }
        if let Some(key) = creation_key {
            body.labels
                .get_or_insert_default()
                .insert(LABEL_CREATION_KEY.into(), key.into());
        }
        prepare_image(
            &self.docker.client,
            &spec.container.image,
            spec.container.pull_policy,
            registry_auth,
        )
        .await
        .map_err(Error::ImagePull)?;
        let mut config_operation = self.specs.config_operation().await;
        // Another request may have won while admission and image preparation ran.
        if let (Some(name), Some(key)) = (&reserved_name, creation_key)
            && let Some(existing) = self
                .matching_creation(machine, namespace, kind, spec, name, key)
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
                                        namespace,
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
            eprintln!(
                "failed to reclaim materialized configs: {error}",
                error = ployz_core::error_chain::inline(&error),
            );
        }
        result
    }

    /// The Container an earlier create under `key` left behind, so a resend adopts it
    /// instead of making a second one.
    pub(crate) async fn find_creation(
        &self,
        machine: &Machine,
        namespace: &Namespace,
        kind: ContainerKind,
        spec: &ResolvedServiceSpec,
        key: &str,
    ) -> Result<Option<ContainerCreated>, Error> {
        let name = creation_name(&machine.id, namespace, kind, key);
        self.matching_creation(machine, namespace, kind, spec, &name, key)
            .await
    }

    async fn matching_creation(
        &self,
        machine: &Machine,
        namespace: &Namespace,
        kind: ContainerKind,
        spec: &ResolvedServiceSpec,
        name: &str,
        key: &str,
    ) -> Result<Option<ContainerCreated>, Error> {
        let Some(existing) = self.keyed_container(&machine.id, name, key).await? else {
            return Ok(None);
        };
        if !same_creation(&existing, namespace, kind, spec) {
            return Err(Error::SlotNameOccupied(name.into()));
        }
        Ok(Some(ContainerCreated {
            container_id: existing.container_id,
            display_name: existing.into_parts().display_name,
        }))
    }

    pub(crate) async fn inspect_slot(
        &self,
        machine_id: &MachineId,
        slot: &SlotKey,
    ) -> Result<Option<ContainerObservation>, Error> {
        let key = slot.creation_key.as_str();
        let name = creation_name(machine_id, &slot.namespace, slot.kind, key);
        self.keyed_container(machine_id, &name, key).await
    }

    async fn keyed_container(
        &self,
        machine_id: &MachineId,
        name: &str,
        key: &str,
    ) -> Result<Option<ContainerObservation>, Error> {
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
        self.inspect_managed(&container_id, machine_id)
            .await
            .map(Some)
    }

    async fn admit_and_ensure_volumes(
        &self,
        machine: &Machine,
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
        storage: impl Future<Output = Option<MachineStorageObservation>>,
    ) -> Result<ServicePlacementEligibility, Error> {
        let storage = if spec.volume_graph().has_mounted_provisioned_volume() {
            storage.await
        } else {
            None
        };
        let eligibility =
            spec.placement_eligibility_in_namespace(namespace, machine, storage.as_ref());
        if matches!(eligibility, ServicePlacementEligibility::Eligible) {
            self.ensure_mounted_volumes(&machine.id, spec).await?;
        }
        Ok(eligibility)
    }

    pub async fn start(&self, container_id: &ContainerId) -> Result<(), Error> {
        let inspected = self.ensure_managed(container_id).await?;
        self.stranded().await.held.remove(container_id);
        self.start_inspected(container_id, &inspected).await
    }

    async fn start_inspected(
        &self,
        container_id: &ContainerId,
        inspected: &ContainerInspectResponse,
    ) -> Result<(), Error> {
        if lost_ployz_network(inspected)
            && let Err(error) = self
                .docker
                .client
                .connect_network(
                    DOCKER_NETWORK_NAME,
                    NetworkConnectRequest {
                        container: container_id.to_string(),
                        endpoint_config: None,
                    },
                )
                .await
            // A concurrent start may have attached it first.
            && lost_ployz_network(&self.ensure_managed(container_id).await?)
        {
            return Err(docker_error(container_id, error));
        }
        let result = self
            .docker
            .client
            .start_container(container_id.as_str(), None)
            .await;
        idempotent_lifecycle_result(container_id, result)
    }

    /// Start an Ingress Proxy whose last start failed, unless a request stopped it
    /// or is about to, or a container was created since `creates` was read or is
    /// being created.
    pub(super) async fn start_stranded(&self, container_id: &ContainerId, creates: u64) {
        let gate = self
            .stranded()
            .await
            .gates
            .entry(*container_id)
            .or_default()
            .clone();
        let Ok(_gate) = gate.try_lock() else {
            return;
        };
        if self.stranded().await.held.contains(container_id)
            || self.stopping.borrow().contains(container_id)
        {
            return;
        }
        let result = async {
            let inspected = self.ensure_managed(container_id).await?;
            // A newer Ingress Proxy may be about to take port 80; the next scan decides.
            if !failed_start(&inspected) || !self.docker.created_nothing_since(creates) {
                return Ok(());
            }
            self.start_inspected(container_id, &inspected).await
        }
        .await;
        let mut stranded = self.stranded().await;
        match result {
            Ok(()) => stranded.reported = None,
            Err(error) if stranded.reported != Some(*container_id) => {
                eprintln!(
                    "Ingress Proxy {container_id} is still not started: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
                stranded.reported = Some(*container_id);
            }
            Err(_) => {}
        }
    }

    pub(super) async fn stranded(
        &self,
    ) -> tokio::sync::MutexGuard<'_, super::observe::StrandedIngress> {
        self.stranded.lock().await
    }

    pub async fn stop(
        &self,
        container_id: &ContainerId,
        signal: Option<&str>,
        grace_period_seconds: Option<i32>,
    ) -> Result<(), Error> {
        self.ensure_managed(container_id).await?;
        let gate = {
            let mut stranded = self.stranded().await;
            stranded.held.insert(*container_id);
            stranded.gates.get(container_id).cloned()
        };
        if let Some(gate) = gate {
            drop(gate.lock().await);
        }
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
        // Stopped, it is no longer routed; still running after a failed stop, it should be again.
        self.clear_stopping(container_id);
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
            Ok(_) => {}
            Err(Error::ContainerNotFound(_)) if config_operation.remove(container_id).await? => {
                self.forget(container_id).await;
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
            }
            Err(Error::ContainerNotFound(_)) if config_operation.remove(container_id).await? => {}
            Err(error) => return Err(error),
        }
        self.forget(container_id).await;
        Ok(())
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
            self.forget(&container_id).await;
        }
        Ok(())
    }

    pub(super) async fn ensure_managed(
        &self,
        container_id: &ContainerId,
    ) -> Result<ContainerInspectResponse, Error> {
        let inspected = self
            .docker
            .client
            .inspect_container(container_id.as_str(), None)
            .await
            .map_err(|error| docker_error(container_id, error))?;
        let labels = inspected
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .ok_or(Error::NotManaged)?;
        ManagedLabels::parse(labels)?;
        self.specs
            .get(container_id)
            .await?
            .ok_or_else(|| Error::SpecNotFound(*container_id))?;
        Ok(inspected)
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
        let _creating = super::Creating::new(&self.creates);
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

pub(crate) fn same_creation(
    observed: &ContainerObservation,
    namespace: &Namespace,
    kind: ContainerKind,
    spec: &ResolvedServiceSpec,
) -> bool {
    observed.namespace == *namespace && observed.kind == kind && observed.resolved_spec == *spec
}

fn creation_name(
    machine: &MachineId,
    namespace: &Namespace,
    kind: ContainerKind,
    key: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let scope =
        serde_json::to_vec(&(machine, namespace, kind, key)).expect("creation scope serializes");
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
        build_concurrency: None,
    }
}

/// Docker left this container created and recorded why its start failed.
/// Docker clears `State.Error` when a start succeeds, so a container at rest with one
/// recorded is one whose last start failed: its first, or the one at boot that
/// leaves it exited with no retry.
fn failed_start(inspected: &ContainerInspectResponse) -> bool {
    inspected.state.as_ref().is_some_and(|state| {
        matches!(
            state.status,
            Some(ContainerStateStatusEnum::CREATED | ContainerStateStatusEnum::EXITED)
        ) && state
            .error
            .as_deref()
            .is_some_and(|error| !error.is_empty())
    })
}

/// Docker 29 empties a container's networks when its start fails, and a later start
/// then runs it with no network and no published ports.
fn lost_ployz_network(inspected: &ContainerInspectResponse) -> bool {
    let created_on_ployz = inspected
        .host_config
        .as_ref()
        .and_then(|host| host.network_mode.as_deref())
        == Some(DOCKER_NETWORK_NAME);
    let attached = inspected
        .network_settings
        .as_ref()
        .and_then(|settings| settings.networks.as_ref())
        .is_some_and(|networks| networks.contains_key(DOCKER_NETWORK_NAME));
    created_on_ployz && !attached
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::test_support::{
        FakeDocker, container_request, fake_runtime_with, machine, spec_with_sources,
    };

    fn inspected(network_mode: &str, networks: serde_json::Value) -> ContainerInspectResponse {
        serde_json::from_value(serde_json::json!({
            "HostConfig": { "NetworkMode": network_mode },
            "NetworkSettings": { "Networks": networks },
        }))
        .unwrap()
    }

    #[test]
    fn a_container_whose_failed_start_dropped_its_network_gets_it_back() {
        assert!(lost_ployz_network(&inspected(
            "ployz",
            serde_json::json!({})
        )));
        assert!(lost_ployz_network(&inspected(
            "ployz",
            serde_json::json!({ "bridge": {} })
        )));
    }

    #[test]
    fn an_attached_or_differently_networked_container_is_left_alone() {
        assert!(!lost_ployz_network(&inspected(
            "ployz",
            serde_json::json!({ "ployz": {} })
        )));
        assert!(!lost_ployz_network(&inspected(
            "host",
            serde_json::json!({})
        )));
    }

    async fn stranded(error: &str) -> (ContainerRuntime, FakeDocker, ContainerId) {
        stranded_with("created", error, FakeDocker::default()).await
    }

    async fn stranded_with(
        status: &str,
        error: &str,
        fake: FakeDocker,
    ) -> (ContainerRuntime, FakeDocker, ContainerId) {
        let (runtime, fake) = fake_runtime_with(FakeDocker {
            create_barrier: Some(std::sync::Arc::new(tokio::sync::Barrier::new(1))),
            ..fake
        })
        .await;
        let namespace = Namespace::parse("app").unwrap();
        let spec = spec_with_sources(Vec::new());
        let created = runtime
            .create_with_admission::<_, _, Error>(
                &machine(),
                container_request(
                    ContainerKind::ServiceContainer,
                    &namespace,
                    &spec,
                    std::future::ready(None),
                ),
            )
            .await
            .unwrap();
        let labels = fake
            .request_bodies
            .lock()
            .unwrap()
            .iter()
            .find(|(path, _)| path.ends_with("/containers/create"))
            .unwrap()
            .1
            .get("Labels")
            .unwrap()
            .clone();
        *fake.existing_container.lock().unwrap() = Some(serde_json::json!({
            "Id": created.container_id.as_str(),
            "Config": { "Labels": labels },
            "HostConfig": { "NetworkMode": "ployz" },
            "NetworkSettings": { "Networks": {} },
            "State": { "Status": status, "ExitCode": 128, "Error": error },
        }));
        fake.requests.lock().unwrap().clear();
        (runtime, fake, created.container_id)
    }

    fn started(fake: &FakeDocker) -> bool {
        fake.requests
            .lock()
            .unwrap()
            .drain(..)
            .any(|(method, path)| method == axum::http::Method::POST && path.ends_with("/start"))
    }

    const PORT_TAKEN: &str = "failed to bind host port 0.0.0.0:80/tcp: address already in use";

    #[tokio::test]
    async fn a_stranded_ingress_starts_until_a_request_stops_it() {
        let (runtime, fake, container_id) = stranded(PORT_TAKEN).await;

        runtime
            .start_stranded(&container_id, runtime.docker.creates())
            .await;
        assert!(started(&fake), "a failed first start is retried");

        runtime.stop(&container_id, None, None).await.unwrap();
        fake.requests.lock().unwrap().clear();
        runtime
            .start_stranded(&container_id, runtime.docker.creates())
            .await;
        assert!(!started(&fake), "a stopped ingress stays stopped");

        runtime.start(&container_id).await.unwrap();
        fake.requests.lock().unwrap().clear();
        runtime
            .start_stranded(&container_id, runtime.docker.creates())
            .await;
        assert!(started(&fake), "a requested start lifts the stop");
    }

    #[tokio::test]
    async fn an_ingress_about_to_be_stopped_is_not_started() {
        let (runtime, fake, container_id) = stranded(PORT_TAKEN).await;

        runtime.mark_stopping(container_id);
        runtime
            .start_stranded(&container_id, runtime.docker.creates())
            .await;

        assert!(!started(&fake));
    }

    #[tokio::test]
    async fn a_stop_that_races_a_stranded_start_leaves_the_ingress_stopped() {
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let (runtime, fake, container_id) = stranded_with(
            "created",
            PORT_TAKEN,
            FakeDocker {
                start_barrier: Some(barrier.clone()),
                ..Default::default()
            },
        )
        .await;

        let starting = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                runtime
                    .start_stranded(&container_id, runtime.docker.creates())
                    .await
            }
        });
        barrier.wait().await;
        let stopping = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.stop(&container_id, None, None).await }
        });
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(
            !fake
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(_, path)| path.ends_with("/stop")),
            "the stop waits for the start it raced"
        );
        barrier.wait().await;
        starting.await.unwrap();
        stopping.await.unwrap().unwrap();

        let lifecycle = fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == axum::http::Method::POST)
            .map(|(_, path)| path.rsplit('/').next().unwrap().to_owned())
            .filter(|action| action == "start" || action == "stop")
            .collect::<Vec<_>>();
        assert_eq!(lifecycle, ["start", "stop"]);
        fake.requests.lock().unwrap().clear();
        runtime
            .start_stranded(&container_id, runtime.docker.creates())
            .await;
        assert!(!started(&fake), "the stop holds");
    }

    #[tokio::test]
    async fn a_create_in_flight_when_the_scan_began_voids_the_start() {
        let (runtime, fake, container_id) = stranded(PORT_TAKEN).await;
        let creating = super::super::Creating::new(&runtime.docker.creates);
        let creates = runtime.docker.creates();

        runtime.start_stranded(&container_id, creates).await;
        assert!(!started(&fake), "a newer ingress may be appearing");

        drop(creating);
        runtime.start_stranded(&container_id, creates).await;
        assert!(!started(&fake), "it appeared after the scan began");

        runtime
            .start_stranded(&container_id, runtime.docker.creates())
            .await;
        assert!(started(&fake), "the next scan decides");
    }

    #[tokio::test]
    async fn a_container_created_since_the_scan_voids_the_start() {
        let (runtime, fake, container_id) = stranded(PORT_TAKEN).await;
        let creates = runtime.docker.creates();
        let stranded = fake.existing_container.lock().unwrap().clone();
        runtime
            .create_with_admission::<_, _, Error>(
                &machine(),
                container_request(
                    ContainerKind::ServiceContainer,
                    &Namespace::parse("app").unwrap(),
                    &spec_with_sources(Vec::new()),
                    std::future::ready(None),
                ),
            )
            .await
            .unwrap();
        *fake.existing_container.lock().unwrap() = stranded;
        fake.requests.lock().unwrap().clear();

        runtime.start_stranded(&container_id, creates).await;

        assert!(!started(&fake), "the next scan decides");
    }

    #[tokio::test]
    async fn a_stranded_start_in_flight_does_not_hold_up_other_containers() {
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let (runtime, _fake, container_id) = stranded_with(
            "created",
            PORT_TAKEN,
            FakeDocker {
                start_barrier: Some(barrier.clone()),
                ..Default::default()
            },
        )
        .await;
        let other = ContainerId::parse("b".repeat(64)).unwrap();

        let starting = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                runtime
                    .start_stranded(&container_id, runtime.docker.creates())
                    .await
            }
        });
        barrier.wait().await;
        tokio::time::timeout(std::time::Duration::from_secs(1), runtime.forget(&other))
            .await
            .expect("forgetting another container does not wait for the start");
        barrier.wait().await;
        starting.await.unwrap();
    }

    #[tokio::test]
    async fn an_ingress_docker_never_tried_to_start_is_left_alone() {
        let (runtime, fake, container_id) = stranded("").await;

        runtime
            .start_stranded(&container_id, runtime.docker.creates())
            .await;

        assert!(!started(&fake));
    }

    // Docker starts a container with a restart policy once at boot and records a
    // failure as exited, with no retry.
    #[tokio::test]
    async fn an_ingress_whose_start_at_boot_failed_is_started_again() {
        let (runtime, fake, container_id) =
            stranded_with("exited", PORT_TAKEN, FakeDocker::default()).await;

        runtime
            .start_stranded(&container_id, runtime.docker.creates())
            .await;

        assert!(started(&fake));
    }

    #[tokio::test]
    async fn an_ingress_that_exited_on_its_own_is_left_alone() {
        let (runtime, fake, container_id) =
            stranded_with("exited", "", FakeDocker::default()).await;

        runtime
            .start_stranded(&container_id, runtime.docker.creates())
            .await;

        assert!(!started(&fake));
    }

    #[tokio::test]
    async fn a_start_that_races_another_for_the_network_still_starts() {
        let (runtime, fake, container_id) = stranded(PORT_TAKEN).await;
        fake.connect_raced
            .store(true, std::sync::atomic::Ordering::SeqCst);

        runtime.start(&container_id).await.unwrap();

        assert!(started(&fake));
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

#[cfg(test)]
mod admission_tests;
