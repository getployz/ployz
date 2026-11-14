use ployz_core::labels::{self, ServiceLabels};
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
    ContainerId, ContainerKind, ContainerObservation, Gen, Machine, MachineId,
    MachineStorageObservation, Namespace, ResolvedServiceSpec, ServiceName, ServiceNetwork,
    ServicePlacementEligibility, ServicePlacementIneligibleReason, ServicePlacementUnknownReason,
    SlotKey,
};

#[cfg(test)]
use ployz_core::MachineGateway;

use crate::{
    docker_image::{ImagePull, prepare_image},
    network::{DOCKER_NETWORK_NAME, TUNNEL_NETWORK_NAME},
};

use super::{
    ContainerRuntime, Error, IngressRecovery, create,
    create::CreationNetwork,
    doors::{self, Door},
    error::{Status, classify, docker_error},
    spec_store::ConfigOperation,
};

const CONTAINER_NAME_ATTEMPTS: u8 = 4;

/// How a Container leaves through [`ContainerRuntime::retire`].
pub(super) enum How {
    /// A request to remove it, as asked.
    Asked { remove_volumes: bool, force: bool },
    /// The Machine is leaving: stopped, then removed with its anonymous volumes.
    Departing,
}

/// The Container one creation landed on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ContainerCreated {
    pub(crate) container_id: ContainerId,
    pub(crate) display_name: String,
}

/// Resolved container inputs shared by every Machine-local creation entry path.
pub(crate) struct ContainerRequest<'spec, Storage, Admission> {
    /// Optional identity of this currently existing creation.
    pub(crate) creation_key: Option<&'spec str>,
    pub(crate) deployment_id: Option<&'spec ployz_core::DeploymentLogId>,
    pub(crate) r#gen: Gen,
    /// How creation obtains the image.
    pub(crate) image: ImagePull<'spec>,
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

