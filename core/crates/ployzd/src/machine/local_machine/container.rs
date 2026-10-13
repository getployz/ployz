//! Machine-local container admission and creation.

use std::sync::Arc;

use ployz_core::{
    AnonymousVolumes, ContainerId, ContainerKind, ContainerLocator, ContainerNow,
    ContainerObservation, ContainerRefusal, ContainerRefusalReason, ContainerRuntimeObservation,
    ContainerWant, CopyRefusal, DockerVolumeName, Drain, EnsuredContainer, ImageIngestOpened,
    ImageIngestReason, ImagePulled, ImagesRemoved, LocalMachinePhase, MachineStorageObservation,
    Namespace, NewContainer, PruneImagesRequest, PullImageFromMachineRequest, ResolvedServiceSpec,
    RunTarget, StopOptions, VolumeRemoved,
};
use ployz_core::{
    AnswerWhen, HEALTH_POLL_INTERVAL, HealthFailure, HealthPoll, HealthVerdict, HealthcheckSpec,
    health_monitor, ready_poll,
};

use super::super::ingress::admit_ingress_service;
use super::{Error, LocalMachine};
use crate::docker::{ContainerCreated, ContainerRequest, ImageIngest, same_creation};
use crate::machine::{STORAGE_OBSERVATION_TIMEOUT, local_storage};

impl LocalMachine {
    /// Return fresh local storage evidence for container admission and Global reconciliation.
    pub(crate) async fn observe_storage(&self) -> Option<MachineStorageObservation> {
        local_storage(&self.zpool, STORAGE_OBSERVATION_TIMEOUT).await
    }

    /// Recheck the complete local placement and secure all provisioned Volumes before applications start.
    pub(crate) async fn prepare_volumes(
        &self,
        specs: Vec<ployz_core::ServiceStorageSpec>,
    ) -> Result<ployz_core::PreparedVolumes, Error> {
        use ployz_core::RawVolumeSource;
        let local = self.clone();
        self.finish_mutation(async move {
            let containers = local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
            let record = local.record();
            if !matches!(
                record.phase(),
                LocalMachinePhase::Joining | LocalMachinePhase::Participating
            ) {
                return Err(Error::NotParticipating);
            }
            let machine = record.machine().ok_or(Error::NotParticipating)?;
            let storage = local.observe_storage().await;
            let mut requested = std::collections::BTreeMap::new();
            for spec in &specs {
                crate::docker::require_eligible(
                    spec.placement_eligibility(machine, storage.as_ref()),
                )?;
                for volume in spec.volume_graph().mounted_provisioned_volumes() {
                    let RawVolumeSource::Provisioned {
                        name,
                        maximum_bytes,
                        ..
                    } = volume.source.kind()
                    else {
                        unreachable!("provisioned iterator")
                    };
                    if let Some(existing) = requested.insert(name.clone(), *maximum_bytes)
                        && existing != *maximum_bytes
                    {
                        return Err(ployz_core::StorageCapacityError::VolumeSizeConflict {
                            name: name.clone(),
                        }
                        .into_rpc_error()
                        .into());
                    }
                }
            }
            containers
                .validate_provisioned_volumes(&machine.id, &specs)
                .await?;
            for name in requested.keys() {
                local.admit_plain_mount(&machine.id, name).await?;
            }
            let names: Vec<ployz_core::DockerVolumeName> =
                local.plugin().call("Storage.Prepare", &requested).await?;
            containers
                .ensure_provisioned_volumes(&machine.id, &specs)
                .await?;
            Ok(ployz_core::PreparedVolumes { names })
        })
        .await
    }

