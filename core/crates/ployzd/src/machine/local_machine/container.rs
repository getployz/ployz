//! Machine-local container admission and creation.
//!
//! Creating a Service Container or hook, or preparing its storage, rechecks
//! placement against fresh local evidence and is refused when this Machine is
//! ineligible or unknown. Starting, stopping and removing existing Containers stay
//! available, and editing Machine Labels evicts nothing.

use std::sync::Arc;
use std::time::Duration;

use ployz_core::{
    AnonymousVolumes, ContainerId, ContainerKind, ContainerLimitsUpdated, ContainerLocator,
    ContainerNow, ContainerObservation, ContainerRefusal, ContainerRefusalReason,
    ContainerRuntimeObservation, ContainerWant, CopyRotation, DockerVolumeName, Drain,
    EnsuredContainer, ImageIngestOpened, ImageIngestReason, ImagePulled, ImagesRemoved,
    LocalMachinePhase, MachineStorageObservation, Namespace, NewContainer, PruneImagesRequest,
    PullImageFromMachineRequest, PullImageRequest, ResolvedServiceSpec, RotationEdge,
    RotationRefusal, RotationTarget, RunRotation, RunTarget, Start, StopOptions, VolumeGen,
    VolumeRemoved,
};
use ployz_core::{
    AnswerWhen, ContainerRun, DeploymentLogId, Gen, HealthFailure, HealthPoll, HealthVerdict,
    HealthcheckSpec, ReadyWait, SETTLE_INSPECT_GRACE, health_monitor, settle_deadline,
};

use self::rotation::RotationWant;
use super::super::ingress::admit_ingress_service;
use super::{Error, LocalMachine};
use crate::docker::{
    ContainerCreated, ContainerRequest, ContainerRuntime, ImageIngest, RotationTurn, Withdrawal,
    same_creation,
};
use crate::docker_image::ImagePull;
use crate::machine::{STORAGE_OBSERVATION_TIMEOUT, local_storage};

/// A settle inspects local Docker, so a short poll costs little and sets how late a
/// verdict is seen. Remote health waits keep `HEALTH_POLL_INTERVAL`.
const SETTLE_POLL: Duration = Duration::from_millis(100);

/// Reading a copy's sockets is cheap, so a short poll costs little and sets how late
/// an idle copy is seen.
const DRAIN_POLL: Duration = Duration::from_millis(50);

/// How long an idle copy stays withdrawn before its stop, so every Server's Internal
/// DNS stops answering it first: a withdrawal reached them 0.3 to 0.72 s later on a
/// Boat Cluster (#3688). A lookup before then still gets the old address, and a
/// client that connects to it after the stop hangs until its connect times out.
const WITHDRAWAL_PROPAGATION: Duration = Duration::from_secs(1);

/// Where a test holds an `ensure_container` call so a Leave can run against it.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnsurePausePoint {
    /// Before any Docker work.
    BeforeWork,
    /// After the copy was created and started, before its health settles.
    BeforeSettle,
}

/// A hold on `ensure_container`: the call signals `reached` at `point` and continues
/// once `release` is notified.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct EnsurePause {
    pub(crate) point: EnsurePausePoint,
    pub(crate) reached: Arc<tokio::sync::Notify>,
    pub(crate) release: Arc<tokio::sync::Notify>,
}

#[cfg(test)]
static SETTLES: std::sync::Mutex<std::collections::BTreeMap<ContainerId, usize>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Each copy a Settled creation found Healthy: the Deploy run that asked, and the
/// Docker run it saw.
#[derive(Clone, Default)]
pub(super) struct SettledCreations(
    Arc<std::sync::Mutex<std::collections::BTreeMap<ContainerId, (DeploymentLogId, ContainerRun)>>>,
);

