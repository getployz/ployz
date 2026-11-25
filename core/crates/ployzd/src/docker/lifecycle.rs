use ployz_core::labels::{self, ServiceLabels};
use std::future::Future;

use bollard::{
    models::{ContainerCreateBody, Mount, MountType, NetworkConnectRequest},
    query_parameters::{
        CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
    },
};
use ployz_core::{
    AddressPart, ContainerId, ContainerKind, ContainerObservation, Gen, Machine, MachineId,
    MachineStorageObservation, Namespace, PullPolicy, QualifiedService, ResolvedServiceSpec,
    ServiceName, ServiceNetwork, ServicePlacementEligibility, ServicePlacementIneligibleReason,
    ServicePlacementUnknownReason, SlotKey,
};

#[cfg(test)]
use ployz_core::MachineGateway;

use crate::{
    docker_image::{ImagePull, UnsignedImage, prepare_image},
    machine::AdmittedSpec,
    network::{DOCKER_NETWORK_NAME, TUNNEL_NETWORK_NAME, bridge},
};

use super::{
    Doors, Error, FloorTurn, Images, Leaving, LocalDocker, MachineSpecStore, Observation,
    RotationTurn, Volumes, Withdrawal, create,
    create::CreationNetwork,
    doors::Door,
    error::{Status, classify, docker_error},
    inspect::{self, Inspected, inspect},
    malformed_container,
    recovery::IngressRecovery,
    spec_store::ConfigOperation,
    standing::Standing,
};
use crate::observe::collector::Collector;
use crate::sealed_compat::LogStore;
use futures_util::StreamExt;