    /// A plain Deploy mounts `name` only on its writer: this Machine's writable root, or
    /// nothing at all while no other Machine holds a copy.
    async fn admit_plain_mount(
        &self,
        me: &ployz_core::MachineId,
        name: &DockerVolumeName,
    ) -> Result<(), Error> {
        use ployz_core::{CopyRole, DockerVolumeStorageObservation, KnownCopy, SwitchError};
        let held: ployz_core::VolumeCopyView = self
            .plugin()
            .call(
                "Volume.Inspect",
                &ployz_core::InspectVolumeCopyRequest { name: name.clone() },
            )
            .await?;
        let replicated = self.replicated()?;
        let machines = replicated.machines().await?.observations;
        let machine_name =
            |id: &ployz_core::MachineId| match machines.iter().find(|machine| machine.id == *id) {
                Some(machine) => Some(machine.name.clone()),
                None => ployz_core::MachineName::parse(id.as_str()).ok(),
            };
        let elsewhere: Vec<KnownCopy> = replicated
            .volumes()
            .await?
            .observations
            .into_iter()
            .filter(|volume| volume.id.machine_id != *me && volume.id.name == *name)
            .filter_map(|volume| {
                let DockerVolumeStorageObservation::Provisioned { role, .. } = volume.storage
                else {
                    return None;
                };
                Some(KnownCopy {
                    machine: machine_name(&volume.id.machine_id)?,
                    role: role.unwrap_or(CopyRole::Writer),
                })
            })
            .collect();
        match ployz_core::admit_plain_mount(held.copy.as_ref(), &elsewhere) {
            Ok(()) => Ok(()),
            Err(SwitchError::NoWriter) => {
                // This Server's own slot is a copy Restore can name, beside any elsewhere.
                let mut copies = elsewhere;
                if let Some(ployz_core::VolumeCopy::Slot { .. }) = held.copy
                    && let Some(machine) = machine_name(me)
                {
                    copies.push(KnownCopy {
                        machine,
                        role: CopyRole::Slot,
                    });
                }
                Err(SwitchError::NoWriter
                    .rpc_error(ployz_core::no_writer_message(
                        ployz_core::Namespace::declared_volume_name(name).unwrap_or(name.as_str()),
                        &copies,
                    ))
                    .into())
            }
            Err(reason) => Err(reason
                .rpc_error(format!(
                    "Volume {name} is mid-run on this Machine; wait for the run to finish"
                ))
                .into()),
        }
    }

    /// Create a container after storage admission and deferred Machine-local validation.
    ///
    /// # Errors
    ///
    /// Returns when local state, networking, storage, or Docker cannot safely create the
    /// requested container.
    pub(crate) async fn create_container(
        &self,
        kind: ContainerKind,
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
        creation_key: Option<String>,
        deployment_id: Option<ployz_core::DeploymentLogId>,
        registry_auth: Option<ployz_core::RegistryAuth>,
    ) -> Result<ContainerCreated, Error> {
        // Once admitted, caller cancellation must not let reset overtake a Docker request.
        let (local, namespace, spec) = (self.clone(), namespace.clone(), spec.clone());
        self.finish_mutation(async move {
            local
                .create_container_admitted(
                    kind,
                    &namespace,
                    &spec,
                    creation_key,
                    deployment_id,
                    registry_auth,
                )
                .await
        })
        .await
    }

    /// Start one local container while holding Machine mutation admission.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or Docker cannot start the
    /// container.
    pub(crate) async fn start_container(&self, container_id: ContainerId) -> Result<(), Error> {
        let local = self.clone();
        self.finish_mutation(async move {
            local
                .containers
                .as_ref()
                .ok_or(Error::DockerUnavailable)?
                .start(&container_id)
                .await?;
            Ok(())
        })
        .await
    }

    /// Remove one local container while holding Machine mutation admission.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or Docker cannot remove the
    /// container.
    pub(crate) async fn remove_container(
        &self,
        container_id: ContainerId,
        remove_volumes: bool,
        force: bool,
    ) -> Result<(), Error> {
        let local = self.clone();
        self.finish_mutation(async move {
            local
                .containers
                .as_ref()
                .ok_or(Error::DockerUnavailable)?
                .remove(&container_id, remove_volumes, force)
                .await?;
            Ok(())
        })
        .await
    }