/// Why the cut's Container half left the root a writer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CutRefusal {
    Busy,
    /// A newer lane raised the floor above the cut's `gen`.
    Stale(VolumeGen),
}

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
                    .rpc_error(ployz_core::no_writer_observation(
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
    #[cfg(test)]
    pub(crate) async fn create_container(
        &self,
        kind: ContainerKind,
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
        creation_key: Option<String>,
        deployment_id: Option<ployz_core::DeploymentLogId>,
        registry_auth: Option<ployz_core::RegistryAuth>,
    ) -> Result<ContainerCreated, Error> {
        let (local, namespace, spec) = (self.clone(), namespace.clone(), spec.clone());
        self.finish_mutation(async move {
            local
                .create_container_admitted(
                    kind,
                    &namespace,
                    &spec,
                    creation_key,
                    deployment_id,
                    Gen::FIRST,
                    ImagePull::Policy(registry_auth.as_ref()),
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

    /// A Writer's start: create its Container when absent, then start it, under one
    /// exclusive turn. A floor raised above `gen` starts nothing, since the newer lane
    /// may have started its own holder of the Volume already. A start ordered after
    /// the check waits for the turn, so it sees this holder running.
    ///
    /// # Errors
    ///
    /// Returns when the floor cannot be read, or Docker cannot create or start the
    /// Container.
    pub(crate) async fn start_volume_holder(
        &self,
        start: Start,
        name: DockerVolumeName,
        r#gen: VolumeGen,
    ) -> Result<Result<(), VolumeGen>, Error> {
        let local = self.clone();
        self.finish_mutation(async move {
            let containers = local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
            if let Err(floor) = containers.floor_turn().await.admit(&name, r#gen).await? {
                return Ok(Err(floor));
            }
            let container_id = match start {
                Start::Kept { container_id } => container_id,
                Start::Create {
                    namespace,
                    resolved_spec,
                    creation_key,
                    r#gen: copy_gen,
                    ..
                } => {
                    local
                        .create_container_admitted(
                            ContainerKind::ServiceContainer,
                            &namespace,
                            &resolved_spec,
                            Some(creation_key.as_str().to_owned()),
                            None,
                            copy_gen,
                            ImagePull::Policy(None),
                        )
                        .await?
                        .container_id
                }
            };
            containers.start(&container_id).await?;
            Ok(Ok(()))
        })
        .await
    }

    /// Remove one local container while holding Machine mutation admission.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or Docker cannot remove the
    /// container.
    #[cfg(test)]
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
    /// The stopping mark is set without a turn so replication tells every proxy and
    /// Internal DNS during the drain. An idle copy ends its drain early, though not
    /// before `WITHDRAWAL_PROPAGATION`, so Internal DNS has heard before the stop. A
    /// drain shorter than that, or a proxy slower to hear, may still dial the stopped
    /// copy and retry another. A daemon restart forgets the mark; a still-running
    /// Container returns to ingress and the resend redoes the drain.
    ///
    /// With a `fence`, the Container holds that Volume and the gen is checked against
    /// its floor once the drain ends, so a floor raised meanwhile returns the copy to
    /// ingress without waiting for a turn, and again under the turn: a Deploy that
    /// raised the floor meanwhile may already have answered Done on this holder, so a
    /// raised floor drops the Withdrawal and stops nothing. The floor lock is held from that check
    /// through the stop, so a floor raised mid-stop waits and its Deploy reads the
    /// holder stopped, not still running. A start ordered after that check waits for
    /// the turn, so it runs after the stop.
    ///
    /// Polls no proxy and reads no other Machine.
    pub(crate) async fn stop_behind_ingress(
        &self,
        installation: &crate::mutation::MutationGuard,
        container: &ContainerId,
        fence: Option<(&DockerVolumeName, VolumeGen)>,
        drain: Drain,
        stop: &StopOptions,
    ) -> Result<Result<ContainerObservation, VolumeGen>, Error> {
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let withdraw = || async move { Ok(Ok(containers.withdraw(*container))) };
        let admit = || async move {
            let Some((name, r#gen)) = fence else {
                return Ok(Ok(None));
            };
            let mut floor = containers.floor_turn().await;
            Ok(floor.admit(name, r#gen).await?.map(|()| Some(floor)))
        };
        let drained = || async move {
            let Some((name, r#gen)) = fence else {
                return Ok(Ok(()));
            };
            Ok(containers.floor_turn().await.admit(name, r#gen).await?)
        };
        self.stop_checked(
            installation,
            container,
            drain,
            stop,
            false,
            withdraw,
            drained,
            admit,
        )
        .await
    }

    /// The rotation is read before the withdrawal, so a copy In is never taken out of
    /// routing, and again under the turn: an In write that won the turn since the
    /// stop was planned keeps the copy running.
    async fn stop_out_of_rotation(
        &self,
        installation: &crate::mutation::MutationGuard,
        container: &ContainerId,
        target: RotationTarget,
        drain: Drain,
        stop: &StopOptions,
    ) -> Result<Result<ContainerObservation, ContainerRefusal>, Error> {
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let record = target == RotationTarget::Stopped;
        let target = &target;
        let withdraw = || async move {
            Ok(leave_rotation(containers, container, target)
                .await?
                .map(|_turn| containers.withdraw(*container)))
        };
        self.stop_checked(
            installation,
            container,
            drain,
            stop,
            record,
            withdraw,
            || async { Ok(Ok(())) },
            || leave_rotation(containers, container, target),
        )
        .await
    }

    /// Drains before the exclusive turn, then stops under it while holding what
    /// `check` returned. `drained` may refuse once the drain ends, so a refusal it can
    /// already see returns the copy to ingress before the turn, which other work may
    /// hold long. A refusal from `withdraw`, `drained` or `check` stops nothing.
    ///
    /// With `record`, a stop that ended a run records it, so its exit reads as a stop.
    /// The run is read after the stop, under the turn, so it is the one the stop ended
    /// even when the Container restarted during the drain. A run that exited before
    /// the stop was issued ended on its own, so its exit stays a crash.
    #[expect(
        clippy::too_many_arguments,
        reason = "one stop's facts and its three guards, shared by every stop path"
    )]
    async fn stop_checked<G, R, W, WFut, D, DFut, F, Fut>(
        &self,
        installation: &crate::mutation::MutationGuard,
        container: &ContainerId,
        drain: Drain,
        stop: &StopOptions,
        record: bool,
        withdraw: W,
        drained: D,
        check: F,
    ) -> Result<Result<ContainerObservation, R>, Error>
    where
        W: FnOnce() -> WFut,
        WFut: std::future::Future<Output = Result<Result<Withdrawal, R>, Error>>,
        D: FnOnce() -> DFut,
        DFut: std::future::Future<Output = Result<Result<(), R>, Error>>,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Result<G, R>, Error>>,
    {
        let _ = installation;
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let machine_id = self.record().id();
        let observed = containers.inspect_managed(container, &machine_id).await?;
        if !is_running(&observed.runtime) {
            return Ok(Ok(observed));
        }
        let _withdrawal = match withdraw().await? {
            Ok(withdrawal) => withdrawal,
            Err(refused) => return Ok(Err(refused)),
        };
        let spread = tokio::time::Instant::now() + WITHDRAWAL_PROPAGATION;
        let _ = tokio::time::timeout(
            drain.as_duration(),
            self.until_idle(containers, container, spread),
        )
        .await;
        if let Err(refused) = drained().await? {
            return Ok(Err(refused));
        }
        let turn = self.owner.admission_lock().write_owned().await;
        let held = match check().await? {
            Ok(held) => held,
            Err(refused) => return Ok(Err(refused)),
        };
        let issued = chrono::Utc::now();
        containers
            .stop(
                container,
                stop.signal.as_deref(),
                stop.grace_period_seconds.map(i32::from),
            )
            .await?;
        let ended = if record {
            containers.run_ended_since(container, issued).await?
        } else {
            None
        };
        drop(held);
        if let Some(run) = ended {
            containers.record_stop(container, &run).await?;
        }
        drop(turn);
        Ok(Ok(containers
            .inspect_managed(container, &machine_id)
            .await?))
    }

    /// Ends once the copy holds no connection it may still answer, per
    /// `drain_idle::holds_connection`, with the listening ports remembered across the
    /// drain's reads, and no earlier than `not_before`: a client may still connect
    /// until then, so idleness counts only when read at or after it. A copy with no
    /// connection from off its loopback reads idle at once. A copy whose sockets cannot be read never reads idle, so its drain waits
    /// the full cap.
    ///
    /// Every Service copy joins the Ployz network, so its pid's tables list only its
    /// own sockets. A copy in the host's namespace would list the host's, and read
    /// busy until the cap.
    async fn until_idle(
        &self,
        containers: &crate::docker::ContainerRuntime,
        container: &ContainerId,
        not_before: tokio::time::Instant,
    ) {
        let Ok(Some(pid)) = containers.pid(container).await else {
            return std::future::pending().await;
        };
        let mut listening = std::collections::BTreeSet::new();
        while !matches!(
            drain_idle::holds_connection(&self.proc_root, pid, &mut listening),
            Ok(false)
        ) || tokio::time::Instant::now() < not_before
        {
            tokio::time::sleep(DRAIN_POLL).await;
        }
    }

    /// The cut's Container half: drain and stop every holder of the Volume, then run
    /// `zfs` under the exclusive turn once no holder runs. The holders are listed again
    /// under the turn, so one started during the drain is `Busy` too, as is one the
    /// stop left running. A `Busy` cut leaves the root a writer.
    ///
    /// `gen` is checked against the floor before each holder's drain, after it (see
    /// `stop_behind_ingress`), and again under the floor lock, held from that last
    /// check through `zfs`, so a floor raised during the
    /// drain makes the cut `Stale` and a floor read never sees the flip half done. A
    /// `Stale` cut leaves the root a writer and stops no holder after the raise, since
    /// the raiser may have kept that holder or started it.
    ///
    /// Detached from the RPC with the installation held, so a caller that hangs up
    /// does not abandon a half-stopped Container.
    pub(crate) async fn cut_volume_copy<Z, Fut>(
        &self,
        name: DockerVolumeName,
        r#gen: VolumeGen,
        drain: Drain,
        zfs: Z,
    ) -> Result<Result<(), CutRefusal>, Error>
    where
        Z: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), Error>> + Send + 'static,
    {
        let local = self.clone();
        self.finish_installed(move |installation| async move {
            let containers = local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
            let stop = StopOptions::default();
            for holder in containers.volume_holder_ids(&name).await? {
                if let Err(floor) = containers.floor_turn().await.admit(&name, r#gen).await? {
                    return Ok(Err(CutRefusal::Stale(floor)));
                }
                let observed = match local
                    .stop_behind_ingress(&installation, &holder, Some((&name, r#gen)), drain, &stop)
                    .await?
                {
                    Ok(observed) => observed,
                    Err(floor) => return Ok(Err(CutRefusal::Stale(floor))),
                };
                if is_running(&observed.runtime) {
                    return Ok(Err(CutRefusal::Busy));
                }
            }
            let _turn = local.owner.admission_lock().write_owned().await;
            if local.volume_holder_running(&name).await? {
                return Ok(Err(CutRefusal::Busy));
            }
            let mut floor = containers.floor_turn().await;
            if let Err(floor) = floor.admit(&name, r#gen).await? {
                return Ok(Err(CutRefusal::Stale(floor)));
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
                Ok(observed) if is_running(&observed.runtime) => return Ok(true),
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

    /// Pull one image from its registry under shared admission, like a peer pull.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or the pull fails.
    pub(crate) async fn pull_image(&self, request: PullImageRequest) -> Result<ImagePulled, Error> {
        let local = self.clone();
        let shared = self.owner.admission_lock().read_owned().await;
        self.finish_admitted(shared, async move {
            let exposed_ports = local
                .containers
                .as_ref()
                .ok_or(Error::DockerUnavailable)?
                .pull_image(
                    &request.image,
                    request.policy,
                    request.registry_auth.as_ref(),
                )
                .await?;
            Ok(ImagePulled { exposed_ports })
        })
        .await
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
            let containers = local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
            if let Err(error) =
                crate::docker::pull_from_ingest(&request.pull, request.source, &request.platform)
                    .await
            {
                return Err(containers.pull_failure(error).await.into());
            }
            // Every caller discards this reply; a deploy reads ports from its `PullImage`.
            Ok(ImagePulled::default())
        })
        .await
    }

    /// Apply `spec`'s CPU and memory limits to Container `container_id` in place
    /// under shared admission, like a Container start.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or the Machine refuses
    /// or fails the update.
    pub(crate) async fn update_container_limits(
        &self,
        container_id: ContainerId,
        spec: ResolvedServiceSpec,
    ) -> Result<ContainerLimitsUpdated, Error> {
        let local = self.clone();
        let shared = self.owner.admission_lock().read_owned().await;
        self.finish_admitted(shared, async move {
            Ok(local
                .containers
                .as_ref()
                .ok_or(Error::DockerUnavailable)?
                .update_limits(&container_id, &spec)
                .await?)
        })
        .await
    }

    /// Make one Container match `want` and answer with what is then observed.
    ///
    /// A Leave stops and removes every copy itself, and this call answers exactly
    /// what it saw: a want that runs a copy ([`ContainerWant::Running`]) waits for
    /// the Leave's admission turn and runs as ever, a stop, a remove and a rotation
    /// write run as ever, and a copy the Leave took answers as missing. When a
    /// Running want did not deliver a running copy, by an error or by a verdict that
    /// is not Healthy, and a Leave is under way or has committed as it returns, the
    /// answer carries the Leave as an additive mark so the caller can name the cause:
    /// the error becomes the source of [`Error::Departing`], and the verdict names
    /// the Server under `departing`. A failed Leave leaves no mark. The mark never
    /// replaces anything, so a caller removes the copy an answer names, or the slot
    /// a failed start left, as it would for any failed start.
    ///
    /// # Errors
    ///
    /// Returns when the Container cannot be observed as this Machine's own, Docker
    /// is unavailable, or an effect fails for a reason that is not a refusal.
    pub(crate) async fn ensure_container(
        &self,
        want: ContainerWant,
    ) -> Result<EnsuredContainer, Error> {
        let runs = matches!(want, ContainerWant::Running { .. });
        #[cfg(test)]
        self.pause_for_test(EnsurePausePoint::BeforeWork).await;
        let ensured = match want {
            ContainerWant::Out {
                container,
                allow_empty,
                expect,
            } => {
                self.rotate(container, RotationWant::Out { allow_empty }, expect)
                    .await
            }
            ContainerWant::Held {
                container,
                run,
                expect,
            } => {
                self.rotate(container, RotationWant::Held(run), expect)
                    .await
            }
            ContainerWant::Running {
                container,
                rotation: RunRotation::In { run, expect },
            } => self.route_in(container, run, expect).await,
            want @ (ContainerWant::Running {
                rotation: RunRotation::Pending,
                ..
            }
            | ContainerWant::Stopped { .. }
            | ContainerWant::Gone { .. }) => self.ensure_docker(want).await,
        };
        if runs
            && self
                .containers
                .as_ref()
                .is_some_and(ContainerRuntime::departing)
        {
            return self.marked_departing(ensured);
        }
        ensured
    }

    /// `ensured`, marked with the Leave under way when it did not deliver a running
    /// copy: an error becomes the source of [`Error::Departing`], and a verdict that
    /// is not Healthy names the Server. A delivered copy, a refusal and a Machine with
    /// no record answer as they were.
    fn marked_departing(
        &self,
        ensured: Result<EnsuredContainer, Error>,
    ) -> Result<EnsuredContainer, Error> {
        let Some(server) = self.record().machine().map(|machine| machine.name.clone()) else {
            return ensured;
        };
        let departure = ployz_core::Departure { server };
        match ensured {
            Err(source) => Err(Error::Departing {
                server: departure.server,
                source: Box::new(source),
            }),
            Ok(EnsuredContainer::Ensured {
                now,
                health: Some(mut health),
            }) if health != HealthVerdict::Healthy => {
                if let HealthVerdict::Failed { departing, .. }
                | HealthVerdict::TimedOut { departing, .. } = &mut health
                {
                    *departing = Some(departure);
                }
                Ok(EnsuredContainer::Ensured {
                    now,
                    health: Some(health),
                })
            }
            other => other,
        }
    }

    /// Moves no rotation. An In want only re-checks itself before a start.
    async fn ensure_docker(&self, want: ContainerWant) -> Result<EnsuredContainer, Error> {
        let settled_spec = if let ContainerWant::Running {
            container: RunTarget::New(new),
            ..
        } = &want
            && new.answer == AnswerWhen::Settled
        {
            Some((new.resolved_spec.clone(), new.deployment_id.clone()))
        } else {
            None
        };
        let ensured = self.converge_container(want).await?;
        let Some((spec, deploy)) = settled_spec else {
            return Ok(ensured);
        };
        let EnsuredContainer::Ensured {
            now: ContainerNow::Present(observed),
            health: None,
        } = ensured
        else {
            return Ok(ensured);
        };
        let health = self
            .settle_health(&observed.container_id, &spec, deploy)
            .await?;
        Ok(EnsuredContainer::Ensured {
            now: ContainerNow::Present(observed),
            health: Some(health),
        })
    }

    async fn settle_health(
        &self,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
        deploy: Option<DeploymentLogId>,
    ) -> Result<HealthVerdict, Error> {
        #[cfg(test)]
        {
            *SETTLES
                .lock()
                .expect("settle counts are never poisoned")
                .entry(*container_id)
                .or_default() += 1;
            self.pause_for_test(EnsurePausePoint::BeforeSettle).await;
        }
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let machine_id = self.record().id();
        let healthcheck = spec.container.healthcheck.as_ref();
        let checked = matches!(
            healthcheck,
            Some(HealthcheckSpec::Configured(_) | HealthcheckSpec::Http(_))
        );
        let monitor = health_monitor(spec);
        let mut wait = ReadyWait::new(healthcheck, monitor);
        let started = tokio::time::Instant::now();
        let limit = settle_deadline(spec);
        let mut first_run = None;
        let mut seen = (None, false);
        loop {
            // Docker can stall an inspect of a Container it OOM-killed, and the
            // verdict still answers by the deadline.
            let Ok(inspected) = tokio::time::timeout_at(
                (started + limit).max(tokio::time::Instant::now() + SETTLE_INSPECT_GRACE),
                containers.inspect_managed_run(container_id, &machine_id),
            )
            .await
            else {
                let (last_check, oom_killed) = seen;
                return Ok(HealthVerdict::TimedOut {
                    deadline_millis: u64::try_from(limit.as_millis()).ok(),
                    last_check,
                    oom_killed,
                    departing: None,
                });
            };
            let inspected = inspected?;
            seen = (inspected.details.last_check.clone(), inspected.oom_killed);
            first_run = first_run.or_else(|| inspected.details.run.clone());
            let elapsed = started.elapsed();
            let wake = match wait.poll(&inspected.details, elapsed).0 {
                HealthPoll::Complete if checked || elapsed >= monitor => {
                    if let (Some(deploy), Some(run)) = (deploy, first_run) {
                        self.settled
                            .0
                            .lock()
                            .expect("settled creations are never poisoned")
                            .insert(*container_id, (deploy, run));
                    }
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
                        oom_killed: inspected.oom_killed,
                        departing: None,
                    });
                }
                HealthPoll::Failed(HealthFailure::TimedOut {
                    deadline_millis,
                    last_check,
                }) => {
                    return Ok(HealthVerdict::TimedOut {
                        deadline_millis,
                        last_check,
                        oom_killed: inspected.oom_killed,
                        departing: None,
                    });
                }
                HealthPoll::Failed(HealthFailure::Cancelled) => {
                    return Ok(HealthVerdict::TimedOut {
                        deadline_millis: None,
                        last_check: None,
                        oom_killed: false,
                        departing: None,
                    });
                }
            };
            tokio::time::sleep_until(started + (elapsed + SETTLE_POLL).min(wake)).await;
        }
    }

    /// Whether `deploy` found this copy Healthy at creation, in the Docker run it
    /// still runs, and the copy may still serve. Answers once per creation.
    pub(super) async fn settled_by(
        &self,
        container_id: &ContainerId,
        deploy: Option<&DeploymentLogId>,
    ) -> Result<bool, Error> {
        let Some(deploy) = deploy else {
            return Ok(false);
        };
        let settled = self
            .settled
            .0
            .lock()
            .expect("settled creations are never poisoned")
            .remove(container_id);
        let Some((settled_by, settled_run)) = settled else {
            return Ok(false);
        };
        if settled_by != *deploy {
            return Ok(false);
        }
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let details = containers
            .inspect_managed_details(container_id, &self.record().id())
            .await?;
        Ok(details.run.as_ref() == Some(&settled_run) && details.container.runtime.may_serve())
    }

    /// How many health settles this daemon ran for `container_id`.
    #[cfg(test)]
    pub(crate) fn settles_for_test(&self, container_id: &ContainerId) -> usize {
        SETTLES
            .lock()
            .expect("settle counts are never poisoned")
            .get(container_id)
            .copied()
            .unwrap_or_default()
    }

    /// Tell the test this call reached `point`, then wait for its release.
    #[cfg(test)]
    async fn pause_for_test(&self, point: EnsurePausePoint) {
        if let Some(pause) = &self.ensure_pause
            && pause.point == point
        {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    async fn converge_container(&self, want: ContainerWant) -> Result<EnsuredContainer, Error> {
        if matches!(
            want,
            ContainerWant::Gone {
                container: ContainerLocator::Key(_),
                ..
            }
        ) {
            // Detached so a caller that stops waiting still removes what the
            // admitted create puts in the slot.
            let local = self.clone();
            return tokio::spawn(async move {
                local.wait_for_admitted_work().await;
                local.converge_now(want).await
            })
            .await?;
        }
        self.converge_now(want).await
    }

    async fn converge_now(&self, want: ContainerWant) -> Result<EnsuredContainer, Error> {
        let locator = want_locator(&want);
        let fence = start_fence(&want);
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
            ContainerStep::CreateAndStart(new) => self.create_and_start(new, fence).await?,
            ContainerStep::Start(id) => match fence {
                Some((run, expect)) => self.start_routed(id, run, expect).await?,
                None => {
                    self.start_container(id).await?;
                    None
                }
            },
            ContainerStep::Stop { id, drain, stop } => {
                let local = self.clone();
                self.finish_installed(move |installation| async move {
                    Ok(local
                        .stop_out_of_rotation(
                            &installation,
                            &id,
                            RotationTarget::Stopped,
                            drain,
                            &stop,
                        )
                        .await?
                        .err())
                })
                .await?
            }
            ContainerStep::Hold(id) => {
                let local = self.clone();
                self.finish_mutation(async move {
                    let containers = local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
                    let turn =
                        match leave_rotation(containers, &id, &RotationTarget::Stopped).await? {
                            Ok(turn) => turn,
                            Err(refusal) => return Ok(Some(refusal)),
                        };
                    containers.stop(&id, None, None).await?;
                    drop(turn);
                    Ok(None)
                })
                .await?
            }
            ContainerStep::StopThenRemove {
                id,
                drain,
                anonymous_volumes,
            } => {
                let local = self.clone();
                self.finish_installed(move |installation| async move {
                    match local
                        .stop_out_of_rotation(
                            &installation,
                            &id,
                            RotationTarget::Gone,
                            drain,
                            &StopOptions::default(),
                        )
                        .await
                    {
                        Ok(Ok(_))
                        | Err(Error::Docker(crate::docker::Error::ContainerNotFound(_))) => {}
                        Ok(Err(refusal)) => return Ok(Some(refusal)),
                        Err(error) => return Err(error),
                    }
                    let _turn = local.owner.admission_lock().write_owned().await;
                    let containers = local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
                    removed(
                        remove_out_of_rotation(
                            containers,
                            &id,
                            anonymous_volumes == AnonymousVolumes::Remove,
                        )
                        .await,
                    )
                })
                .await?
            }
            ContainerStep::Remove(id, anonymous_volumes) => {
                let local = self.clone();
                self.finish_mutation(async move {
                    let containers = local.containers.as_ref().ok_or(Error::DockerUnavailable)?;
                    removed(
                        remove_out_of_rotation(
                            containers,
                            &id,
                            anonymous_volumes == AnonymousVolumes::Remove,
                        )
                        .await,
                    )
                })
                .await?
            }
        };
        let now = redacted(self.container_now(&locator).await?);
        Ok(match refusal {
            None => EnsuredContainer::Ensured { now, health: None },
            Some(refusal) => EnsuredContainer::Refused { now, refusal },
        })
    }

    async fn wait_for_admitted_work(&self) {
        drop(self.owner.admission_lock().write_owned().await);
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

    /// A create may adopt a copy another request made since the slot was read, so an
    /// In want's start is fenced like any start of an existing copy.
    async fn create_and_start(
        &self,
        new: NewContainer,
        fence: Option<StartFence>,
    ) -> Result<Option<ContainerRefusal>, Error> {
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
                        new.r#gen,
                        if new.image_prepared {
                            ImagePull::Prepared
                        } else {
                            ImagePull::Policy(new.registry_auth.as_ref())
                        },
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
                if let Some((run, expect)) = fence {
                    return rotation::start_fenced(
                        containers,
                        &created.container_id,
                        run,
                        expect.as_ref(),
                    )
                    .await;
                }
                containers.start(&created.container_id).await?;
                Ok(None)
            })
            .await?;
        Ok(outcome)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "A NewContainer's creation facts, also reached from Volume holders that have no NewContainer."
    )]
    async fn create_container_admitted(
        &self,
        kind: ContainerKind,
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
        creation_key: Option<String>,
        deployment_id: Option<ployz_core::DeploymentLogId>,
        r#gen: Gen,
        image: ImagePull<'_>,
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
                    r#gen,
                    image,
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
fn is_running(runtime: &ContainerRuntimeObservation) -> bool {
    matches!(
        runtime,
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
            ..
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
            ..
        } => present.map_or_else(|| not_found(&locator), run_step),
        ContainerWant::Stopped {
            container,
            drain,
            stop,
        } => match present {
            None => not_found(&container),
            Some(observed) if in_rotation(observed.rotation(), &RotationTarget::Stopped) => {
                ContainerStep::Refuse(in_rotation_refusal(
                    &observed.container_id,
                    observed.rotation(),
                ))
            }
            Some(observed) if is_running(&observed.runtime) => ContainerStep::Stop {
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
            Some(observed) if in_rotation(observed.rotation(), &RotationTarget::Gone) => {
                ContainerStep::Refuse(in_rotation_refusal(
                    &observed.container_id,
                    observed.rotation(),
                ))
            }
            Some(observed) if is_running(&observed.runtime) => ContainerStep::StopThenRemove {
                id: observed.container_id,
                drain,
                anonymous_volumes,
            },
            Some(observed) => ContainerStep::Remove(observed.container_id, anonymous_volumes),
        },
        ContainerWant::Out { .. } | ContainerWant::Held { .. } => {
            unreachable!("a rotation want moves no Docker state")
        }
    }
}

/// The run and rotation an In want's start must still match.
type StartFence = (Option<ployz_core::DeploymentLogId>, Option<CopyRotation>);

fn start_fence(want: &ContainerWant) -> Option<StartFence> {
    match want {
        ContainerWant::Running {
            rotation: RunRotation::In { run, expect },
            ..
        } => Some((run.clone(), expect.clone())),
        ContainerWant::Running {
            rotation: RunRotation::Pending,
            ..
        }
        | ContainerWant::Stopped { .. }
        | ContainerWant::Gone { .. }
        | ContainerWant::Out { .. }
        | ContainerWant::Held { .. } => None,
    }
}

/// True when the copy must leave routing through an Out want first.
fn in_rotation(rotation: &CopyRotation, target: &RotationTarget) -> bool {
    matches!(
        rotation.edge(target),
        RotationEdge::Refused(RotationRefusal::InRotation)
    )
}

fn in_rotation_refusal(container: &ContainerId, rotation: &CopyRotation) -> ContainerRefusal {
    refuse(
        ContainerRefusalReason::InRotation,
        format!(
            "Container {container} is {}; take it out of routing first",
            rotation::describe(rotation)
        ),
    )
}

async fn leave_rotation<'a>(
    containers: &'a crate::docker::ContainerRuntime,
    container: &ContainerId,
    target: &RotationTarget,
) -> Result<Result<RotationTurn<'a>, ContainerRefusal>, Error> {
    let turn = containers.rotation_turn(container).await?;
    Ok(if in_rotation(turn.rotation(), target) {
        Err(in_rotation_refusal(container, turn.rotation()))
    } else {
        Ok(turn)
    })
}

/// A Container Docker no longer has serves nothing, so only its records go.
async fn remove_out_of_rotation(
    containers: &crate::docker::ContainerRuntime,
    container: &ContainerId,
    remove_volumes: bool,
) -> Result<Option<ContainerRefusal>, Error> {
    match leave_rotation(containers, container, &RotationTarget::Gone).await {
        Ok(Ok(turn)) => turn.remove(remove_volumes, false).await?,
        Ok(Err(refusal)) => return Ok(Some(refusal)),
        Err(Error::Docker(crate::docker::Error::ContainerNotFound(_))) => {
            containers.remove(container, remove_volumes, false).await?;
        }
        Err(error) => return Err(error),
    }
    Ok(None)
}

fn removed(
    outcome: Result<Option<ContainerRefusal>, Error>,
) -> Result<Option<ContainerRefusal>, Error> {
    match outcome {
        Err(Error::Docker(crate::docker::Error::ContainerNotFound(_))) => Ok(None),
        outcome => outcome,
    }
}

fn run_step(observed: &ContainerObservation) -> ContainerStep {
    if is_running(&observed.runtime) {
        ContainerStep::Done
    } else {
        ContainerStep::Start(observed.container_id)
    }
}

fn not_found(locator: &ContainerLocator) -> ContainerStep {
    ContainerStep::Refuse(not_found_refusal(locator))
}

fn not_found_refusal(locator: &ContainerLocator) -> ContainerRefusal {
    let message = match locator {
        ContainerLocator::Id(id) => format!("Container {id} does not exist on this Machine"),
        ContainerLocator::Key(slot) => format!(
            "no Container of {} holds slot {}",
            slot.namespace,
            slot.creation_key.as_str()
        ),
    };
    refuse(ContainerRefusalReason::NotFound, message)
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
            ..
        } => ContainerLocator::Key(new.slot.clone()),
        ContainerWant::Running {
            container: RunTarget::Existing(locator),
            ..
        }
        | ContainerWant::Stopped {
            container: locator, ..
        }
        | ContainerWant::Gone {
            container: locator, ..
        }
        | ContainerWant::Out {
            container: locator, ..
        }
        | ContainerWant::Held {
            container: locator, ..
        } => locator.clone(),
    }
}

fn create_refusal(error: Error) -> Result<ContainerRefusal, Error> {
    use crate::{docker::Error as Docker, sealed_env::OpenError};
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
        Error::Docker(Docker::ImagePull(_) | Docker::DiskFull { .. }) => {
            ContainerRefusalReason::ImagePullFailed
        }
        Error::Docker(Docker::ExternalVolumeNotFound(_) | Docker::VolumeShapeMismatch { .. }) => {
            ContainerRefusalReason::VolumesNotPrepared
        }
        Error::Docker(Docker::Sealed(open)) => match open {
            OpenError::KeyMissing => ContainerRefusalReason::SealingKeyMissing,
            OpenError::KeyUnreadable(_) => return Err(error),
            OpenError::MissingIdentity(_)
            | OpenError::SealedPort
            | OpenError::BadName(_)
            | OpenError::Unbound(_)
            | OpenError::Unopenable(_)
            | OpenError::ShortLine(_) => ContainerRefusalReason::SealedValueUnopenable,
        },
        _ => return Err(error),
    };
    Ok(refuse(reason, ployz_core::error_chain::inline(&error)))
}

#[path = "drain_idle.rs"]
mod drain_idle;
#[path = "rotation.rs"]
mod rotation;

#[cfg(test)]
#[path = "container_tests.rs"]
mod tests;