/// Whether a Leave owns this Machine's Containers: set when it starts removing them and
/// kept once it commits, cleared only by the caller when the Leave fails. Nothing else
/// writes it.
#[derive(Clone, Default)]
pub(super) struct Departure(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Departure {
    pub(super) fn begin(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(super) fn failed(&self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    pub(super) fn under_way(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// What a removal does with the Container's anonymous volumes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VolumeDisposal {
    /// They stay.
    Keep,
    /// They go when this Machine recorded the named Container as made to replace this one,
    /// and it still runs in the slot (see [`Lifecycle::replaced`]).
    ReplacedBy(ContainerId),
    /// They go with a system Container, whose spec is the daemon's own.
    System,
}

impl From<ployz_core::AnonymousVolumes> for VolumeDisposal {
    fn from(asked: ployz_core::AnonymousVolumes) -> Self {
        asked.replaced_by().map_or(Self::Keep, Self::ReplacedBy)
    }
}

/// How a Container leaves through [`Lifecycle::retire`].
pub(super) enum How {
    /// A request to remove it, as asked.
    Asked {
        volumes: VolumeDisposal,
        force: bool,
    },
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
    /// Identity of this currently existing creation.
    pub(crate) creation_key: &'spec str,
    pub(crate) deployment_id: Option<&'spec ployz_core::DeploymentLogId>,
    pub(crate) r#gen: Gen,
    /// The copy on this Machine the new Container takes the place of.
    pub(crate) replaces: Option<&'spec ContainerId>,
    /// How creation obtains the image.
    pub(crate) image: ImagePull<'spec>,
    /// Whether this is a long-running Service Container or a Pre-deploy Hook.
    pub(crate) kind: ContainerKind,
    /// Namespace that owns the resulting container.
    pub(crate) namespace: &'spec Namespace,
    /// Fully resolved Service specification to persist and execute.
    pub(crate) spec: &'spec ResolvedServiceSpec,
    /// This Machine's admission of `spec`.
    pub(crate) admitted: AdmittedSpec,
    /// Deferred Machine-local admission, awaited only after volume admission.
    pub(crate) admission: Admission,
    /// Fresh local storage observation deferred to final container admission.
    pub(crate) storage: Storage,
    /// The volume plugin, which says what origin each Provisioned Volume carries.
    pub(crate) plugin: &'spec crate::storage::Plugin,
}

/// A request once admission has consumed its futures: what the rest of creation reads.
struct Creating<'a> {
    machine: &'a Machine,
    creation_key: &'a str,
    deployment_id: Option<&'a ployz_core::DeploymentLogId>,
    r#gen: Gen,
    replaces: Option<&'a ContainerId>,
    image: ImagePull<'a>,
    kind: ContainerKind,
    namespace: &'a Namespace,
    spec: &'a ResolvedServiceSpec,
    admitted: AdmittedSpec,
    plugin: &'a crate::storage::Plugin,
}

/// Creating, starting, stopping and removing the managed Containers, and the marks and
/// records those operations keep.
#[derive(Clone)]
pub struct Lifecycle {
    pub(super) docker: LocalDocker,
    pub(super) specs: MachineSpecStore,
    pub(super) observation: Observation,
    pub(super) volumes: Volumes,
    pub(super) doors: Doors,
    pub(super) images: Images,
    pub(super) standing: Standing,
    pub(super) recovery: IngressRecovery,
    pub(super) log_store: LogStore,
    pub(super) collector: Collector,
    /// Set when a Leave begins removing every managed Container, and cleared only when
    /// that Leave fails, so a committed Leave keeps it set.
    pub(super) departure: Departure,
}

impl Lifecycle {
    /// Whether a Leave is removing every managed Container here, or has committed. A
    /// Container call that overlapped it met the Leave's stops.
    #[must_use]
    pub fn departing(&self) -> bool {
        self.departure.under_way()
    }

    /// A Leave that failed leaves this Machine in the Cluster, taking Container work again.
    pub fn departure_failed(&self) {
        self.departure.failed();
    }

    pub async fn floor_turn(&self) -> FloorTurn<'_> {
        self.specs.floor_turn().await
    }

    pub async fn clear_floors(&self) -> Result<(), Error> {
        Ok(self.specs.clear_floors().await?)
    }

    /// Record that this Machine is forgetting its copy of `name`, which carries `origin`.
    pub async fn forgot(
        &self,
        name: &ployz_core::DockerVolumeName,
        origin: &ployz_core::VolumeOrigin,
    ) -> Result<(), Error> {
        Ok(self.specs.forgot(name, origin.to_string()).await?)
    }

    /// Whether the copy of `volume` this Machine last forgot carried the origin `volume`
    /// expects: its data lives on another Server now.
    ///
    /// # Errors
    /// Returns the read's error, or an origin `volume` can't name.
    pub(crate) async fn forgot_here(
        &self,
        volume: &ployz_core::ServiceVolume,
    ) -> Result<bool, Error> {
        let Some(name) = volume.source.docker_volume_name() else {
            return Ok(false);
        };
        let Some(forgotten) = self.specs.forgotten(name).await? else {
            return Ok(false);
        };
        let expected = ployz_core::VolumeOrigin::expected(volume.source.birth(), name)
            .map_err(|error| Error::InvalidContainerConfig(format!("mount origins: {error}")))?;
        Ok(forgotten == expected.to_string())
    }

    /// Refuse a mounted Provisioned Volume this Machine forgot its copy of and holds no
    /// copy of now. Its data lives on another Server, so a fresh root here would be a
    /// second Writer. A Volume added again after `ployz volume rm` has a new birth, so
    /// another origin, and starts empty as before.
    ///
    /// # Errors
    /// Returns [`Error::VolumeNotWritable`] for such a Volume, or the read's error.
    pub(crate) async fn refuse_forgotten<'a>(
        &self,
        volumes: impl IntoIterator<Item = &'a ployz_core::ServiceVolume>,
        plugin: &crate::storage::Plugin,
    ) -> Result<(), Error> {
        for volume in volumes {
            let Some(name) = volume.source.docker_volume_name() else {
                continue;
            };
            if !self.forgot_here(volume).await? {
                continue;
            }
            let copy = plugin
                .call::<Option<ployz_core::VolumeCopy>>(
                    crate::volume_copy::INSPECT_ROUTE,
                    &crate::volume_copy::CopyRequest { name: name.clone() },
                )
                .await
                .map_err(|source| Error::VolumeOriginUnread {
                    name: name.clone(),
                    source: Box::new(source),
                })?;
            if copy.is_none() {
                return Err(Error::VolumeNotWritable(name.clone()));
            }
        }
        Ok(())
    }

    /// Refuse what a Volume start's Create would refuse once `moving`'s copy is promoted:
    /// each other mounted Provisioned Volume this Machine forgot and holds no copy of, and
    /// each copy, root or a slot the start would promote, whose origin isn't the one the
    /// spec expects. `moving` with no copy here yet is the one the Move pulls before its
    /// start, even onto a Server that once forgot it.
    ///
    /// # Errors
    /// Returns [`Error::VolumeNotWritable`] or [`Error::ForeignVolume`] for such a Volume,
    /// or the read's error.
    pub(crate) async fn check_start_volumes(
        &self,
        spec: &ResolvedServiceSpec,
        plugin: &crate::storage::Plugin,
        moving: &ployz_core::DockerVolumeName,
    ) -> Result<(), Error> {
        for volume in spec.volume_graph().mounted_provisioned_volumes() {
            let Some(name) = volume.source.docker_volume_name() else {
                continue;
            };
            let copy = plugin
                .call::<Option<ployz_core::VolumeCopy>>(
                    crate::volume_copy::INSPECT_ROUTE,
                    &crate::volume_copy::CopyRequest { name: name.clone() },
                )
                .await
                .map_err(|source| Error::VolumeOriginUnread {
                    name: name.clone(),
                    source: Box::new(source),
                })?;
            let carried = match copy {
                None if name != moving && self.forgot_here(volume).await? => {
                    return Err(Error::VolumeNotWritable(name.clone()));
                }
                None => continue,
                Some(
                    ployz_core::VolumeCopy::Root { origin, .. }
                    | ployz_core::VolumeCopy::Slot { origin, .. },
                ) => origin.unwrap_or_else(|| ployz_core::VolumeOrigin::legacy(name)),
            };
            let expected = ployz_core::VolumeOrigin::expected(volume.source.birth(), name)
                .map_err(|error| {
                    Error::InvalidContainerConfig(format!("mount origins: {error}"))
                })?;
            if carried != expected {
                return Err(super::volume::other_data(name));
            }
        }
        Ok(())
    }

    /// Take the lock `remove` takes and read a copy's rotation under it, so no other
    /// rotation write or removal lands before the turn is spent.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ContainerNotFound`] when Docker no longer has the Container,
    /// and when the Machine database cannot be read.
    pub async fn rotation_turn(
        &self,
        container_id: &ContainerId,
    ) -> Result<RotationTurn<'_>, Error> {
        let operation = self.specs.config_operation().await;
        self.observation.ensure_managed(container_id).await?;
        let rotation = self.specs.rotation(container_id).await?.unwrap_or_default();
        Ok(RotationTurn {
            runtime: self,
            operation,
            container_id: *container_id,
            rotation,
        })
    }

    /// Record that this Machine stopped `run` of the Container for a `Stopped` want.
    /// Observations of that run, once exited, report it stopped; a later run does not.
    ///
    /// # Errors
    ///
    /// Returns when the Machine database cannot be written.
    pub async fn record_stop(
        &self,
        container_id: &ContainerId,
        run: &ployz_core::ContainerRun,
    ) -> Result<(), Error> {
        Ok(self
            .specs
            .config_operation()
            .await
            .set_stopped(container_id, run)
            .await?)
    }

    /// Report this Container as `stopping` until the returned Withdrawal drops,
    /// so the Ingress Proxies stop routing to it before it stops. Each operation
    /// holds its own; the Container returns to ingress once none is held.
    pub fn withdraw(&self, container_id: ContainerId) -> Withdrawal {
        self.standing.withdraw(container_id)
    }

    /// As [`Self::withdraw`], for a caller that holds no config lock: the mark is set
    /// under it, so a removal that read this Container unmarked has finished first.
    pub async fn withdraw_locked(&self, container_id: ContainerId) -> Withdrawal {
        let _operation = self.specs.config_operation().await;
        self.standing.withdraw(container_id)
    }

    /// Mark this Container leaving until the returned guard drops, so a peer's inspect
    /// reports it. The caller holds the Machine's exclusive admission turn.
    #[must_use]
    pub fn leave(&self, container_id: ContainerId) -> Leaving {
        self.standing.leave(container_id)
    }

    /// Drop what the daemon remembers about a container Docker no longer has.
    pub(super) async fn forget(&self, container_id: &ContainerId) {
        self.observation.forget(container_id);
        self.recovery.forget(container_id).await;
    }

    pub async fn list_managed(
        &self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Error> {
        let mut observations = Vec::new();
        let mut inspected = futures_util::stream::iter(self.observation.managed_inventory().await?)
            .map(|container_id| async move {
                (
                    container_id,
                    self.observation
                        .inspect_managed(&container_id, machine_id)
                        .await,
                )
            })
            .buffered(8);
        while let Some((container_id, result)) = inspected.next().await {
            match result {
                Ok(observation) => observations.push(observation),
                Err(Error::SpecNotFound(_))
                    if self
                        .discard_half_made(&container_id)
                        .await
                        .unwrap_or_else(|error| {
                            eprintln!(
                                "couldn't remove container {container_id}, which has no spec: {error}",
                                error = ployz_core::error_chain::inline(&error),
                            );
                            false
                        }) =>
                {
                    eprintln!(
                        "removed container {container_id}, which a crash left without its spec before it started"
                    );
                }
                Err(error) if malformed_container(&error) => {
                    eprintln!(
                        "ignoring malformed managed container {container_id}: {error}",
                        error = ployz_core::error_chain::inline(&error),
                    );
                }
                Err(error) => return Err(error),
            }
        }
        Ok(observations)
    }

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
                creation_key: &format!("test:{}", MachineId::random()),
                deployment_id: None,
                r#gen: Gen::FIRST,
                replaces: None,
                image: ImagePull::Policy(None),
                kind,
                namespace,
                spec,
                admitted: AdmittedSpec::test_signed(namespace, spec),
                admission: std::future::ready(Ok::<_, Error>(())),
                storage: std::future::ready(None),
                plugin: &super::test_support::NO_PLUGIN,
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
            replaces,
            image,
            kind,
            namespace,
            spec,
            admitted,
            admission,
            storage,
            plugin,
        } = request;
        // A key that admitted the spec may have been removed while this creation waited
        // for its turn: it then makes, pulls and adopts nothing, Volumes included, so it
        // never starts a Container another key's creation left in the slot.
        admitted
            .still_holds(Error::AdmissionUnread)
            .map_err(E::from)?
            .map_err(|refusal| E::from(Error::Unadmitted(refusal)))?;
        {
            // Wait for an in-flight create to persist its spec before comparing a retry.
            let mut operation = self.specs.config_operation().await;
            if let Some(existing) = self
                .matching_creation(
                    &mut operation,
                    machine,
                    (namespace, kind, spec),
                    creation_key,
                    admitted.admission(),
                )
                .await
                .map_err(E::from)?
            {
                return self.adopt(existing, image).await.map_err(E::from);
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
            self.admit_and_ensure_volumes(machine, namespace, spec, storage, plugin)
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
            replaces,
            image,
            kind,
            namespace,
            spec,
            admitted,
            plugin,
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
        if let ImagePull::Pinned(id) = creating.image {
            body.image = Some(id.to_string());
        }
        if !creating.admitted.signed() {
            // The caller's word that the image is here, and its login, count for nothing.
            match creating.image {
                ImagePull::Pinned(id) => {
                    // A digest pull can't change what it names, so `always` asks nothing more.
                    let policy = match spec.container.pull_policy {
                        PullPolicy::Always => PullPolicy::Missing,
                        policy @ (PullPolicy::Missing | PullPolicy::Never) => policy,
                    };
                    self.images
                        .pull_free(
                            &crate::docker_image::pinned_reference(&spec.container.image, id),
                            policy,
                        )
                        .await?;
                }
                ImagePull::Placed(id) if self.images.is_free(id) => {
                    body.image = Some(id.to_owned());
                }
                ImagePull::Policy(_) | ImagePull::Prepared | ImagePull::Placed(_) => {
                    // By ID: the spec's tag here may name what a caller with Full access
                    // put under it, which this creation must not run.
                    let id = self
                        .images
                        .pull_free(&spec.container.image, spec.container.pull_policy)
                        .await?;
                    body.image = Some(id);
                }
            }
        } else if let ImagePull::Placed(id) = creating.image {
            body.image = Some(id.to_owned());
        } else if let ImagePull::Policy(registry_auth) = creating.image
            && let Err(error) = prepare_image(
                &self.docker.client,
                &spec.container.image,
                spec.container.pull_policy,
                registry_auth,
            )
            .await
        {
            return Err(self.images.pull_failure(Error::ImagePull(error)).await);
        }
        let mut config_operation = self.specs.config_operation().await;
        // Another request may have won while admission and image preparation ran.
        if let Some(existing) = self
            .matching_creation(
                &mut config_operation,
                machine,
                (namespace, kind, spec),
                creation_key,
                creating.admitted.admission(),
            )
            .await?
        {
            return self.adopt(existing, creating.image).await;
        }
        // A key that admitted the spec may have been removed, or a newer generation of
        // its rule may have arrived, while the image pulled.
        creating
            .admitted
            .still_holds(Error::AdmissionUnread)?
            .map_err(Error::Unadmitted)?;
        // Read last, under this lock and the caller's switch guard, so no promote or seed
        // lands other data under a mounted name before Docker makes the Container.
        self.volumes
            .require_origins(&machine.id, spec, creating.plugin)
            .await?;
        let mounts = body
            .host_config
            .get_or_insert_default()
            .mounts
            .get_or_insert_default();
        self.volumes
            .pin_external_drivers(&machine.id, spec, mounts)
            .await?;
        mounts.extend(docker_config_mounts(&mut config_operation, spec).await?);
        let replaced = self.slot_taken(&creating).await;
        let result = self
            .create_and_record(&creating, &mut config_operation, body, replaced)
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
                self.doors
                    .running_door(Door::Holder)
                    .await?
                    .ok_or(Error::IngressHolderNotRunning)?,
            ),
            Some(ServiceNetwork::TunnelOrigin) => CreationNetwork::Holder(
                self.doors
                    .running_door(Door::TunnelOrigin)
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
            self.observation.sealing(),
            &creating.admitted,
        )?;
        create::ingress_sysctls(
            &mut body,
            Door::of_service(namespace, &spec.name),
            std::path::Path::new("/proc/sys")
                .join(create::TCP_MIGRATE_REQ.replace('.', "/"))
                .exists(),
        );
        labels::stamp_run(
            body.labels.get_or_insert_default(),
            deployment_id,
            Some(creation_key),
            r#gen,
        );
        Ok(body)
    }

    /// Creates the Container under its slot's name, then writes its spec, and that it
    /// takes `replaced`'s place. A Container with no spec is removed again, so none
    /// outlives a failed write.
    async fn create_and_record(
        &self,
        creating: &Creating<'_>,
        config_operation: &mut ConfigOperation<'_>,
        body: ContainerCreateBody,
        replaced: Option<ContainerId>,
    ) -> Result<ContainerCreated, Error> {
        let Creating {
            machine,
            creation_key,
            kind,
            namespace,
            spec,
            ..
        } = *creating;
        let display_name = creation_name(&machine.id, namespace, &spec.name, kind, creation_key);
        let options = CreateContainerOptionsBuilder::default()
            .name(&display_name)
            .build();
        let part = ployz_core::part_of(
            &QualifiedService::new(namespace.clone(), spec.name.clone()),
            Some(creating.admitted.by().admission()),
            spec.volume_graph(),
        );
        let created = match self
            .docker
            .create_container(Some(options), body, Some(part))
            .await
        {
            Ok(created) => created,
            Err(Error::Docker(ref error)) if classify(error) == Status::Conflict => {
                return self
                    .matching_creation(
                        config_operation,
                        machine,
                        (namespace, kind, spec),
                        creation_key,
                        creating.admitted.admission(),
                    )
                    .await?
                    .ok_or(Error::SlotNameOccupied(display_name));
            }
            Err(error) => return Err(error),
        };
        let container_id =
            ContainerId::parse(created.id).map_err(|source| Error::InvalidValue {
                field: "container ID",
                source,
            })?;
        if !creating.admitted.signed() && !self.runs_free_image(&container_id).await? {
            self.remove_if_present(&container_id).await?;
            return Err(Error::UnsignedImage(UnsignedImage::Moved));
        }
        // A daemon killed here leaves a Container no spec describes.
        crate::faults::kill_inside(
            crate::faults::Point::Verb("CreateContainer.BeforeSpec"),
            None,
        );
        let slot = super::spec_store::CreationSlot::new(namespace, kind, creation_key);
        if let Err(error) = config_operation
            .put(
                &container_id,
                Some(&slot),
                spec,
                Some(creating.admitted.by()),
            )
            .await
        {
            self.remove_if_present(&container_id).await?;
            return Err(error.into());
        }
        // A daemon killed before this write leaves no record, so the old copy's anonymous
        // volumes stay.
        if let Some(replaced) = replaced {
            config_operation
                .record_replacement(&container_id, &replaced)
                .await?;
        }
        Ok(ContainerCreated {
            container_id,
            display_name,
        })
    }

    /// The copy whose slot this creation takes: the one the caller names, when it is a
    /// Container of the same Service and kind on this Machine that is In or Held, so it
    /// is what the slot routes. Read under the config lock, which every rotation write
    /// takes. A copy this Machine can't read takes no record, so its volumes stay.
    async fn slot_taken(&self, creating: &Creating<'_>) -> Option<ContainerId> {
        let replaced = creating.replaces?;
        let inspected = self.observation.ensure_managed(replaced).await.ok()?;
        let labels = ServiceLabels::parse(inspected.labels()?).ok()?;
        let new = ServiceLabels {
            identity: ployz_core::QualifiedService::new(
                creating.namespace.clone(),
                creating.spec.name.clone(),
            ),
            kind: creating.kind,
        };
        let rotation = self.specs.rotation(replaced).await.ok()??;
        (labels == new && slotted(&rotation)).then_some(*replaced)
    }

    /// The image ID `container_id` runs, as Docker resolved it when it made the Container.
    ///
    /// # Errors
    ///
    /// Returns when Docker cannot inspect the Container.
    pub(crate) async fn image_of(
        &self,
        container_id: &ContainerId,
    ) -> Result<Option<String>, Error> {
        Ok(self.docker.inspect(container_id).await?.image)
    }

    /// Whether the Container just made runs an image [`Images::pull_free`] placed. Its
    /// image was resolved by name when Docker made it, after the pull, so a name another
    /// caller moved in between is caught here, before the Container starts.
    async fn runs_free_image(&self, container_id: &ContainerId) -> Result<bool, Error> {
        Ok(self
            .image_of(container_id)
            .await?
            .is_some_and(|image| self.images.is_free(&image)))
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
        self.matching_creation(
            &mut operation,
            machine,
            (namespace, kind, spec),
            key,
            ployz_core::Admission::Free,
        )
        .await
    }

    /// The Container an earlier create under `key` made. Every caller holds the spec
    /// store's config lock, so no create is between Docker's create and its spec's write.
    /// A create `wanted` signed refuses a copy this Machine didn't admit signed rather
    /// than adopt it: that copy answers for no name a signed one keeps.
    async fn matching_creation(
        &self,
        operation: &mut ConfigOperation<'_>,
        machine: &Machine,
        (namespace, kind, spec): (&Namespace, ContainerKind, &ResolvedServiceSpec),
        key: &str,
        wanted: ployz_core::Admission,
    ) -> Result<Option<ContainerCreated>, Error> {
        let name = &creation_name(&machine.id, namespace, &spec.name, kind, key);
        let existing = self.resolve_slot(operation, &machine.id, name, key).await?;
        let Some(existing) = existing else {
            return Ok(None);
        };
        let signed = wanted == ployz_core::Admission::Signed;
        if !same_creation(&existing, namespace, kind, spec) || (signed && !existing.signed()) {
            return Err(Error::SlotNameOccupied(name.into()));
        }
        Ok(Some(ContainerCreated {
            container_id: existing.container_id,
            display_name: existing.into_parts().display_name,
        }))
    }

    /// `existing`, the Container an earlier create under the key made, for a create that
    /// asks for `image`. A create by a placed ID adopts only a Container Docker made from
    /// that ID: [`same_creation`] reads the spec, whose tag can name another image since.
    async fn adopt(
        &self,
        existing: ContainerCreated,
        image: ImagePull<'_>,
    ) -> Result<ContainerCreated, Error> {
        let ImagePull::Placed(id) = image else {
            return Ok(existing);
        };
        let made = inspect(&self.docker.client, existing.container_id.as_str())
            .await
            .map_err(inspect::failed)?
            .and_then(|inspected| inspected.image);
        if made.as_deref() == Some(id) {
            Ok(existing)
        } else {
            Err(Error::SlotNameOccupied(existing.display_name))
        }
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
        let listed = self.observation.managed_slots().await?;
        let present = listed.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        for (old, new) in operation.adopt_restored(listed).await? {
            eprintln!("adopted restored container {new}, which replaces {old}");
        }
        if self.specs.get(container_id).await?.is_some() {
            return Ok(false);
        }
        let Some(inspected) = inspect(&self.docker.client, container_id.as_str())
            .await
            .map_err(inspect::failed)?
        else {
            return Ok(true);
        };
        let (true, Some(service_id)) = (
            inspected.creation_key().is_some() && inspected.status() == Some("created"),
            inspected.label(labels::SERVICE_ID),
        ) else {
            return Ok(false);
        };
        Ok(!self.specs.holds_absent(present, service_id.into()).await?)
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
        let Some(inspected) = inspect(&self.docker.client, name)
            .await
            .map_err(inspect::failed)?
        else {
            return Ok(None);
        };
        if inspected.creation_key() != Some(key) {
            return Err(Error::SlotNameOccupied(name.into()));
        }
        let container_id = ContainerId::parse(
            inspected.id.ok_or(Error::MissingField("container ID"))?,
        )
        .map_err(|source| Error::InvalidValue {
            field: "container ID",
            source,
        })?;
        self.observation
            .inspect_managed(&container_id, machine_id)
            .await
            .map(Some)
    }

    async fn admit_and_ensure_volumes(
        &self,
        machine: &Machine,
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
        storage: impl Future<Output = Option<MachineStorageObservation>>,
        plugin: &crate::storage::Plugin,
    ) -> Result<ServicePlacementEligibility, Error> {
        let storage = if spec.volume_graph().has_mounted_provisioned_volume() {
            storage.await
        } else {
            None
        };
        let eligibility =
            spec.placement_eligibility_in_namespace(namespace, machine, storage.as_ref());
        if matches!(eligibility, ServicePlacementEligibility::Eligible) {
            self.refuse_forgotten(spec.volume_graph().mounted_provisioned_volumes(), plugin)
                .await?;
            self.volumes
                .ensure_mounted_volumes(&machine.id, spec)
                .await?;
        }
        Ok(eligibility)
    }

    /// Who admitted the Container, `None` when this Machine kept no admission for it.
    pub(crate) async fn admitted_by(
        &self,
        container_id: &ContainerId,
    ) -> Result<Option<crate::machine::AdmittedBy>, Error> {
        Ok(self.specs.admitted_by(container_id).await?)
    }

    pub async fn start(&self, container_id: &ContainerId) -> Result<(), Error> {
        let inspected = self.observation.ensure_managed(container_id).await?;
        self.recovery.release(container_id).await;
        self.start_inspected(container_id, &inspected).await
    }

    pub(super) async fn start_inspected(
        &self,
        container_id: &ContainerId,
        inspected: &Inspected,
    ) -> Result<(), Error> {
        if let Some(network) = lost_network(inspected)
            && let Err(error) = self.rejoin(container_id, network, inspected).await
            // A concurrent start may have attached it first.
            && lost_network(&self.observation.ensure_managed(container_id).await?).is_some()
        {
            return Err(error);
        }
        if let Some(container_labels) = inspected.labels() {
            self.collector.cover(container_labels).await?;
        }
        let spec = self.specs.get(container_id).await?;
        match super::holder::start_inspected(&self.docker.client, inspected, spec.as_ref()).await {
            Err(Error::Docker(error)) => idempotent_lifecycle_result(container_id, Err(error)),
            started => started,
        }
    }

    /// Attach a Container to the network a failed start took from it. The Tunnel
    /// Origin's network is checked as at creation, so it never joins one the firewall
    /// doesn't fence.
    async fn rejoin(
        &self,
        container_id: &ContainerId,
        network: &str,
        inspected: &Inspected,
    ) -> Result<(), Error> {
        if network == DOCKER_NETWORK_NAME {
            let ipv6 = inspected.labels().is_some_and(bridge::takes_ipv6);
            let namespace = inspected
                .labels()
                .and_then(|labels| labels.get(labels::NAMESPACE))
                .and_then(|namespace| Namespace::parse(namespace).ok());
            let part = self
                .specs
                .address_part(container_id, namespace.as_ref())
                .await?;
            return self.docker.join_bridge(container_id, ipv6, part).await;
        }
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
            .map_err(|error| docker_error(container_id, error))
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
        let managed = self.observation.ensure_managed(container_id).await;
        let plan = if managed.is_ok() {
            Some(self.recovery.before_stop(container_id).await)
        } else {
            None
        };
        let wanted = still_wanted.await;
        if !matches!(wanted, Ok(true)) {
            return wanted;
        }
        if let Some(plan) = plan {
            plan.proceed();
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

    /// Removes `container_id` and its records. Its anonymous volumes go only as
    /// [`Self::replaced`] decides from `volumes`.
    pub async fn remove(
        &self,
        container_id: &ContainerId,
        volumes: VolumeDisposal,
        force: bool,
    ) -> Result<(), Error> {
        let mut config_operation = self.specs.config_operation().await;
        self.remove_locked(&mut config_operation, container_id, volumes, force)
            .await
    }

    pub(super) async fn remove_locked(
        &self,
        config_operation: &mut super::spec_store::ConfigOperation<'_>,
        container_id: &ContainerId,
        volumes: VolumeDisposal,
        force: bool,
    ) -> Result<(), Error> {
        self.retire(
            config_operation,
            container_id,
            How::Asked { volumes, force },
        )
        .await
    }

    pub async fn remove_all_managed(&self) -> Result<(), Error> {
        self.departure.begin();
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
        let (removed, kept) = match how {
            How::Asked { volumes, force } => {
                match self.observation.ensure_managed(container_id).await {
                    Ok(inspected) => {
                        let remove_volumes = self
                            .replaced(config_operation, container_id, &inspected, volumes)
                            .await?;
                        let kept = if remove_volumes {
                            Vec::new()
                        } else {
                            self.anonymous_volumes(container_id, &inspected).await
                        };
                        let options = RemoveContainerOptionsBuilder::default()
                            .v(remove_volumes)
                            .force(force)
                            .build();
                        let removed = self
                            .docker
                            .client
                            .remove_container(container_id.as_str(), Some(options))
                            .await
                            .map_err(|error| docker_error(container_id, error));
                        (removed, kept)
                    }
                    Err(error) => (Err(error), Vec::new()),
                }
            }
            How::Departing => {
                self.stop_if_running(container_id).await?;
                (self.remove_if_present(container_id).await, Vec::new())
            }
        };
        match removed {
            Ok(()) => {
                // A daemon killed before this write keeps the Volumes unrecorded, which
                // only this Machine's record would let anything remove.
                config_operation.remove_keeping(container_id, kept).await?;
            }
            Err(Error::ContainerNotFound(_)) if config_operation.remove(container_id).await? => {}
            Err(error) => return Err(error),
        }
        self.forget(container_id).await;
        Ok(())
    }

    /// Whether `retiree`'s anonymous volumes go with it. Only when the removal names the
    /// Container it relies on, this Machine recorded when it made that Container that it
    /// takes `retiree`'s place (see [`Self::slot_taken`]), and that Container runs, In or
    /// Held, and no stop is taking it out of routing. Read under the config lock, which
    /// every create, removal, rotation write and stopping mark takes, so none lands
    /// between this read and the removal. A system Container's, whose spec is the
    /// daemon's own, go when the daemon asks.
    async fn replaced(
        &self,
        config_operation: &mut ConfigOperation<'_>,
        retiree: &ContainerId,
        inspected: &Inspected,
        volumes: VolumeDisposal,
    ) -> Result<bool, Error> {
        let named = match volumes {
            VolumeDisposal::Keep => return Ok(false),
            VolumeDisposal::System => {
                let labels = inspected.labels().ok_or(Error::NotManaged)?;
                return Ok(labels::Class::of(labels) == Some(labels::Class::System));
            }
            VolumeDisposal::ReplacedBy(named) => named,
        };
        let Some(replacement) = config_operation.replacement(retiree).await? else {
            return Ok(false);
        };
        if replacement != named
            || self.standing.is_withdrawn(&replacement)
            || !self
                .specs
                .rotation(&replacement)
                .await?
                .is_some_and(|rotation| slotted(&rotation))
        {
            return Ok(false);
        }
        match self.docker.inspect(&replacement).await {
            Ok(replacement) => {
                Ok(super::runtime_observation(replacement.state.as_ref()).is_running())
            }
            Err(Error::ContainerNotFound(_)) => Ok(false),
            Err(error) => Err(error),
        }
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
    /// Connects a Container that lost its bridge endpoint at a new address in `part`,
    /// holding the gate until Docker records it so no create takes the same one.
    async fn join_bridge(
        &self,
        container_id: &ContainerId,
        ipv6: bool,
        part: AddressPart,
    ) -> Result<(), Error> {
        let mut handed = self.endpoint_creates.lock().await;
        let address = self.bridge().await?.take(&mut handed, part)?;
        self.client
            .connect_network(
                DOCKER_NETWORK_NAME,
                NetworkConnectRequest {
                    container: container_id.to_string(),
                    endpoint_config: Some(bridge::address::endpoint(ipv6, address)),
                },
            )
            .await
            .map_err(|error| docker_error(container_id, error))
    }

    /// Creates a Container, at a new address in `part` when it joins the bridge. A
    /// Container in another's network, or on the host's, takes no address and no part.
    pub(super) async fn create_container(
        &self,
        options: Option<bollard::query_parameters::CreateContainerOptions>,
        mut body: ContainerCreateBody,
        part: Option<AddressPart>,
    ) -> Result<bollard::models::ContainerCreateResponse, Error> {
        let mut handed = self.endpoint_creates.lock().await;
        let on_bridge = body
            .host_config
            .as_ref()
            .and_then(|host| host.network_mode.as_deref())
            == Some(DOCKER_NETWORK_NAME);
        if on_bridge {
            let part = part.ok_or(bridge::address::NoAddress::NoRecords)?;
            let bridge = self.bridge().await?;
            if bridge.capacity.free_endpoints() == 0 {
                return Err(Error::EndpointCapacity);
            }
            let address = bridge.take(&mut handed, part)?;
            let endpoints = body
                .networking_config
                .get_or_insert_default()
                .endpoints_config
                .get_or_insert_default();
            let endpoint = endpoints.entry(DOCKER_NETWORK_NAME.to_owned()).or_default();
            endpoint.ipam_config = bridge::address::endpoint(false, address).ipam_config;
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

pub(super) fn idempotent_lifecycle_result(
    container_id: &ContainerId,
    result: Result<(), bollard::errors::Error>,
) -> Result<(), Error> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if classify(&error) == Status::Unchanged => Ok(()),
        Err(error) => Err(docker_error(container_id, error)),
    }
}

/// Whether a copy in `rotation` is what its slot routes: In, or Held by a run that may
/// route it again.
fn slotted(rotation: &ployz_core::CopyRotation) -> bool {
    matches!(
        rotation,
        ployz_core::CopyRotation::In | ployz_core::CopyRotation::Held(_)
    )
}

/// Whether `observed` is what a create of `spec` makes. The pull policy decides only how
/// the image got here, not what runs, so a resend that asks for it differently, as a
/// forward Release does after its Move's `never`, finds the Container that create made.
/// The image reference must still match, and a create by ID adopts only a Container
/// made from that ID ([`LocalLifecycle::adopt`]).
pub(crate) fn same_creation(
    observed: &ContainerObservation,
    namespace: &Namespace,
    kind: ContainerKind,
    spec: &ResolvedServiceSpec,
) -> bool {
    let made = &observed.resolved_spec;
    observed.namespace == *namespace
        && observed.kind == kind
        && (made == spec || {
            let mut asked = spec.clone();
            asked.container.pull_policy = made.container.pull_policy;
            *made == asked
        })
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

/// Docker 29 empties a container's networks when its start fails, and a later start
/// then runs it with no network and no published ports. A Container in another's
/// network has none to lose. Answers the network a Container on the `ployz` bridge
/// or a Tunnel Origin lost.
fn lost_network(inspected: &Inspected) -> Option<&'static str> {
    let network = match inspected.network_mode() {
        Some(DOCKER_NETWORK_NAME) => DOCKER_NETWORK_NAME,
        Some(TUNNEL_NETWORK_NAME) => TUNNEL_NETWORK_NAME,
        _ => return None,
    };
    (!inspected.attached_to(network)).then_some(network)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::{
        recovery::Repair,
        test_support::{
            FakeDocker, container_request, fake_runtime_with, machine, spec_with_sources,
        },
    };

    fn inspected(network_mode: &str, networks: serde_json::Value) -> Inspected {
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

    /// A signed Container takes an Open address, and a signed hook that mounts its
    /// Service's Volume a signed data one, which the fence closes to unsigned ones.
    #[tokio::test]
    async fn a_container_joins_the_bridge_at_a_static_address_in_its_part() {
        let namespace = Namespace::parse("app").unwrap();
        for (kind, sources, address) in [
            (ContainerKind::ServiceContainer, Vec::new(), "10.210.0.2"),
            (
                ContainerKind::PreDeployHook,
                vec![crate::docker::test_support::ordinary_source("data")],
                "10.210.0.160",
            ),
        ] {
            let (runtime, fake) = fake_runtime_with(FakeDocker {
                create_barrier: Some(std::sync::Arc::new(tokio::sync::Barrier::new(1))),
                ..FakeDocker::default()
            })
            .await;
            let spec = spec_with_sources(sources);
            runtime
                .lifecycle()
                .create_with_admission::<_, _, Error>(
                    &machine(),
                    container_request(kind, &namespace, &spec, std::future::ready(None)),
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
                Some(&serde_json::json!(address)),
                "{kind:?}",
            );
        }
    }

    /// A resend under a creation's key that asks for the image by another placed ID gets
    /// no Container: the spec's tag matches, but the Container runs what Docker made it
    /// from. The same ID, under another pull policy, adopts it.
    #[tokio::test]
    async fn a_create_by_placed_id_adopts_only_a_container_made_from_that_id() {
        let namespace = Namespace::parse("app").unwrap();
        let containers = std::sync::Arc::default();
        let (runtime, _fake) = fake_runtime_with(FakeDocker {
            named_containers: Some(std::sync::Arc::clone(&containers)),
            ..FakeDocker::default()
        })
        .await;
        let mut spec = spec_with_sources(Vec::new());
        let (made, other) = (
            format!("sha256:{}", "a".repeat(64)),
            format!("sha256:{}", "b".repeat(64)),
        );
        let machine = machine();
        let create = |spec: ResolvedServiceSpec, id: String| {
            let (lifecycle, machine, namespace) =
                (runtime.lifecycle(), machine.clone(), namespace.clone());
            async move {
                lifecycle
                    .create_with_admission::<_, _, Error>(
                        &machine,
                        ContainerRequest {
                            image: ImagePull::Placed(&id),
                            ..container_request(
                                ContainerKind::ServiceContainer,
                                &namespace,
                                &spec,
                                std::future::ready(None),
                            )
                        },
                    )
                    .await
            }
        };
        spec.container.pull_policy = PullPolicy::Never;
        let first = create(spec.clone(), made.clone()).await.unwrap();

        spec.container.pull_policy = PullPolicy::Missing;
        let refused = create(spec.clone(), other).await;
        assert!(
            matches!(refused, Err(Error::SlotNameOccupied(_))),
            "{refused:?}"
        );
        let adopted = create(spec, made).await.unwrap();
        assert_eq!(adopted.container_id, first.container_id);
    }

    async fn stamped_labels(
        kind: ContainerKind,
        creation_key: &str,
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
            .lifecycle()
            .create_with_admission::<_, _, Error>(
                &machine(),
                ContainerRequest {
                    creation_key,
                    deployment_id,
                    r#gen: generation,
                    replaces: None,
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
            "plan:0",
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
        let hook = stamped_labels(ContainerKind::PreDeployHook, "plan:1", None, Gen::FIRST).await;
        assert_eq!(
            without_service_id(hook),
            serde_json::json!({
                "ployz.managed": "",
                "ployz.namespace": "app",
                "ployz.service.name": "api",
                "ployz.service.hook": "pre-deploy",
                "ployz.network.ipv6": "true",
                "ployz.creation.key": "plan:1",
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
            .lifecycle()
            .create_with_admission::<_, _, Error>(
                &machine,
                ContainerRequest {
                    creation_key: "plan:0",
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
        let found = runtime
            .lifecycle()
            .inspect_slot(&machine.id, &slot)
            .await
            .unwrap();
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
        crate::docker::ContainerRuntime,
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
                    None,
                )
                .await
                .unwrap();
        }
        let request = || ContainerRequest {
            creation_key: "plan:0",
            ..container_request(
                ContainerKind::ServiceContainer,
                &namespace,
                &spec,
                std::future::ready(None),
            )
        };
        let first = runtime
            .lifecycle()
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
            .lifecycle()
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
    /// With no key it is one made before creations were keyed, so it carries no key label.
    async fn specless(
        key: Option<&'static str>,
        status: &str,
    ) -> (
        crate::docker::ContainerRuntime,
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
            .lifecycle()
            .create_with_admission::<_, _, Error>(
                &machine,
                ContainerRequest {
                    creation_key: key.unwrap_or("legacy"),
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
            if key.is_none() {
                container
                    .pointer_mut("/Config/Labels")
                    .and_then(serde_json::Value::as_object_mut)
                    .unwrap()
                    .remove(labels::CREATION_KEY);
            }
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

        let found = runtime.lifecycle().inspect_slot(&machine.id, &slot).await;

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
            async move { runtime.lifecycle().inspect_slot(&machine.id, &slot).await }
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
            .put(&replacement, Some(&creation), &spec, None)
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

        let found = runtime.lifecycle().inspect_slot(&machine.id, &slot).await;

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
            .observation()
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
                .observation()
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
            .observation()
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
            assert!(
                runtime
                    .lifecycle()
                    .list_managed(&machine.id)
                    .await
                    .unwrap()
                    .is_empty()
            );
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

    async fn stranded(error: &str) -> (crate::docker::ContainerRuntime, FakeDocker, ContainerId) {
        stranded_with("created", error, FakeDocker::default()).await
    }

    async fn stranded_with(
        status: &str,
        error: &str,
        fake: FakeDocker,
    ) -> (crate::docker::ContainerRuntime, FakeDocker, ContainerId) {
        let (runtime, fake) = fake_runtime_with(FakeDocker {
            create_barrier: Some(std::sync::Arc::new(tokio::sync::Barrier::new(1))),
            ..fake
        })
        .await;
        let namespace = Namespace::parse("app").unwrap();
        let spec = spec_with_sources(Vec::new());
        let created = runtime
            .lifecycle()
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

    fn start_legacy_caddy(caddy: ContainerId) -> Repair {
        Repair::StartCaddy {
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
            .lifecycle()
            .recover(start_legacy_caddy(container_id), runtime.docker.creates())
            .await;
        assert!(started(&fake), "a failed first start is retried");

        runtime
            .lifecycle()
            .stop(&container_id, None, None)
            .await
            .unwrap();
        fake.requests.lock().unwrap().clear();
        runtime
            .lifecycle()
            .recover(start_legacy_caddy(container_id), runtime.docker.creates())
            .await;
        assert!(!started(&fake), "a stopped ingress stays stopped");

        runtime.lifecycle().start(&container_id).await.unwrap();
        fake.requests.lock().unwrap().clear();
        runtime
            .lifecycle()
            .recover(start_legacy_caddy(container_id), runtime.docker.creates())
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
                    .lifecycle()
                    .stop_if(&container_id, None, None, async move {
                        let _ = asked.send(());
                        std::future::pending::<Result<bool, Error>>().await
                    })
                    .await
            }
        });
        was_asked.await.unwrap();
        assert!(runtime.lifecycle().recovery.is_held(&container_id).await);

        stopping.abort();
        assert!(stopping.await.unwrap_err().is_cancelled());
        assert!(
            !runtime.lifecycle().recovery.is_held(&container_id).await,
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
                    .lifecycle()
                    .stop_if(&container_id, None, None, async move {
                        let _ = asked.send(());
                        let _ = decided.await;
                        Ok::<_, Error>(true)
                    })
                    .await
            }
        });
        was_asked.await.unwrap();

        runtime.lifecycle().forget(&container_id).await;
        decide.send(()).unwrap();
        let _ = stopping.await.unwrap();
        assert!(
            !runtime.lifecycle().recovery.is_held(&container_id).await,
            "a forgotten container must not stay held"
        );
    }

    #[tokio::test]
    async fn a_container_stays_withdrawn_until_its_last_withdrawal_drops() {
        let (runtime, _fake, container_id) = stranded(PORT_TAKEN).await;
        for first_released_first in [true, false] {
            let mut observer = runtime.lifecycle().standing.subscribe();
            let first = runtime.lifecycle().withdraw(container_id);
            let second = runtime.lifecycle().withdraw(container_id);
            assert!(observer.has_changed().unwrap());
            observer.mark_unchanged();

            let (released, held) = if first_released_first {
                (first, second)
            } else {
                (second, first)
            };
            drop(released);
            assert!(runtime.lifecycle().standing.is_withdrawn(&container_id));
            assert!(!observer.has_changed().unwrap());

            drop(held);
            assert!(!runtime.lifecycle().standing.is_withdrawn(&container_id));
            assert!(
                observer.has_changed().unwrap(),
                "the last release wakes the observer"
            );
        }
    }

    #[tokio::test]
    async fn an_ingress_about_to_be_stopped_is_not_started() {
        let (runtime, fake, container_id) = stranded(PORT_TAKEN).await;

        let _withdrawal = runtime.lifecycle().withdraw(container_id);
        runtime
            .lifecycle()
            .recover(start_legacy_caddy(container_id), runtime.docker.creates())
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
                    .lifecycle()
                    .recover(start_legacy_caddy(container_id), runtime.docker.creates())
                    .await
            }
        });
        barrier.wait().await;
        let stopping = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.lifecycle().stop(&container_id, None, None).await }
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
            .lifecycle()
            .recover(start_legacy_caddy(container_id), runtime.docker.creates())
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
                    .lifecycle()
                    .recover(start_legacy_caddy(container_id), runtime.docker.creates())
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
                    .lifecycle()
                    .stop_if(&container_id, None, None, async move {
                        Ok::<_, Error>(still_wanted.load(Ordering::SeqCst))
                    })
                    .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !runtime.lifecycle().recovery.is_held(&container_id).await {
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
        assert!(!runtime.lifecycle().recovery.is_held(&container_id).await);
    }

    #[tokio::test]
    async fn a_create_in_flight_when_the_scan_began_voids_the_start() {
        let (runtime, fake, container_id) = stranded(PORT_TAKEN).await;
        let creating = super::super::Creating::new(&runtime.docker.creates);
        let creates = runtime.docker.creates();

        runtime
            .lifecycle()
            .recover(start_legacy_caddy(container_id), creates)
            .await;
        assert!(!started(&fake), "a newer ingress may be appearing");

        drop(creating);
        runtime
            .lifecycle()
            .recover(start_legacy_caddy(container_id), creates)
            .await;
        assert!(!started(&fake), "it appeared after the scan began");

        runtime
            .lifecycle()
            .recover(start_legacy_caddy(container_id), runtime.docker.creates())
            .await;
        assert!(started(&fake), "the next scan decides");
    }

    #[tokio::test]
    async fn a_container_created_since_the_scan_voids_the_start() {
        let (runtime, fake, container_id) = stranded(PORT_TAKEN).await;
        let creates = runtime.docker.creates();
        let stranded = fake.existing_container.lock().unwrap().take();
        runtime
            .lifecycle()
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
            .lifecycle()
            .recover(start_legacy_caddy(container_id), creates)
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
                    .lifecycle()
                    .recover(start_legacy_caddy(container_id), runtime.docker.creates())
                    .await
            }
        });
        barrier.wait().await;
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            runtime.lifecycle().forget(&other),
        )
        .await
        .expect("forgetting another container does not wait for the start");
        barrier.wait().await;
        starting.await.unwrap();
    }

    #[tokio::test]
    async fn an_ingress_docker_never_tried_to_start_is_left_alone() {
        let (runtime, fake, container_id) = stranded("").await;

        runtime
            .lifecycle()
            .recover(start_legacy_caddy(container_id), runtime.docker.creates())
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
            .lifecycle()
            .recover(start_legacy_caddy(container_id), runtime.docker.creates())
            .await;

        assert!(started(&fake));
    }

    #[tokio::test]
    async fn an_ingress_that_exited_on_its_own_is_left_alone() {
        let (runtime, fake, container_id) =
            stranded_with("exited", "", FakeDocker::default()).await;

        runtime
            .lifecycle()
            .recover(start_legacy_caddy(container_id), runtime.docker.creates())
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

        runtime.lifecycle().start(&container_id).await.unwrap();

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

        runtime.lifecycle().start(&container_id).await.unwrap();

        assert!(started(&fake));
    }

    #[tokio::test]
    async fn removing_a_container_drops_its_record_after_docker_removed_it() {
        let (runtime, fake, container_id) =
            crate::docker::test_support::runtime_with_running_container().await;

        runtime
            .lifecycle()
            .remove(&container_id, VolumeDisposal::Keep, true)
            .await
            .unwrap();

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

        runtime
            .lifecycle()
            .remove(&container_id, VolumeDisposal::Keep, true)
            .await
            .unwrap();
        assert!(runtime.specs.get(&container_id).await.unwrap().is_none());

        assert!(matches!(
            runtime.lifecycle().remove(&container_id, VolumeDisposal::Keep, true).await,
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
            .lifecycle()
            .create_with_admission::<_, _, Error>(
                &machine(),
                ContainerRequest {
                    creation_key: "plan:0",
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

        runtime.lifecycle().remove_all_managed().await.unwrap();

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