    /// Take one running managed Container out of ingress, wait `drain`, stop it.
    ///
    /// Contract: the caller holds `installation` and no admission turn. The
    /// helper takes the exclusive turn for the Docker stop itself, so a caller
    /// already holding a turn deadlocks.
    ///
    /// The stopping mark is set without a turn so replication tells every proxy
    /// while the drain sleeps. The stop clears the mark whether it succeeds or
    /// fails, so a Container that failed to stop is routed again. A daemon
    /// restart forgets the mark; a still-running Container returns to ingress
    /// and the resend redoes the drain.
    ///
    /// Polls no proxy and reads no other Machine.
    pub(crate) async fn stop_behind_ingress(
        &self,
        installation: &crate::mutation::MutationGuard,
        container: &ContainerId,
        drain: Drain,
        stop: &StopOptions,
    ) -> Result<ContainerObservation, Error> {
        let _ = installation;
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let machine_id = self.record().id();
        let observed = containers.inspect_managed(container, &machine_id).await?;
        if !is_running(&observed) {
            return Ok(observed);
        }
        containers.mark_stopping(*container);
        tokio::time::sleep(drain.as_duration()).await;
        let turn = self.owner.admission_lock().write_owned().await;
        let stopped = containers
            .stop(
                container,
                stop.signal.as_deref(),
                stop.grace_period_seconds.map(i32::from),
            )
            .await;
        drop(turn);
        stopped?;
        Ok(containers.inspect_managed(container, &machine_id).await?)
    }