/// A request once admission has consumed its futures: what the rest of creation reads.
struct Creating<'a> {
    machine: &'a Machine,
    creation_key: Option<&'a str>,
    deployment_id: Option<&'a ployz_core::DeploymentLogId>,
    r#gen: Gen,
    image: ImagePull<'a>,
    kind: ContainerKind,
    namespace: &'a Namespace,
    spec: &'a ResolvedServiceSpec,
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
                r#gen: Gen::FIRST,
                image: ImagePull::Policy(None),
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
            r#gen,
            image,
            kind,
            namespace,
            spec,
            admission,
            storage,
        } = request;
        if let Some(key) = creation_key {
            // Wait for an in-flight create to persist its spec before comparing a retry.
            let mut operation = self.specs.config_operation().await;
            if let Some(existing) = self
                .matching_creation(&mut operation, machine, namespace, kind, spec, key)
                .await
                .map_err(E::from)?
            {
                return Ok(existing);
            }
        }
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
        // Boxed: every caller's future embeds this one, and the stages hold a create body.
        Box::pin(self.create_admitted(Creating {
            machine,
            creation_key,
            deployment_id,
            r#gen,
            image,
            kind,
            namespace,
            spec,
        }))
        .await
        .map_err(E::from)
    }

    /// Everything after admission: the body Docker is sent, the image, and the Container
    /// with its spec, written under one config lock.
    async fn create_admitted(&self, creating: Creating<'_>) -> Result<ContainerCreated, Error> {
        let Creating {
            machine,
            creation_key,
            kind,
            namespace,
            spec,
            ..
        } = creating;
        let mut body = self.build_body(&creating).await?;
        if let ImagePull::Policy(registry_auth) = creating.image
            && let Err(error) = prepare_image(
                &self.docker.client,
                &spec.container.image,
                spec.container.pull_policy,
                registry_auth,
            )
            .await
        {
            return Err(self.pull_failure(Error::ImagePull(error)).await);
        }
        let mut config_operation = self.specs.config_operation().await;
        // Another request may have won while admission and image preparation ran.
        if let Some(key) = creation_key
            && let Some(existing) = self
                .matching_creation(&mut config_operation, machine, namespace, kind, spec, key)
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
        let result = self
            .create_and_record(&creating, &mut config_operation, body)
            .await;
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

    /// The create body for Docker: the network it joins, the sysctls only the Ingress
    /// holder takes, and the labels that say which run made it.
    async fn build_body(&self, creating: &Creating<'_>) -> Result<ContainerCreateBody, Error> {
        let Creating {
            machine,
            creation_key,
            deployment_id,
            r#gen,
            kind,
            namespace,
            spec,
            ..
        } = *creating;
        if create::holds_sealed(kind, spec) {
            self.log_store.admit_sealed().await?;
        }
        let network = match spec.container.network {
            None => CreationNetwork::Bridge,
            Some(ServiceNetwork::IngressHolder) => CreationNetwork::Holder(
                self.running_door(Door::Holder)
                    .await?
                    .ok_or(Error::IngressHolderNotRunning)?,
            ),
            Some(ServiceNetwork::TunnelOrigin) => CreationNetwork::Holder(
                self.running_door(Door::TunnelOrigin)
                    .await?
                    .ok_or(Error::TunnelOriginNotRunning)?,
            ),
            Some(ServiceNetwork::Tunnel) => {
                self.docker.ensure_tunnel_network().await?;
                CreationNetwork::Tunnel
            }
            Some(ServiceNetwork::Unrecognized) => return Err(Error::UnrecognizedNetwork),
        };
        let mut body = create::container_create_body(
            &machine.id,
            machine.subnet.gateway(),
            kind,
            namespace,
            spec,
            &network,
            self.sealing_key_dir.as_deref(),
        )?;
        create::ingress_sysctls(
            &mut body,
            namespace,
            spec,
            std::path::Path::new("/proc/sys")
                .join(create::TCP_MIGRATE_REQ.replace('.', "/"))
                .exists(),
        );
        labels::stamp_run(
            body.labels.get_or_insert_default(),
            deployment_id,
            creation_key,
            r#gen,
        );
        Ok(body)
    }

    /// Creates the Container, under its slot's name when it has a key, then writes its
    /// spec. A Container with no spec is removed again, so none outlives a failed write.
    async fn create_and_record(
        &self,
        creating: &Creating<'_>,
        config_operation: &mut ConfigOperation<'_>,
        body: ContainerCreateBody,
    ) -> Result<ContainerCreated, Error> {
        let Creating {
            machine,
            creation_key,
            kind,
            namespace,
            spec,
            ..
        } = *creating;
        let (created, display_name) = match creation_key {
            Some(key) => {
                let display_name = creation_name(&machine.id, namespace, &spec.name, kind, key);
                let options = CreateContainerOptionsBuilder::default()
                    .name(&display_name)
                    .build();
                match self.docker.create_container(Some(options), body).await {
                    Ok(created) => (created, display_name),
                    Err(Error::Docker(ref error)) if classify(error) == Status::Conflict => {
                        return self
                            .matching_creation(
                                config_operation,
                                machine,
                                namespace,
                                kind,
                                spec,
                                key,
                            )
                            .await?
                            .ok_or(Error::SlotNameOccupied(display_name));
                    }
                    Err(error) => return Err(error),
                }
            }
            None => self.create_unkeyed(kind, spec, body).await?,
        };
        let container_id =
            ContainerId::parse(created.id).map_err(|source| Error::InvalidValue {
                field: "container ID",
                source,
            })?;
        // A daemon killed here leaves a Container no spec describes.
        crate::faults::kill_inside("CreateContainer.BeforeSpec", None);
        let slot =
            creation_key.map(|key| super::spec_store::CreationSlot::new(namespace, kind, key));
        if let Err(error) = config_operation
            .put(&container_id, slot.as_ref(), spec)
            .await
        {
            self.remove_if_present(&container_id).await?;
            return Err(error.into());
        }
        Ok(ContainerCreated {
            container_id,
            display_name,
        })
    }

    /// Creates a Container under a random name, retrying a name Docker already holds.
    async fn create_unkeyed(
        &self,
        kind: ContainerKind,
        spec: &ResolvedServiceSpec,
        body: ContainerCreateBody,
    ) -> Result<(bollard::models::ContainerCreateResponse, String), Error> {
        let mut attempt = 0;
        loop {
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
                .docker
                .create_container(Some(options), body.clone())
                .await
            {
                Ok(created) => break Ok((created, display_name)),
                Err(Error::Docker(error)) if retry_name_conflict(attempt, &error) => {}
                Err(error) => break Err(error),
            }
        }
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
        // Wait for an in-flight create to persist its spec, as a create does before comparing.
        let mut operation = self.specs.config_operation().await;
        self.matching_creation(&mut operation, machine, namespace, kind, spec, key)
            .await
    }

    /// The Container an earlier create under `key` made. Every caller holds the spec
    /// store's config lock, so no create is between Docker's create and its spec's write.
    async fn matching_creation(
        &self,
        operation: &mut ConfigOperation<'_>,
        machine: &Machine,
        namespace: &Namespace,
        kind: ContainerKind,
        spec: &ResolvedServiceSpec,
        key: &str,
    ) -> Result<Option<ContainerCreated>, Error> {
        let name = &creation_name(&machine.id, namespace, &spec.name, kind, key);
        let existing = self.resolve_slot(operation, &machine.id, name, key).await?;
        let Some(existing) = existing else {
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

    /// Removes a Container a daemon killed between Docker's create and the spec's write
    /// left, so nothing stays bound to its Volumes. Creates hold the config lock across
    /// both, so under it a missing spec is never one still being written.
    pub(super) async fn discard_half_made(
        &self,
        container_id: &ContainerId,
    ) -> Result<bool, Error> {
        let mut operation = self.specs.config_operation().await;
        if !self.half_made(&mut operation, container_id).await? {
            return Ok(false);
        }
        self.remove_if_present(container_id).await?;
        Ok(true)
    }

    /// Whether a daemon killed between Docker's create and the spec's write left
    /// `container_id`: a keyed Container Docker never ran, with no spec. One already gone
    /// ran nothing either. A restore also makes a Container with no spec under its ID, so
    /// its slot's old records are adopted first, and a spec of its Service whose
    /// Container is gone may be its own from before slots were recorded.
    async fn half_made(
        &self,
        operation: &mut ConfigOperation<'_>,
        container_id: &ContainerId,
    ) -> Result<bool, Error> {
        let listed = self.managed_slots().await?;
        let present = listed.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        for (old, new) in operation.adopt_restored(listed).await? {
            eprintln!("adopted restored container {new}, which replaces {old}");
        }
        if self.specs.get(container_id).await?.is_some() {
            return Ok(false);
        }
        let inspected = match self
            .docker
            .client
            .inspect_container(container_id.as_str(), None)
            .await
            .map_err(|error| docker_error(container_id, error))
        {
            Ok(inspected) => inspected,
            Err(Error::ContainerNotFound(_)) => return Ok(true),
            Err(error) => return Err(error),
        };
        let labels = inspected
            .config
            .and_then(|config| config.labels)
            .unwrap_or_default();
        let (true, Some(service_id)) = (
            labels.contains_key(labels::CREATION_KEY)
                && inspected.state.and_then(|state| state.status)
                    == Some(ContainerStateStatusEnum::CREATED),
            labels.get(labels::SERVICE_ID),
        ) else {
            return Ok(false);
        };
        Ok(!self.specs.holds_absent(present, service_id.clone()).await?)
    }

    pub(crate) async fn inspect_slot(
        &self,
        machine_id: &MachineId,
        slot: &SlotKey,
    ) -> Result<Option<ContainerObservation>, Error> {
        let key = slot.creation_key.as_str();
        let name = creation_name(machine_id, &slot.namespace, &slot.service, slot.kind, key);
        let found = self.keyed_container(machine_id, &name, key).await;
        if !matches!(found, Err(Error::SpecNotFound(_))) {
            return found;
        }
        // Boxed, so the rare locked read doesn't grow every slot read's future.
        Box::pin(self.reread_slot(machine_id, &name, key)).await
    }

    async fn reread_slot(
        &self,
        machine_id: &MachineId,
        name: &str,
        key: &str,
    ) -> Result<Option<ContainerObservation>, Error> {
        // A create that ran while this waited may have replaced the Container, so the
        // slot is read again under the lock.
        let mut operation = self.specs.config_operation().await;
        self.resolve_slot(&mut operation, machine_id, name, key)
            .await
    }

    /// The Container in the creation slot `name`, read under the config lock `operation`
    /// holds, so no create is between Docker's create and its spec's write. A Container
    /// with no spec is one of two things. A daemon killed before its spec's write left
    /// it half made, and it is removed, so the slot reads empty and holds nothing that
    /// keeps its Volumes in use; the create that follows makes it again. A restore
    /// made it, and the records it took over are found now, so the slot is read again.
    async fn resolve_slot(
        &self,
        operation: &mut ConfigOperation<'_>,
        machine_id: &MachineId,
        name: &str,
        key: &str,
    ) -> Result<Option<ContainerObservation>, Error> {
        match self.keyed_container(machine_id, name, key).await {
            Err(Error::SpecNotFound(container_id))
                if self.half_made(operation, &container_id).await? =>
            {
                self.remove_if_present(&container_id).await?;
                Ok(None)
            }
            Err(Error::SpecNotFound(_)) => self.keyed_container(machine_id, name, key).await,
            found => found,
        }
    }

    async fn keyed_container(
        &self,
        machine_id: &MachineId,
        name: &str,
        key: &str,
    ) -> Result<Option<ContainerObservation>, Error> {
        let inspected = match self.docker.client.inspect_container(name, None).await {
            Ok(inspected) => inspected,
            Err(error) if classify(&error) == Status::Absent => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if inspected
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .and_then(|labels| labels.get(labels::CREATION_KEY))
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
        self.stranded().await.held.release(container_id);
        self.start_inspected(container_id, &inspected).await
    }

    async fn start_inspected(
        &self,
        container_id: &ContainerId,
        inspected: &ContainerInspectResponse,
    ) -> Result<(), Error> {
        if let Some(network) = lost_network(inspected)
            && let Err(error) = self.rejoin(container_id, network, inspected).await
            // A concurrent start may have attached it first.
            && lost_network(&self.ensure_managed(container_id).await?).is_some()
        {
            return Err(error);
        }
        let result = self
            .docker
            .client
            .start_container(container_id.as_str(), None)
            .await;
        idempotent_lifecycle_result(container_id, result)
    }

    /// Attach a Container to the network a failed start took from it. The Tunnel
    /// Origin's network is checked as at creation, so it never joins one the firewall
    /// doesn't fence.
    async fn rejoin(
        &self,
        container_id: &ContainerId,
        network: &str,
        inspected: &ContainerInspectResponse,
    ) -> Result<(), Error> {
        let joined = if network == DOCKER_NETWORK_NAME {
            let ipv6 = inspected
                .config
                .as_ref()
                .and_then(|config| config.labels.as_ref())
                .is_some_and(super::takes_bridge_ipv6);
            self.docker.join_bridge(container_id, ipv6).await
        } else {
            self.docker.ensure_tunnel_network().await?;
            self.docker
                .client
                .connect_network(
                    network,
                    NetworkConnectRequest {
                        container: container_id.to_string(),
                        endpoint_config: None,
                    },
                )
                .await
        };
        joined.map_err(|error| docker_error(container_id, error))
    }

    /// Carry out one Ingress door recovery, unless a request stopped its Container or
    /// is about to, or a container was created since `creates` was read or is being
    /// created. Recovery only starts what exists; it never creates.
    pub(super) async fn start_stranded(&self, recovery: IngressRecovery, creates: u64) {
        // Under the doors lock so a route placing the doors never races it.
        let _doors = self.door_lifecycle().await;
        let container_id = *recovery.target();
        let gate = self
            .stranded()
            .await
            .gates
            .entry(container_id)
            .or_default()
            .clone();
        let Ok(_gate) = gate.try_lock() else {
            return;
        };
        if self.stranded().await.held.contains(&container_id) || self.is_withdrawn(&container_id) {
            return;
        }
        let result = self.recover_door(&recovery, creates).await;
        let mut stranded = self.stranded().await;
        match result {
            Ok(acted) => {
                if acted {
                    tracing::info!(?recovery, "{} an Ingress door", recovery.verb());
                }
                stranded.reported = None;
            }
            Err(error) if stranded.reported != Some(container_id) => {
                eprintln!(
                    "Ingress door {container_id} is still not {}: {error}",
                    recovery.verb(),
                    error = ployz_core::error_chain::inline(&error),
                );
                stranded.reported = Some(container_id);
            }
            Err(_) => {}
        }
    }

    async fn recover_door(&self, recovery: &IngressRecovery, creates: u64) -> Result<bool, Error> {
        match recovery {
            IngressRecovery::StartHolder(container_id)
            | IngressRecovery::StartCaddy {
                holder: None,
                caddy: container_id,
            } => {
                let inspected = self.ensure_managed(container_id).await?;
                // A newer door may be about to take the ports; the next scan decides.
                if !failed_start(&inspected) || !self.docker.created_nothing_since(creates) {
                    return Ok(false);
                }
                self.start_inspected(container_id, &inspected).await?;
                Ok(true)
            }
            IngressRecovery::StartCaddy {
                holder: Some(holder),
                caddy,
            } => {
                let inspected = self.ensure_managed(caddy).await?;
                if !failed_start(&inspected)
                    || !is_running(&self.ensure_managed(holder).await?)
                    || !self.docker.created_nothing_since(creates)
                {
                    return Ok(false);
                }
                self.start_inspected(caddy, &inspected).await?;
                Ok(true)
            }
            IngressRecovery::Rejoin { owner, joined } => {
                let owner_now = self.ensure_managed(owner).await?;
                let joined_now = self.ensure_managed(joined).await?;
                let stale = match (doors::netns_of(&owner_now), doors::netns_of(&joined_now)) {
                    (Some(owner_ns), Some(joined_ns)) => owner_ns != joined_ns,
                    _ => false,
                };
                if !stale
                    || doors::joined_to(&joined_now) != Some(*owner)
                    || !self.docker.created_nothing_since(creates)
                {
                    return Ok(false);
                }
                // A stop here is no request's, so it holds nothing: the start that
                // follows is the same repair.
                let result = self
                    .docker
                    .client
                    .stop_container(joined.as_str(), None)
                    .await;
                idempotent_lifecycle_result(joined, result)?;
                let inspected = self.ensure_managed(joined).await?;
                self.start_inspected(joined, &inspected).await?;
                Ok(true)
            }
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
        self.stop_if(container_id, signal, grace_period_seconds, async {
            Ok::<_, Error>(true)
        })
        .await
        .map(drop)
    }

    /// Stop the Container only if `still_wanted` holds after it is inspected and
    /// any Ingress Proxy repair of it has finished, right before the stop request.
    /// A refusal wins over a failed inspect.
    pub async fn stop_if<E: From<Error>>(
        &self,
        container_id: &ContainerId,
        signal: Option<&str>,
        grace_period_seconds: Option<i32>,
        still_wanted: impl std::future::Future<Output = Result<bool, E>>,
    ) -> Result<bool, E> {
        let managed = self.ensure_managed(container_id).await;
        let hold = if managed.is_ok() {
            let (hold, gate) = {
                let stranded = self.stranded().await;
                let hold = stranded.held.hold(*container_id);
                (hold, stranded.gates.get(container_id).cloned())
            };
            if let Some(gate) = gate {
                drop(gate.lock().await);
            }
            Some(hold)
        } else {
            None
        };
        let wanted = still_wanted.await;
        if !matches!(wanted, Ok(true)) {
            return wanted;
        }
        if let Some(hold) = hold {
            hold.keep();
        }
        managed?;
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
        idempotent_lifecycle_result(container_id, result)?;
        Ok(true)
    }

    pub async fn remove(
        &self,
        container_id: &ContainerId,
        remove_volumes: bool,
        force: bool,
    ) -> Result<(), Error> {
        let mut config_operation = self.specs.config_operation().await;
        self.remove_locked(&mut config_operation, container_id, remove_volumes, force)
            .await
    }

    pub(super) async fn remove_locked(
        &self,
        config_operation: &mut super::spec_store::ConfigOperation<'_>,
        container_id: &ContainerId,
        remove_volumes: bool,
        force: bool,
    ) -> Result<(), Error> {
        self.retire(
            config_operation,
            container_id,
            How::Asked {
                remove_volumes,
                force,
            },
        )
        .await
    }

    pub async fn remove_all_managed(&self) -> Result<(), Error> {
        self.departing
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let mut config_operation = self.specs.config_operation().await;
        // A Caddy in a holder's network goes before the holder, and a connector before
        // its Tunnel Origin, so no Container is ever left in a removed Container's
        // network.
        let mut managed = self.docker.managed_containers().await?;
        managed.sort_by_key(|(_, labels)| {
            ServiceLabels::parse(labels)
                .is_ok_and(|managed| Door::of(&managed.identity).is_some_and(Door::owns_netns))
        });
        for (container_id, _) in managed {
            self.retire(&mut config_operation, &container_id, How::Departing)
                .await?;
        }
        Ok(())
    }

    /// Takes a Container out of Docker, then out of the Machine's records, then out of
    /// the daemon's memory, in that order. A crash after the first step leaves a record
    /// with no Container, which the next inventory settles; the reverse order would
    /// leave a Container no record describes. A Container Docker no longer has is
    /// settled only if a record of it remained, so a name that was never ours still
    /// answers not found.
    async fn retire(
        &self,
        config_operation: &mut super::spec_store::ConfigOperation<'_>,
        container_id: &ContainerId,
        how: How,
    ) -> Result<(), Error> {
        let removed = match how {
            How::Asked {
                remove_volumes,
                force,
            } => match self.ensure_managed(container_id).await {
                Ok(_) => {
                    let options = RemoveContainerOptionsBuilder::default()
                        .v(remove_volumes)
                        .force(force)
                        .build();
                    self.docker
                        .client
                        .remove_container(container_id.as_str(), Some(options))
                        .await
                        .map_err(|error| docker_error(container_id, error))
                }
                Err(error) => Err(error),
            },
            How::Departing => {
                self.stop_if_running(container_id).await?;
                self.remove_if_present(container_id).await
            }
        };
        match removed {
            Ok(()) => {
                config_operation.remove(container_id).await?;
            }
            Err(Error::ContainerNotFound(_)) if config_operation.remove(container_id).await? => {}
            Err(error) => return Err(error),
        }
        self.forget(container_id).await;
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
        ServiceLabels::parse(labels)?;
        self.specs
            .get(container_id)
            .await?
            .ok_or_else(|| Error::SpecNotFound(*container_id))?;
        Ok(inspected)
    }

    /// Stops `container_id`. A Container already stopped or gone is success.
    async fn stop_if_running(&self, container_id: &ContainerId) -> Result<(), Error> {
        match self
            .docker
            .client
            .stop_container(container_id.as_str(), None)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => match classify(&error) {
                Status::Unchanged | Status::Absent => Ok(()),
                Status::Conflict | Status::Other => Err(error.into()),
            },
        }
    }

    /// Removes `container_id` with its anonymous volumes, killing it if it runs. A
    /// Container already gone is success.
    async fn remove_if_present(&self, container_id: &ContainerId) -> Result<(), Error> {
        let options = RemoveContainerOptionsBuilder::default()
            .v(true)
            .force(true)
            .build();
        match self
            .docker
            .client
            .remove_container(container_id.as_str(), Some(options))
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => match classify(&error) {
                Status::Absent => Ok(()),
                Status::Conflict | Status::Unchanged | Status::Other => Err(error.into()),
            },
        }
    }
}

impl super::LocalDocker {
    /// Connects a Container that lost its bridge endpoint at the next free address,
    /// holding the gate until Docker records it so no create takes the same one.
    async fn join_bridge(&self, container_id: &ContainerId, ipv6: bool) -> Result<(), DockerError> {
        let mut last = self.endpoint_creates.lock().await;
        let address = match self.bridge().await {
            Ok(bridge) => bridge
                .subnet
                .and_then(|subnet| super::bridge_address::next(subnet, &bridge.taken, *last)),
            Err(_) => None,
        };
        if address.is_some() {
            *last = address;
        }
        self.client
            .connect_network(
                DOCKER_NETWORK_NAME,
                NetworkConnectRequest {
                    container: container_id.to_string(),
                    endpoint_config: Some(super::bridge_address::endpoint(ipv6, address)),
                },
            )
            .await
    }

    pub(super) async fn create_container(
        &self,
        options: Option<bollard::query_parameters::CreateContainerOptions>,
        mut body: ContainerCreateBody,
    ) -> Result<bollard::models::ContainerCreateResponse, Error> {
        let mut last = self.endpoint_creates.lock().await;
        let bridge = self.bridge().await?;
        let on_bridge = body
            .host_config
            .as_ref()
            .and_then(|host| host.network_mode.as_deref())
            == Some(DOCKER_NETWORK_NAME);
        // A Container in another's network takes no endpoint of its own.
        if on_bridge && bridge.capacity.free_endpoints() == 0 {
            return Err(Error::EndpointCapacity);
        }
        if on_bridge && let Some(subnet) = bridge.subnet {
            let address = super::bridge_address::next(subnet, &bridge.taken, *last)
                .ok_or(Error::EndpointCapacity)?;
            let endpoints = body
                .networking_config
                .get_or_insert_default()
                .endpoints_config
                .get_or_insert_default();
            let endpoint = endpoints.entry(DOCKER_NETWORK_NAME.to_owned()).or_default();
            endpoint.ipam_config =
                super::bridge_address::endpoint(false, Some(address)).ipam_config;
            *last = Some(address);
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
        Ok(()) => Ok(()),
        Err(error) if classify(&error) == Status::Unchanged => Ok(()),
        Err(error) => Err(docker_error(container_id, error)),
    }
}

fn retry_name_conflict(attempt: u8, error: &bollard::errors::Error) -> bool {
    attempt < CONTAINER_NAME_ATTEMPTS && classify(error) == Status::Conflict
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
    service: &ServiceName,
    kind: ContainerKind,
    key: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let scope =
        serde_json::to_vec(&(machine, namespace, kind, key)).expect("creation scope serializes");
    let digest = hex::encode(Sha256::digest(scope));
    format!("{service}-{}", &digest[..12])
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
        join_approval: None,
        build_concurrency: None,
    }
}

/// Docker left this container created and recorded why its start failed.
/// Docker clears `State.Error` when a start succeeds, so a container at rest with one
/// recorded is one whose last start failed: its first, or the one at boot that
/// leaves it exited with no retry.
pub(super) fn failed_start(inspected: &ContainerInspectResponse) -> bool {
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

pub(super) fn is_running(inspected: &ContainerInspectResponse) -> bool {
    inspected.state.as_ref().and_then(|state| state.status)
        == Some(ContainerStateStatusEnum::RUNNING)
}

/// Docker 29 empties a container's networks when its start fails, and a later start
/// then runs it with no network and no published ports. A Container in another's
/// network has none to lose. Answers the network a Container on the `ployz` bridge
/// or a Tunnel Origin lost.
fn lost_network(inspected: &ContainerInspectResponse) -> Option<&'static str> {
    let network = match inspected
        .host_config
        .as_ref()
        .and_then(|host| host.network_mode.as_deref())
    {
        Some(DOCKER_NETWORK_NAME) => DOCKER_NETWORK_NAME,
        Some(TUNNEL_NETWORK_NAME) => TUNNEL_NETWORK_NAME,
        _ => return None,
    };
    let attached = inspected
        .network_settings
        .as_ref()
        .and_then(|settings| settings.networks.as_ref())
        .is_some_and(|networks| networks.contains_key(network));
    (!attached).then_some(network)
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
        assert_eq!(
            lost_network(&inspected("ployz", serde_json::json!({}))),
            Some("ployz")
        );
        assert_eq!(
            lost_network(&inspected("ployz", serde_json::json!({ "bridge": {} }))),
            Some("ployz")
        );
        assert_eq!(
            lost_network(&inspected("ployz-tunnel", serde_json::json!({}))),
            Some("ployz-tunnel")
        );
    }

    #[test]
    fn an_attached_or_differently_networked_container_is_left_alone() {
        assert_eq!(
            lost_network(&inspected("ployz", serde_json::json!({ "ployz": {} }))),
            None
        );
        assert_eq!(
            lost_network(&inspected(
                "ployz-tunnel",
                serde_json::json!({ "ployz-tunnel": {} })
            )),
            None
        );
        assert_eq!(
            lost_network(&inspected("host", serde_json::json!({}))),
            None
        );
    }

    #[tokio::test]
    async fn a_container_joins_the_bridge_at_a_static_address() {
        let (runtime, fake) = fake_runtime_with(FakeDocker {
            create_barrier: Some(std::sync::Arc::new(tokio::sync::Barrier::new(1))),
            ..FakeDocker::default()
        })
        .await;
        let namespace = Namespace::parse("app").unwrap();
        let spec = spec_with_sources(Vec::new());
        runtime
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
        let body = fake
            .request_bodies
            .lock()
            .unwrap()
            .iter()
            .find(|(path, _)| path.ends_with("/containers/create"))
            .unwrap()
            .1
            .clone();
        assert_eq!(
            body.pointer("/NetworkingConfig/EndpointsConfig/ployz/IPAMConfig/IPv4Address"),
            Some(&serde_json::json!("10.210.0.2")),
        );
    }

    async fn stamped_labels(
        kind: ContainerKind,
        creation_key: Option<&str>,
        deployment_id: Option<&ployz_core::DeploymentLogId>,
        generation: Gen,
    ) -> serde_json::Value {
        let namespace = Namespace::parse("app").unwrap();
        let spec = spec_with_sources(Vec::new());
        let (runtime, fake) = fake_runtime_with(FakeDocker {
            create_barrier: Some(std::sync::Arc::new(tokio::sync::Barrier::new(1))),
            ..FakeDocker::default()
        })
        .await;
        runtime
            .create_with_admission::<_, _, Error>(
                &machine(),
                ContainerRequest {
                    creation_key,
                    deployment_id,
                    r#gen: generation,
                    ..container_request(kind, &namespace, &spec, std::future::ready(None))
                },
            )
            .await
            .unwrap();
        let body = fake
            .request_bodies
            .lock()
            .unwrap()
            .iter()
            .find(|(path, _)| path.ends_with("/containers/create"))
            .unwrap()
            .1
            .clone();
        body.get("Labels").unwrap().clone()
    }

    #[tokio::test]
    async fn a_created_container_carries_exactly_the_stamped_labels() {
        let deployment = ployz_core::DeploymentLogId::parse("deploy-7").unwrap();
        let without_service_id = |mut labels: serde_json::Value| {
            let labels = labels.as_object_mut().unwrap();
            let id = labels.remove("ployz.service.id").unwrap();
            ployz_core::ServiceId::parse(id.as_str().unwrap()).unwrap();
            serde_json::Value::Object(labels.clone())
        };
        let service = stamped_labels(
            ContainerKind::ServiceContainer,
            Some("plan:0"),
            Some(&deployment),
            Gen::new(3),
        )
        .await;
        assert_eq!(
            without_service_id(service),
            serde_json::json!({
                "ployz.managed": "",
                "ployz.namespace": "app",
                "ployz.service.name": "api",
                "ployz.network.ipv6": "true",
                "ployz.deployment.id": "deploy-7",
                "ployz.creation.key": "plan:0",
                "ployz.gen": "3",
            })
        );
        let hook = stamped_labels(ContainerKind::PreDeployHook, None, None, Gen::FIRST).await;
        assert_eq!(
            without_service_id(hook),
            serde_json::json!({
                "ployz.managed": "",
                "ployz.namespace": "app",
                "ployz.service.name": "api",
                "ployz.service.hook": "pre-deploy",
                "ployz.network.ipv6": "true",
                "ployz.gen": Gen::FIRST.to_string(),
            })
        );
    }

    #[tokio::test]
    async fn a_keyed_container_is_named_after_its_service() {
        let containers =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
        let (runtime, _fake) = fake_runtime_with(FakeDocker {
            named_containers: Some(containers.clone()),
            ..Default::default()
        })
        .await;
        let machine = machine();
        let namespace = Namespace::parse("app").unwrap();
        let spec = spec_with_sources(Vec::new());
        let created = runtime
            .create_with_admission::<_, _, Error>(
                &machine,
                ContainerRequest {
                    creation_key: Some("plan:0"),
                    ..container_request(
                        ContainerKind::ServiceContainer,
                        &namespace,
                        &spec,
                        std::future::ready(None),
                    )
                },
            )
            .await
            .unwrap();
        let names = containers
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let [name] = names.as_slice() else {
            panic!("one keyed create makes one Container: {names:?}");
        };
        let suffix = name
            .strip_prefix("api-")
            .unwrap_or_else(|| panic!("{name} starts with the Service name"));
        assert!(
            suffix.len() == 12 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{name} ends in 12 hex digits"
        );
        let slot = SlotKey {
            namespace,
            service: spec.name.clone(),
            kind: ContainerKind::ServiceContainer,
            creation_key: ployz_core::CreationKey::try_from("plan:0".to_owned()).unwrap(),
        };
        let found = runtime.inspect_slot(&machine.id, &slot).await.unwrap();
        assert_eq!(
            found.map(|container| container.container_id),
            Some(created.container_id),
            "the slot finds the Container it named"
        );
    }

    /// A keyed create whose spec never reached `machine.db`, as a daemon killed between
    /// Docker's create and the spec's write leaves it, and the request to create it again.
    /// With `gone`, a spec of the same Service first holds that slot for a Container Docker
    /// no longer lists, as a `docker rm` or a crash after Docker's remove leaves it.
    async fn half_made(
        status: &str,
        gone: Option<&'static str>,
    ) -> (
        ContainerRuntime,
        std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>>,
        ContainerId,
        Result<ContainerCreated, Error>,
    ) {
        let containers =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
        let (runtime, _fake) = fake_runtime_with(FakeDocker {
            named_containers: Some(containers.clone()),
            list_named: true,
            ..Default::default()
        })
        .await;
        let machine = machine();
        let namespace = Namespace::parse("app").unwrap();
        let spec = spec_with_sources(Vec::new());
        if let Some(key) = gone {
            let slot = super::super::spec_store::CreationSlot::new(
                &namespace,
                ContainerKind::ServiceContainer,
                key,
            );
            runtime
                .specs
                .config_operation()
                .await
                .put(
                    &ContainerId::parse("f".repeat(64)).unwrap(),
                    Some(&slot),
                    &spec,
                )
                .await
                .unwrap();
        }
        let request = || ContainerRequest {
            creation_key: Some("plan:0"),
            ..container_request(
                ContainerKind::ServiceContainer,
                &namespace,
                &spec,
                std::future::ready(None),
            )
        };
        let first = runtime
            .create_with_admission::<_, _, Error>(&machine, request())
            .await
            .unwrap();
        runtime
            .specs
            .config_operation()
            .await
            .remove(&first.container_id)
            .await
            .unwrap();
        for container in containers.lock().unwrap().values_mut() {
            container["State"] = serde_json::json!({ "Status": status });
        }
        let again = runtime
            .create_with_admission::<_, _, Error>(&machine, request())
            .await;
        (runtime, containers, first.container_id, again)
    }

    #[tokio::test]
    async fn a_create_resent_after_a_crash_left_its_container_without_a_spec_makes_it_again() {
        let (runtime, containers, lost, again) = half_made("created", None).await;
        let made = again.expect("the resend creates the Container again");
        assert_ne!(made.container_id, lost);
        let ids = containers
            .lock()
            .unwrap()
            .values()
            .map(|container| container["Id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![made.container_id.to_string()],
            "the half-made Container is gone, so nothing is left bound to its Volumes"
        );
        assert!(
            runtime
                .specs
                .get(&made.container_id)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_keyed_spec_whose_container_is_gone_does_not_keep_a_half_made_container() {
        let (_runtime, containers, lost, again) = half_made("created", Some("plan:old")).await;
        let made = again.expect("the resend creates the Container again");
        assert_ne!(made.container_id, lost);
        let ids = containers
            .lock()
            .unwrap()
            .values()
            .map(|container| container["Id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![made.container_id.to_string()],
            "the half-made Container is gone, so nothing is left bound to its Volumes"
        );
    }

    /// A Container the create under `key` left, with no spec and Docker state `status`.
    async fn specless(
        key: Option<&'static str>,
        status: &str,
    ) -> (
        ContainerRuntime,
        std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>>,
        Machine,
        ResolvedServiceSpec,
        ContainerId,
        FakeDocker,
    ) {
        let containers =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
        let (runtime, fake) = fake_runtime_with(FakeDocker {
            named_containers: Some(containers.clone()),
            list_named: true,
            ..Default::default()
        })
        .await;
        let machine = machine();
        let namespace = Namespace::parse("app").unwrap();
        let spec = spec_with_sources(Vec::new());
        let created = runtime
            .create_with_admission::<_, _, Error>(
                &machine,
                ContainerRequest {
                    creation_key: key,
                    ..container_request(
                        ContainerKind::ServiceContainer,
                        &namespace,
                        &spec,
                        std::future::ready(None),
                    )
                },
            )
            .await
            .unwrap();
        runtime
            .specs
            .config_operation()
            .await
            .remove(&created.container_id)
            .await
            .unwrap();
        for container in containers.lock().unwrap().values_mut() {
            container["State"] = serde_json::json!({ "Status": status });
        }
        (
            runtime,
            containers,
            machine,
            spec,
            created.container_id,
            fake,
        )
    }

    #[tokio::test]
    async fn reading_a_slot_a_crash_left_without_its_spec_empties_it() {
        let (runtime, containers, machine, spec, _, _) = specless(Some("plan:0"), "created").await;
        let slot = SlotKey {
            namespace: Namespace::parse("app").unwrap(),
            service: spec.name.clone(),
            kind: ContainerKind::ServiceContainer,
            creation_key: ployz_core::CreationKey::try_from("plan:0".to_owned()).unwrap(),
        };

        let found = runtime.inspect_slot(&machine.id, &slot).await;

        assert!(matches!(found, Ok(None)), "{found:?}");
        assert!(
            containers.lock().unwrap().is_empty(),
            "a slot read as empty holds no Container that keeps its Volumes in use"
        );
    }

    #[tokio::test]
    async fn a_slot_read_waiting_out_a_create_that_replaced_its_container_reads_the_replacement() {
        let (runtime, containers, machine, spec, lost, fake) =
            specless(Some("plan:0"), "created").await;
        let namespace = Namespace::parse("app").unwrap();
        let slot = SlotKey {
            namespace: namespace.clone(),
            service: spec.name.clone(),
            kind: ContainerKind::ServiceContainer,
            creation_key: ployz_core::CreationKey::try_from("plan:0".to_owned()).unwrap(),
        };
        let answered = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        *fake.answered_hold.lock().unwrap() = Some(answered.clone());
        let mut create = runtime.specs.config_operation().await;
        let read = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.inspect_slot(&machine.id, &slot).await }
        });
        // The read finds the half-made Container by name, then inspects it by ID. Once that
        // answer is read, the create the read then waits on removes the Container and
        // makes another.
        answered.wait().await;
        let by_id = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        *fake.answered_hold.lock().unwrap() = Some(by_id.clone());
        answered.wait().await;
        by_id.wait().await;
        let replacement = ContainerId::parse("e".repeat(64)).unwrap();
        for container in containers.lock().unwrap().values_mut() {
            container["Id"] = replacement.as_str().into();
            container["State"] = serde_json::json!({ "Status": "running" });
        }
        by_id.wait().await;
        let creation = super::super::spec_store::CreationSlot::new(
            &namespace,
            ContainerKind::ServiceContainer,
            "plan:0",
        );
        create
            .put(&replacement, Some(&creation), &spec)
            .await
            .unwrap();
        drop(create);

        let found = read.await.unwrap().unwrap();

        assert_ne!(replacement, lost);
        assert_eq!(found.map(|found| found.container_id), Some(replacement));
    }

    #[tokio::test]
    async fn a_slot_holding_a_started_container_without_its_spec_is_still_reported() {
        let (runtime, containers, machine, spec, id, _) = specless(Some("plan:0"), "running").await;
        let slot = SlotKey {
            namespace: Namespace::parse("app").unwrap(),
            service: spec.name.clone(),
            kind: ContainerKind::ServiceContainer,
            creation_key: ployz_core::CreationKey::try_from("plan:0".to_owned()).unwrap(),
        };

        let found = runtime.inspect_slot(&machine.id, &slot).await;

        assert!(
            matches!(found, Err(Error::SpecNotFound(found)) if found == id),
            "{found:?}"
        );
        assert_eq!(containers.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_started_container_without_its_spec_answers_not_found_marked_as_held() {
        let (runtime, _, machine, _, id, _) = specless(Some("plan:0"), "running").await;

        let error = runtime
            .inspect_managed(&id, &machine.id)
            .await
            .expect_err("Docker has the Container, but no spec describes it");

        assert!(
            matches!(error, Error::SpecNotFound(found) if found == id),
            "{error:?}"
        );
        let answer = ployz_core::RpcError::from(&error);
        assert_eq!(answer.code, ployz_core::RpcErrorCode::NotFound);
        assert_eq!(
            answer.details,
            serde_json::json!({ "reason": "no_resolved_spec" })
        );
    }

    #[tokio::test]
    async fn an_unstarted_or_unkeyed_container_without_its_spec_is_marked_as_held_too() {
        for (key, status) in [(Some("plan:0"), "created"), (None, "running")] {
            let (runtime, _, machine, _, id, _) = specless(key, status).await;

            let error = runtime
                .inspect_managed(&id, &machine.id)
                .await
                .expect_err("Docker has the Container, but no spec describes it");

            assert!(
                matches!(error, Error::SpecNotFound(found) if found == id),
                "{error:?}"
            );
            let answer = ployz_core::RpcError::from(&error);
            assert_eq!(answer.code, ployz_core::RpcErrorCode::NotFound);
            assert_eq!(
                answer.details,
                serde_json::json!({ "reason": "no_resolved_spec" }),
                "a {status} Container keyed {key:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_container_docker_no_longer_has_answers_not_found_unmarked() {
        let (runtime, containers, machine, _, id, _) = specless(Some("plan:0"), "running").await;
        containers.lock().unwrap().clear();

        let error = runtime
            .inspect_managed(&id, &machine.id)
            .await
            .expect_err("Docker no longer has the Container");

        assert!(
            matches!(error, Error::ContainerNotFound(found) if found == id),
            "{error:?}"
        );
        let answer = ployz_core::RpcError::from(&error);
        assert_eq!(answer.code, ployz_core::RpcErrorCode::NotFound);
        assert_eq!(answer.details, serde_json::Value::Null);
    }

    #[tokio::test]
    async fn listing_removes_a_container_a_crash_left_without_its_spec_and_keeps_a_started_one() {
        // An abandoned deploy: no create under this key runs again. A restore can make
        // an unkeyed Container again with no spec, and only keyed ones are adopted.
        for (key, status, kept) in [
            (Some("plan:0"), "created", 0),
            (Some("plan:0"), "running", 1),
            (None, "created", 1),
        ] {
            let (runtime, containers, machine, _, _, _) = specless(key, status).await;
            assert!(runtime.list_managed(&machine.id).await.unwrap().is_empty());
            assert_eq!(
                containers.lock().unwrap().len(),
                kept,
                "a {status} Container keyed {key:?} without its spec"
            );
        }
    }

    #[tokio::test]
    async fn a_running_container_without_a_spec_is_never_removed_to_create_again() {
        let (_runtime, containers, lost, again) = half_made("running", None).await;
        assert!(
            matches!(again, Err(Error::SpecNotFound(id)) if id == lost),
            "{again:?}"
        );
        assert_eq!(containers.lock().unwrap().len(), 1);
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

    fn start_legacy_caddy(caddy: ContainerId) -> IngressRecovery {
        IngressRecovery::StartCaddy {
            holder: None,
            caddy,
        }
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
            .start_stranded(start_legacy_caddy(container_id), runtime.docker.creates())
            .await;
        assert!(started(&fake), "a failed first start is retried");

        runtime.stop(&container_id, None, None).await.unwrap();
        fake.requests.lock().unwrap().clear();
        runtime
            .start_stranded(start_legacy_caddy(container_id), runtime.docker.creates())
            .await;
        assert!(!started(&fake), "a stopped ingress stays stopped");

        runtime.start(&container_id).await.unwrap();
        fake.requests.lock().unwrap().clear();
        runtime
            .start_stranded(start_legacy_caddy(container_id), runtime.docker.creates())
            .await;
        assert!(started(&fake), "a requested start lifts the stop");
    }

    #[tokio::test]
    async fn a_stop_cancelled_before_it_proceeds_releases_its_hold() {
        let (runtime, _fake, container_id) = stranded(PORT_TAKEN).await;
        let (asked, was_asked) = tokio::sync::oneshot::channel();
        let stopping = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                runtime
                    .stop_if(&container_id, None, None, async move {
                        let _ = asked.send(());
                        std::future::pending::<Result<bool, Error>>().await
                    })
                    .await
            }
        });
        was_asked.await.unwrap();
        assert!(runtime.stranded().await.held.contains(&container_id));

        stopping.abort();
        assert!(stopping.await.unwrap_err().is_cancelled());
        assert!(
            !runtime.stranded().await.held.contains(&container_id),
            "a dropped stop must not keep the ingress from its retry"
        );
    }

    #[tokio::test]
    async fn a_stop_deciding_when_its_container_is_forgotten_keeps_no_hold() {
        let (runtime, _fake, container_id) = stranded(PORT_TAKEN).await;
        let (asked, was_asked) = tokio::sync::oneshot::channel();
        let (decide, decided) = tokio::sync::oneshot::channel::<()>();
        let stopping = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                runtime
                    .stop_if(&container_id, None, None, async move {
                        let _ = asked.send(());
                        let _ = decided.await;
                        Ok::<_, Error>(true)
                    })
                    .await
            }
        });
        was_asked.await.unwrap();

        runtime.forget(&container_id).await;
        decide.send(()).unwrap();
        let _ = stopping.await.unwrap();
        assert!(
            !runtime.stranded().await.held.contains(&container_id),
            "a forgotten container must not stay held"
        );
    }

    #[tokio::test]
    async fn a_hold_forget_voided_never_touches_a_later_hold() {
        let (_runtime, _fake, container_id) = stranded(PORT_TAKEN).await;
        let holds = super::super::observe::Holds::default();

        let voided = holds.hold(container_id);
        holds.forget(&container_id);
        let later = holds.hold(container_id);
        voided.keep();
        drop(later);
        assert!(
            !holds.contains(&container_id),
            "a voided keep must not hold"
        );

        let voided = holds.hold(container_id);
        holds.forget(&container_id);
        let later = holds.hold(container_id);
        drop(voided);
        assert!(holds.contains(&container_id), "a voided drop must not lift");
        later.keep();
        assert!(holds.contains(&container_id), "the later stop is kept");
    }

    #[tokio::test]
    async fn a_container_stays_withdrawn_until_its_last_withdrawal_drops() {
        let (runtime, _fake, container_id) = stranded(PORT_TAKEN).await;
        for first_released_first in [true, false] {
            let mut observer = runtime.stopping.subscribe();
            let first = runtime.withdraw(container_id);
            let second = runtime.withdraw(container_id);
            assert!(observer.has_changed().unwrap());
            observer.mark_unchanged();

            let (released, held) = if first_released_first {
                (first, second)
            } else {
                (second, first)
            };
            drop(released);
            assert!(runtime.is_withdrawn(&container_id));
            assert!(!observer.has_changed().unwrap());

            drop(held);
            assert!(!runtime.is_withdrawn(&container_id));
            assert!(
                observer.has_changed().unwrap(),
                "the last release wakes the observer"
            );
        }
    }

    #[tokio::test]
    async fn an_ingress_about_to_be_stopped_is_not_started() {
        let (runtime, fake, container_id) = stranded(PORT_TAKEN).await;

        let _withdrawal = runtime.withdraw(container_id);
        runtime
            .start_stranded(start_legacy_caddy(container_id), runtime.docker.creates())
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
                    .start_stranded(start_legacy_caddy(container_id), runtime.docker.creates())
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
            .start_stranded(start_legacy_caddy(container_id), runtime.docker.creates())
            .await;
        assert!(!started(&fake), "the stop holds");
    }

    #[tokio::test]
    async fn a_stop_refused_after_a_raced_repair_neither_stops_nor_holds_the_ingress() {
        use std::sync::atomic::{AtomicBool, Ordering};
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
                    .start_stranded(start_legacy_caddy(container_id), runtime.docker.creates())
                    .await
            }
        });
        barrier.wait().await;
        let still_wanted = std::sync::Arc::new(AtomicBool::new(true));
        let stopping = tokio::spawn({
            let runtime = runtime.clone();
            let still_wanted = still_wanted.clone();
            async move {
                runtime
                    .stop_if(&container_id, None, None, async move {
                        Ok::<_, Error>(still_wanted.load(Ordering::SeqCst))
                    })
                    .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !runtime.stranded().await.held.contains(&container_id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        still_wanted.store(false, Ordering::SeqCst);
        barrier.wait().await;
        starting.await.unwrap();

        assert!(!stopping.await.unwrap().unwrap());
        assert!(
            !fake
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(_, path)| path.ends_with("/stop"))
        );
        assert!(!runtime.stranded().await.held.contains(&container_id));
    }

    #[tokio::test]
    async fn a_create_in_flight_when_the_scan_began_voids_the_start() {
        let (runtime, fake, container_id) = stranded(PORT_TAKEN).await;
        let creating = super::super::Creating::new(&runtime.docker.creates);
        let creates = runtime.docker.creates();

        runtime
            .start_stranded(start_legacy_caddy(container_id), creates)
            .await;
        assert!(!started(&fake), "a newer ingress may be appearing");

        drop(creating);
        runtime
            .start_stranded(start_legacy_caddy(container_id), creates)
            .await;
        assert!(!started(&fake), "it appeared after the scan began");

        runtime
            .start_stranded(start_legacy_caddy(container_id), runtime.docker.creates())
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

        runtime
            .start_stranded(start_legacy_caddy(container_id), creates)
            .await;

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
                    .start_stranded(start_legacy_caddy(container_id), runtime.docker.creates())
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
            .start_stranded(start_legacy_caddy(container_id), runtime.docker.creates())
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
            .start_stranded(start_legacy_caddy(container_id), runtime.docker.creates())
            .await;

        assert!(started(&fake));
    }

    #[tokio::test]
    async fn an_ingress_that_exited_on_its_own_is_left_alone() {
        let (runtime, fake, container_id) =
            stranded_with("exited", "", FakeDocker::default()).await;

        runtime
            .start_stranded(start_legacy_caddy(container_id), runtime.docker.creates())
            .await;

        assert!(!started(&fake));
    }

    #[tokio::test]
    async fn a_tunnel_origin_whose_failed_start_dropped_its_network_rejoins_it_before_starting() {
        let (runtime, fake, container_id) = stranded(PORT_TAKEN).await;
        *fake
            .existing_container
            .lock()
            .unwrap()
            .as_mut()
            .and_then(|container| container.pointer_mut("/HostConfig/NetworkMode"))
            .unwrap() = TUNNEL_NETWORK_NAME.into();

        runtime.start(&container_id).await.unwrap();

        let posts: Vec<String> = fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _)| method == axum::http::Method::POST)
            .map(|(_, path)| path.clone())
            .collect();
        let joined = posts
            .iter()
            .position(|path| path.ends_with(&format!("/networks/{TUNNEL_NETWORK_NAME}/connect")));
        let started = posts.iter().position(|path| path.ends_with("/start"));
        assert!(
            joined.is_some() && joined < started,
            "it must be back on its fenced bridge before it runs: {posts:?}"
        );
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

    #[tokio::test]
    async fn removing_a_container_drops_its_record_after_docker_removed_it() {
        let (runtime, fake, container_id) =
            crate::docker::test_support::runtime_with_running_container().await;

        runtime.remove(&container_id, false, true).await.unwrap();

        assert!(
            fake.requests
                .lock()
                .unwrap()
                .iter()
                .any(|(method, path)| method == axum::http::Method::DELETE
                    && path.contains(container_id.as_str())),
            "Docker was asked to remove it"
        );
        assert!(runtime.specs.get(&container_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn removing_a_container_docker_lost_succeeds_only_when_a_record_remained() {
        let (runtime, fake, container_id) =
            crate::docker::test_support::runtime_with_running_container().await;
        fake.existing_container.lock().unwrap().take();

        runtime.remove(&container_id, false, true).await.unwrap();
        assert!(runtime.specs.get(&container_id).await.unwrap().is_none());

        assert!(matches!(
            runtime.remove(&container_id, false, true).await,
            Err(Error::ContainerNotFound(id)) if id == container_id
        ));
    }

    #[tokio::test]
    async fn leaving_stops_then_removes_each_managed_container_and_its_record() {
        let containers =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
        let (runtime, fake) = fake_runtime_with(FakeDocker {
            named_containers: Some(containers.clone()),
            list_named: true,
            ..Default::default()
        })
        .await;
        let namespace = Namespace::parse("app").unwrap();
        let spec = spec_with_sources(Vec::new());
        let created = runtime
            .create_with_admission::<_, _, Error>(
                &machine(),
                ContainerRequest {
                    creation_key: Some("plan:0"),
                    ..container_request(
                        ContainerKind::ServiceContainer,
                        &namespace,
                        &spec,
                        std::future::ready(None),
                    )
                },
            )
            .await
            .unwrap();

        runtime.remove_all_managed().await.unwrap();

        assert!(containers.lock().unwrap().is_empty());
        assert!(
            runtime
                .specs
                .get(&created.container_id)
                .await
                .unwrap()
                .is_none()
        );
        let calls = fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, path)| {
                path.contains(created.container_id.as_str()) || path.contains(&created.display_name)
            })
            .filter(|(method, path)| {
                *method == axum::http::Method::DELETE
                    || (*method == axum::http::Method::POST && path.ends_with("/stop"))
            })
            .map(|(method, _)| method.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            calls,
            [axum::http::Method::POST, axum::http::Method::DELETE],
            "stop, then remove"
        );
    }
}

#[cfg(test)]
mod admission_tests;

#[cfg(test)]
mod body_tests;