    /// The cut's Container half: drain and stop every holder of the Volume, then run
    /// `zfs` under the exclusive turn once no holder runs. The holders are listed again
    /// under the turn, so one started during the drain is `Busy` too, as is one the
    /// stop left running. A `Busy` cut leaves the root a writer.
    ///
    /// Detached from the RPC with the installation held, so a caller that hangs up
    /// does not abandon a half-stopped Container.
    pub(crate) async fn cut_volume_copy<Z, Fut>(
        &self,
        name: DockerVolumeName,
        drain: Drain,
        zfs: Z,
    ) -> Result<Result<(), CopyRefusal>, Error>
    where
        Z: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), Error>> + Send + 'static,
    {
        let local = self.clone();
        self.finish_installed(move |installation| async move {
            let containers = local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
            let stop = StopOptions::default();
            for holder in containers.volume_holder_ids(&name).await? {
                let observed = local
                    .stop_behind_ingress(&installation, &holder, drain, &stop)
                    .await?;
                if is_running(&observed) {
                    return Ok(Err(CopyRefusal::Busy));
                }
            }
            let _turn = local.owner.admission_lock().write_owned().await;
            if local.volume_holder_running(&name).await? {
                return Ok(Err(CopyRefusal::Busy));
            }
            zfs().await.map(Ok)
        })
        .await
    }

    /// Whether Docker runs any Container that mounts the Volume `name`. A holder gone
    /// between the list and its inspect does not run.
    pub(crate) async fn volume_holder_running(
        &self,
        name: &DockerVolumeName,
    ) -> Result<bool, Error> {
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let machine_id = self.record().id();
        for holder in containers.volume_holder_ids(name).await? {
            match containers.inspect_managed(&holder, &machine_id).await {
                Ok(observed) if is_running(&observed) => return Ok(true),
                Ok(_) | Err(crate::docker::Error::ContainerNotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(false)
    }

    /// Remove one local Docker Volume while holding Machine mutation admission.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or Docker cannot remove the
    /// Volume.
    pub(crate) async fn remove_volume(
        &self,
        name: DockerVolumeName,
        force: bool,
    ) -> Result<VolumeRemoved, Error> {
        let local = self.clone();
        self.finish_mutation(async move {
            local
                .containers
                .as_ref()
                .ok_or(Error::DockerUnavailable)?
                .remove_volume(&name, force)
                .await?;
            Ok(VolumeRemoved {})
        })
        .await
    }

    /// Remove each repository's superseded build images while holding Machine mutation
    /// admission, so no Container is created from a reference as it is removed.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or Docker cannot list
    /// images or Containers.
    pub(crate) async fn prune_images(
        &self,
        request: PruneImagesRequest,
    ) -> Result<ImagesRemoved, Error> {
        let local = self.clone();
        self.finish_mutation(async move {
            Ok(local
                .containers
                .as_ref()
                .ok_or(Error::DockerUnavailable)?
                .prune_images(&request.repositories)
                .await?)
        })
        .await
    }

    /// Start or reconcile image ingest without taking a turn in Machine admission:
    /// a Build Grant mint answers while a Deploy or image transfer runs. The ingest
    /// serializes its own reconciliation, and daemon shutdown removes its helper, so a
    /// Reset racing it leaves at most a disposable helper behind.
    ///
    /// # Errors
    ///
    /// Returns when this Machine is not participating, an installation or upgrade is active,
    /// or the ingest helper cannot start.
    pub(crate) async fn ensure_image_ingest(
        &self,
        ingest: Arc<ImageIngest>,
    ) -> Result<ImageIngestOpened, Error> {
        let _installation = self.admit_installation()?;
        let record = self.record();
        let address = record
            .machine()
            .filter(|_| record.phase() == LocalMachinePhase::Participating)
            .map(|machine| machine.management_address())
            .ok_or_else(|| {
                Error::StoragePreparation(
                    ImageIngestReason::NotParticipating.rpc_error("Machine is not participating"),
                )
            })?;
        ingest
            .open(address)
            .await
            .map_err(Error::StoragePreparation)
    }

    /// Pull one image from peer ingest under shared admission: transfers run side by
    /// side, and exclusive work, such as Reset or image removal, waits for them.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or the image transfer fails.
    pub(crate) async fn pull_image_from_machine(
        &self,
        request: PullImageFromMachineRequest,
    ) -> Result<ImagePulled, Error> {
        let local = self.clone();
        let shared = self.owner.admission_lock().read_owned().await;
        self.finish_admitted(shared, async move {
            local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
            crate::docker::pull_from_ingest(&request.pull, request.source, &request.platform)
                .await?;
            Ok(ImagePulled {})
        })
        .await
    }

    /// Make one Container match `want` and answer with what is then observed. A
    /// Settled creation answers once the started Container's health settles.
    ///
    /// # Errors
    ///
    /// Returns when the Container cannot be observed as this Machine's own, Docker
    /// is unavailable, or an effect fails for a reason that is not a refusal.
    pub(crate) async fn ensure_container(
        &self,
        want: ContainerWant,
    ) -> Result<EnsuredContainer, Error> {
        let settled_spec = match &want {
            ContainerWant::Running {
                container: RunTarget::New(new),
            } if new.answer == AnswerWhen::Settled => Some(new.resolved_spec.clone()),
            _ => None,
        };
        let ensured = self.converge_container(want).await?;
        let Some(spec) = settled_spec else {
            return Ok(ensured);
        };
        Ok(match ensured {
            EnsuredContainer::Ensured {
                now: ContainerNow::Present(observed),
                health: None,
            } => {
                let health = self.settle_health(&observed.container_id, &spec).await?;
                EnsuredContainer::Ensured {
                    now: ContainerNow::Present(observed),
                    health: Some(health),
                }
            }
            other => other,
        })
    }

    /// Poll a started Container until it is healthy, fails, or reaches its deadline.
    /// A spec without its own healthcheck waits out the monitor window, the only sign
    /// a crash loop did not start.
    async fn settle_health(
        &self,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
    ) -> Result<HealthVerdict, Error> {
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let machine_id = self.record().id();
        let healthcheck = spec.container.healthcheck.as_ref();
        let checked = matches!(
            healthcheck,
            Some(HealthcheckSpec::Configured(_) | HealthcheckSpec::Http(_))
        );
        let monitor = health_monitor(spec);
        let started = tokio::time::Instant::now();
        loop {
            let details = containers
                .inspect_managed_details(container_id, &machine_id)
                .await?;
            let elapsed = started.elapsed();
            let wake = match ready_poll(healthcheck, monitor, &details, elapsed).0 {
                HealthPoll::Complete if checked || elapsed >= monitor => {
                    return Ok(HealthVerdict::Healthy);
                }
                HealthPoll::Complete => monitor,
                HealthPoll::PendingUntil(wake) => wake,
                HealthPoll::Failed(HealthFailure::Runtime {
                    observation,
                    last_check,
                }) => {
                    return Ok(HealthVerdict::Failed {
                        observation,
                        last_check,
                    });
                }
                HealthPoll::Failed(HealthFailure::TimedOut { last_check }) => {
                    return Ok(HealthVerdict::TimedOut { last_check });
                }
                // Classification never answers a cancellation; no caller cancels here.
                HealthPoll::Failed(HealthFailure::Cancelled) => {
                    return Ok(HealthVerdict::TimedOut { last_check: None });
                }
            };
            tokio::time::sleep_until(started + (elapsed + HEALTH_POLL_INTERVAL).min(wake)).await;
        }
    }

    async fn converge_container(&self, want: ContainerWant) -> Result<EnsuredContainer, Error> {
        let locator = want_locator(&want);
        let now = self.container_now(&locator).await?;
        let refusal = match plan_container(want, &now, &self.record().phase()) {
            ContainerStep::Done => {
                return Ok(EnsuredContainer::Ensured {
                    now: redacted(now),
                    health: None,
                });
            }
            ContainerStep::Refuse(refusal) => {
                return Ok(EnsuredContainer::Refused {
                    now: redacted(now),
                    refusal,
                });
            }
            ContainerStep::CreateAndStart(new) => self.create_and_start(new).await?,
            ContainerStep::Start(id) => {
                self.start_container(id).await?;
                None
            }
            ContainerStep::Stop { id, drain, stop } => {
                let local = self.clone();
                self.finish_installed(move |installation| async move {
                    local
                        .stop_behind_ingress(&installation, &id, drain, &stop)
                        .await?;
                    Ok(())
                })
                .await?;
                None
            }
            ContainerStep::Hold(id) => {
                let local = self.clone();
                self.finish_mutation(async move {
                    local
                        .containers
                        .as_ref()
                        .ok_or(Error::DockerUnavailable)?
                        .stop(&id, None, None)
                        .await?;
                    Ok(())
                })
                .await?;
                None
            }
            ContainerStep::StopThenRemove {
                id,
                drain,
                anonymous_volumes,
            } => {
                let local = self.clone();
                self.finish_installed(move |installation| async move {
                    local
                        .stop_behind_ingress(&installation, &id, drain, &StopOptions::default())
                        .await?;
                    let turn = local.owner.admission_lock().write_owned().await;
                    let removed = local
                        .containers
                        .as_ref()
                        .ok_or(Error::DockerUnavailable)?
                        .remove(&id, anonymous_volumes == AnonymousVolumes::Remove, false)
                        .await;
                    drop(turn);
                    removed?;
                    Ok(())
                })
                .await?;
                None
            }
            ContainerStep::Remove(id, anonymous_volumes) => {
                match self
                    .remove_container(id, anonymous_volumes == AnonymousVolumes::Remove, false)
                    .await
                {
                    Ok(_) | Err(Error::Docker(crate::docker::Error::ContainerNotFound(_))) => {}
                    Err(error) => return Err(error),
                }
                None
            }
        };
        let now = redacted(self.container_now(&locator).await?);
        Ok(match refusal {
            None => EnsuredContainer::Ensured { now, health: None },
            Some(refusal) => EnsuredContainer::Refused { now, refusal },
        })
    }

    async fn container_now(&self, locator: &ContainerLocator) -> Result<ContainerNow, Error> {
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let machine_id = self.record().id();
        let observed = match locator {
            ContainerLocator::Id(id) => match containers.inspect_managed(id, &machine_id).await {
                Ok(observed) => Some(observed),
                Err(crate::docker::Error::ContainerNotFound(_)) => None,
                Err(error) => return Err(error.into()),
            },
            ContainerLocator::Key(slot) => containers.inspect_slot(&machine_id, slot).await?,
        };
        Ok(observed.map_or_else(
            || ContainerNow::Absent {
                of: locator.clone(),
            },
            ContainerNow::Present,
        ))
    }

    async fn create_and_start(&self, new: NewContainer) -> Result<Option<ContainerRefusal>, Error> {
        let local = self.clone();
        let outcome = self
            .finish_mutation(async move {
                let created = local
                    .create_container_admitted(
                        new.slot.kind,
                        &new.slot.namespace,
                        &new.resolved_spec,
                        Some(new.slot.creation_key.as_str().to_owned()),
                        new.deployment_id,
                        new.registry_auth,
                    )
                    .await;
                let created = match created {
                    Ok(created) => created,
                    Err(error) => return create_refusal(error).map(Some),
                };
                let containers = local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
                // A racing create adopted a hook another request already started: never run it twice.
                if new.slot.kind == ContainerKind::PreDeployHook {
                    let adopted = containers
                        .inspect_managed(&created.container_id, &local.record().id())
                        .await?;
                    if !never_started(&adopted) {
                        return Ok(None);
                    }
                }
                containers.start(&created.container_id).await?;
                Ok(None)
            })
            .await?;
        Ok(outcome)
    }

    async fn create_container_admitted(
        &self,
        kind: ContainerKind,
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
        creation_key: Option<String>,
        deployment_id: Option<ployz_core::DeploymentLogId>,
        registry_auth: Option<ployz_core::RegistryAuth>,
    ) -> Result<ContainerCreated, Error> {
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let record = self.record();
        if !matches!(
            record.phase(),
            LocalMachinePhase::Joining | LocalMachinePhase::Participating
        ) {
            return Err(Error::NotParticipating);
        }
        let machine = record.machine().ok_or(Error::NotParticipating)?;
        containers
            .create_with_admission(
                machine,
                ContainerRequest {
                    creation_key: creation_key.as_deref(),
                    deployment_id: deployment_id.as_ref(),
                    registry_auth: registry_auth.as_ref(),
                    kind,
                    namespace,
                    spec,
                    admission: async { admit_ingress_service(namespace, spec) },
                    storage: self.observe_storage(),
                },
            )
            .await
    }
}

/// Whether Docker still runs the Container's process. Paused and restarting count as
/// running: both can write the Volume again without a start.
fn is_running(observed: &ContainerObservation) -> bool {
    matches!(
        observed.runtime,
        ContainerRuntimeObservation::Running { .. }
            | ContainerRuntimeObservation::Paused
            | ContainerRuntimeObservation::Restarting
    )
}

/// The observation as it goes on the wire: env and Config values redacted, as
/// ListContainers redacts them unless asked.
fn redacted(now: ContainerNow) -> ContainerNow {
    match now {
        ContainerNow::Present(mut observed) => {
            observed.redact_values();
            ContainerNow::Present(observed)
        }
        absent @ ContainerNow::Absent { .. } => absent,
    }
}

/// Whether Docker created the Container but never ran it. A one-shot hook in any
/// other state has already run, or is running, its command.
fn never_started(observed: &ContainerObservation) -> bool {
    matches!(observed.runtime, ContainerRuntimeObservation::Created)
}

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "a creation step carries the whole resolved spec, as the want it came from does"
)]
enum ContainerStep {
    Done,
    Refuse(ContainerRefusal),
    CreateAndStart(NewContainer),
    Start(ContainerId),
    /// Stops a Container already at rest, so its Server holds it there rather than
    /// restarting a stranded ingress.
    Hold(ContainerId),
    Stop {
        id: ContainerId,
        drain: Drain,
        stop: StopOptions,
    },
    StopThenRemove {
        id: ContainerId,
        drain: Drain,
        anonymous_volumes: AnonymousVolumes,
    },
    Remove(ContainerId, AnonymousVolumes),
}

fn plan_container(
    want: ContainerWant,
    now: &ContainerNow,
    phase: &LocalMachinePhase,
) -> ContainerStep {
    let present = match now {
        ContainerNow::Present(observed) => Some(observed),
        ContainerNow::Absent { .. } => None,
    };
    match want {
        ContainerWant::Running { .. }
            if !matches!(
                phase,
                LocalMachinePhase::Joining | LocalMachinePhase::Participating
            ) =>
        {
            ContainerStep::Refuse(refuse(
                ContainerRefusalReason::NotParticipating,
                "Machine is not participating",
            ))
        }
        ContainerWant::Running {
            container: RunTarget::New(new),
        } => match present {
            None => ContainerStep::CreateAndStart(new),
            Some(observed)
                if same_creation(
                    observed,
                    &new.slot.namespace,
                    new.slot.kind,
                    &new.resolved_spec,
                ) =>
            {
                match new.slot.kind {
                    ContainerKind::ServiceContainer => run_step(observed),
                    // Start an unstarted hook only through the admitted create, which
                    // reinspects it there: another request may have started it since.
                    ContainerKind::PreDeployHook if never_started(observed) => {
                        ContainerStep::CreateAndStart(new)
                    }
                    // A hook that ran once is adopted as it is, so its exit status is the reply.
                    ContainerKind::PreDeployHook => ContainerStep::Done,
                }
            }
            Some(_) => ContainerStep::Refuse(refuse(
                ContainerRefusalReason::SlotOccupied,
                "the slot holds a Container created from another spec",
            )),
        },
        ContainerWant::Running {
            container: RunTarget::Existing(locator),
        } => present.map_or_else(|| not_found(&locator), run_step),
        ContainerWant::Stopped {
            container,
            drain,
            stop,
        } => match present {
            None => not_found(&container),
            Some(observed) if is_running(observed) => ContainerStep::Stop {
                id: observed.container_id,
                drain,
                stop,
            },
            Some(observed) => ContainerStep::Hold(observed.container_id),
        },
        ContainerWant::Gone {
            container,
            drain,
            anonymous_volumes,
        } => match present {
            // The remove helper deletes a spec Docker no longer has a Container for. A slot
            // names no spec row without Docker, so an absent Key is already gone.
            None => match container {
                ContainerLocator::Id(id) => ContainerStep::Remove(id, anonymous_volumes),
                ContainerLocator::Key(_) => ContainerStep::Done,
            },
            Some(observed) if is_running(observed) => ContainerStep::StopThenRemove {
                id: observed.container_id,
                drain,
                anonymous_volumes,
            },
            Some(observed) => ContainerStep::Remove(observed.container_id, anonymous_volumes),
        },
    }
}

fn run_step(observed: &ContainerObservation) -> ContainerStep {
    if is_running(observed) {
        ContainerStep::Done
    } else {
        ContainerStep::Start(observed.container_id)
    }
}

fn not_found(locator: &ContainerLocator) -> ContainerStep {
    let message = match locator {
        ContainerLocator::Id(id) => format!("Container {id} does not exist on this Machine"),
        ContainerLocator::Key(slot) => format!(
            "no Container of {} holds slot {}",
            slot.namespace,
            slot.creation_key.as_str()
        ),
    };
    ContainerStep::Refuse(refuse(ContainerRefusalReason::NotFound, message))
}

fn refuse(reason: ContainerRefusalReason, message: impl Into<String>) -> ContainerRefusal {
    ContainerRefusal {
        reason,
        message: message.into(),
    }
}

fn want_locator(want: &ContainerWant) -> ContainerLocator {
    match want {
        ContainerWant::Running {
            container: RunTarget::New(new),
        } => ContainerLocator::Key(new.slot.clone()),
        ContainerWant::Running {
            container: RunTarget::Existing(locator),
        }
        | ContainerWant::Stopped {
            container: locator, ..
        }
        | ContainerWant::Gone {
            container: locator, ..
        } => locator.clone(),
    }
}

fn create_refusal(error: Error) -> Result<ContainerRefusal, Error> {
    use crate::docker::Error as Docker;
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "only create-path errors a driver can act on become refusals"
    )]
    let reason = match &error {
        Error::NotParticipating => ContainerRefusalReason::NotParticipating,
        Error::IngressProxyServiceSpec(_) => ContainerRefusalReason::InvalidIngress,
        Error::Docker(
            Docker::WorkNotAccepted
            | Docker::ServicePlacementMismatch
            | Docker::ProvisionedStorageUnsupported
            | Docker::StorageUnobservable,
        ) => ContainerRefusalReason::NotEligible,
        Error::Docker(Docker::SlotNameOccupied(_)) => ContainerRefusalReason::SlotOccupied,
        Error::Docker(Docker::ImagePull(_)) => ContainerRefusalReason::ImagePullFailed,
        Error::Docker(Docker::ExternalVolumeNotFound(_) | Docker::VolumeShapeMismatch { .. }) => {
            ContainerRefusalReason::VolumesNotPrepared
        }
        _ => return Err(error),
    };
    Ok(refuse(reason, ployz_core::error_chain::inline(&error)))
}

#[cfg(test)]
#[path = "container_tests.rs"]
mod tests;
