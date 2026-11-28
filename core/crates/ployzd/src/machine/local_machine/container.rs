//! Machine-local container admission and creation.
//!
//! Creating a Service Container or hook, or preparing its storage, rechecks
//! placement against fresh local evidence and is refused when this Machine is
//! ineligible or unknown. Starting, stopping and removing existing Containers stay
//! available, and editing Machine Labels evicts nothing.

use crate::machine::has_cluster_identity;
use ployz_core::authority::MismatchField;
use std::sync::Arc;
use std::time::Duration;

use ployz_core::{
    AnonymousVolumes, ContainerId, ContainerKind, ContainerLimitsUpdated, ContainerLocator,
    ContainerNow, ContainerObservation, ContainerPlacementUpdated, ContainerRefusal,
    ContainerRefusalReason, ContainerRuntimeObservation, ContainerWant, CopyRotation,
    DockerVolumeName, Drain, EnsuredContainer, ImageIngestOpened, ImageIngestReason, ImagePulled,
    ImagesRemoved, KeptVolumes, KeptVolumesRemoved, ListKeptVolumesRequest, LocalMachinePhase,
    MachineStorageObservation, Namespace, NewContainer, PruneImagesRequest, PullFreeImageRequest,
    PullImageFromMachineRequest, PullImageRequest, QualifiedService, RemoveKeptVolumesRequest,
    ReservedKind, ResolvedServiceSpec, RotationTarget, RunRotation, RunTarget, Start, StopOptions,
    VolumeGen, VolumeRemoved,
};
use ployz_core::{
    AnswerWhen, ContainerRun, DeploymentLogId, Gen, HealthFailure, HealthPoll, HealthVerdict,
    HealthcheckSpec, ReadyWait, SETTLE_INSPECT_GRACE, health_monitor, settle_deadline,
};

use super::super::ingress::{admit_door, admit_router, admit_unreserved};
use super::{
    AdmittedSpec, Error, LocalMachine, Scope, SwitchGuard,
    turns::{ExclusiveTurn, Installed, SharedTurn},
};
use crate::docker::{
    ContainerCreated, ContainerRequest, ContainerRuntime, ImageIngest, RotationTurn, Withdrawal,
    same_creation,
};
use crate::docker_image::{Credential, ImagePull};
use crate::host_facts::{STORAGE_OBSERVATION_TIMEOUT, local_storage};
use crate::images::DoorSpecs;
use ployz_decide::rotation::{RotationWant, in_rotation, in_rotation_refusal};

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

/// How long a stopped container's `json-file` log outlives its stop, so the Collector
/// reads the last lines before removal deletes the file. A container the Collector never
/// opened lost 70 to 90% of its lines at 0 s and none at 1 s or 2 s.
const LOG_READ_GRACE: Duration = Duration::from_secs(2);

fn log_read_remaining(
    exited_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Duration {
    let since_exit = exited_at
        .and_then(|exited_at| (now - exited_at).to_std().ok())
        .unwrap_or(Duration::ZERO);
    LOG_READ_GRACE.saturating_sub(since_exit)
}

/// Whether the container ran a Service's process, whose last lines a removal must not
/// cut off. A failed start's lines are the ones that say why it did not come up, so the
/// drain does not matter. A hook's output reaches its caller in the reply, and a
/// container Docker never started has no lines.
fn has_logs_to_read(observed: &ContainerObservation) -> bool {
    observed.kind == ContainerKind::ServiceContainer && !never_started(observed)
}

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

impl SettledCreations {
    #[cfg(test)]
    pub(super) fn is_shared_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Why the cut's Container half left the root a writer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CutRefusal {
    Busy,
    /// A newer lane raised the floor above the cut's `gen`.
    Stale(VolumeGen),
    /// The named Container runs on the root beside the one the cut may stop.
    Held(String),
    /// The one Container the cut may stop no longer runs, and no cut at this `gen`
    /// stopped it.
    Stopped,
}

impl LocalMachine {
    /// Return fresh local storage evidence for container admission and Global reconciliation.
    pub(crate) async fn observe_storage(&self) -> Option<MachineStorageObservation> {
        local_storage(&self.zpool, STORAGE_OBSERVATION_TIMEOUT).await
    }

    /// Recheck the complete local placement and secure all provisioned Volumes before applications start.
    ///
    /// Holds the switch lock, as a Forget does, from before the admission turn through
    /// provisioning, so no Forget lands between the read of the forgotten record and
    /// the read of the copy.
    pub(crate) async fn prepare_volumes(
        &self,
        specs: Vec<ployz_core::ServiceStorageSpec>,
    ) -> Result<ployz_core::PreparedVolumes, Error> {
        use ployz_core::RawVolumeSource;
        let switch = self.lock_switch().await?;
        self.finish_mutation(move |local| async move {
            let _switch = switch;
            let containers = local.docker()?;
            let record = local.record();
            let machine = record.cluster_machine().ok_or(Error::NotParticipating)?;
            for name in specs.iter().flat_map(|spec| {
                spec.volume_graph()
                    .mounted_volumes()
                    .filter_map(|volume| volume.source.managed_docker_volume_name())
            }) {
                local.open_preview_arrival(name)?;
            }
            let storage = local.observe_storage().await;
            let mut requested = std::collections::BTreeMap::new();
            let mut mounted = std::collections::BTreeMap::new();
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
                    mounted.entry(name.clone()).or_insert(volume);
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
                .volumes()
                .validate_provisioned_volumes(&machine.id, &specs)
                .await?;
            for (name, volume) in &mounted {
                let forgot = containers.lifecycle().forgot_here(volume).await?;
                local.admit_plain_mount(&machine.id, name, forgot).await?;
            }
            let names: Vec<ployz_core::DockerVolumeName> = local
                .plugin()
                .call(crate::volume_copy::STORAGE_PREPARE_ROUTE, &requested)
                .await?;
            containers
                .volumes()
                .ensure_provisioned_volumes(&machine.id, &specs)
                .await?;
            Ok(ployz_core::PreparedVolumes { names })
        })
        .await
    }

    /// A plain Deploy mounts `name` only on its writer: this Machine's writable root, or
    /// nothing at all while no other Machine holds a copy. `forgot` says this Machine
    /// forgot a copy of the data, which then lives elsewhere however late the view.
    async fn admit_plain_mount(
        &self,
        me: &ployz_core::MachineId,
        name: &DockerVolumeName,
        forgot: bool,
    ) -> Result<(), Error> {
        use ployz_core::{CopyRole, DockerVolumeStorageObservation, KnownCopy, SwitchError};
        let held: ployz_core::VolumeCopyView = self
            .plugin()
            .call(
                crate::volume_copy::VOLUME_INSPECT_ROUTE,
                &ployz_core::InspectVolumeCopyRequest {
                    name: name.clone(),
                    prove: false,
                },
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
        let admitted = if forgot && held.copy.is_none() {
            Err(SwitchError::NoWriter)
        } else {
            ployz_core::admit_plain_mount(held.copy.as_ref(), &elsewhere)
        };
        match admitted {
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
        let admitted = AdmittedSpec::test_signed(namespace, spec);
        self.create_container_for_test(
            kind,
            namespace,
            spec,
            creation_key,
            admitted,
            (deployment_id, registry_auth),
        )
        .await
    }

    /// [`Self::create_container`] under `admitted`, for a test that needs a Container
    /// another key made.
    #[cfg(test)]
    pub(crate) async fn create_container_for_test(
        &self,
        kind: ContainerKind,
        namespace: &Namespace,
        spec: &ResolvedServiceSpec,
        creation_key: Option<String>,
        admitted: AdmittedSpec,
        (deployment_id, registry_auth): (
            Option<ployz_core::DeploymentLogId>,
            Option<ployz_core::RegistryAuth>,
        ),
    ) -> Result<ContainerCreated, Error> {
        let (namespace, spec) = (namespace.clone(), spec.clone());
        self.finish_mutation(move |local| async move {
            local
                .create_container_admitted(CreationFacts {
                    kind,
                    namespace: &namespace,
                    spec: &spec,
                    creation_key: creation_key
                        .unwrap_or_else(|| format!("test:{}", ployz_core::MachineId::random())),
                    deployment_id,
                    r#gen: Gen::FIRST,
                    image: ImagePull::Policy(Credential::Auth(registry_auth.as_ref())),
                    admitted,
                    replaces: None,
                })
                .await
        })
        .await
    }

    /// Start one local container while holding Machine mutation admission, and `switch`
    /// until the start ends, even when the caller stops waiting.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or Docker cannot start the
    /// container.
    pub(crate) async fn start_container(
        &self,
        container_id: ContainerId,
        switch: Option<SwitchGuard>,
    ) -> Result<(), Error> {
        self.finish_mutation(move |local| async move {
            let _switch = switch;
            local.start_kept(local.docker()?, &container_id).await
        })
        .await
    }

    /// Place the image a Writer start's Create runs, under shared admission like
    /// [`Self::pull_free_image`], and read its ID, so that Create, after the promote,
    /// runs that ID and reaches no registry. The ID the admission resolved, as a
    /// registry rule's pull does, comes first, and a promote's `recorded` ID must agree
    /// with it. A signed start the admission resolved no ID for takes the `recorded`
    /// one, or else pulls `spec`'s image with the Service's sealed `login`. An unsigned
    /// start pulls as [`Images::pull_free`] does whatever the record says, since only
    /// that pull proves an image public.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, the login or pull is
    /// refused or fails, Docker no longer holds the image chosen, or the `recorded` ID
    /// is not the one the admission resolved.
    ///
    /// [`Images::pull_free`]: crate::docker::Images::pull_free
    pub(crate) async fn place_holder_image(
        &self,
        spec: ResolvedServiceSpec,
        admitted: AdmittedSpec,
        recorded: Option<String>,
        login: Option<ployz_core::SealedLogin>,
    ) -> Result<PlacedHolder, Error> {
        self.finish_shared(move |local| async move {
            let docker = local.docker()?;
            let images = docker.images();
            let (image, policy) = (&spec.container.image, spec.container.pull_policy);
            let id = match (admitted.preview_image(), admitted.pin(), recorded) {
                // A preview's admission pulled and inspected its image once: its checks
                // read that ID, so the Create runs it and nothing pulls the tag again.
                (Some(preview), _, _) => preview.id().to_owned(),
                (None, Some(resolved), Some(recorded)) if resolved.as_str() != recorded => {
                    return Err(crate::docker::Error::PlacedImageDisagrees {
                        recorded,
                        resolved: resolved.as_str().to_owned(),
                    }
                    .into());
                }
                (None, Some(resolved), _) => images.placed(resolved.as_str()).await?,
                (None, None, Some(id)) if admitted.signed() => images.placed(&id).await?,
                (None, None, _) if admitted.signed() => {
                    let credential = match &login {
                        Some(login) => Credential::Sealed {
                            login,
                            sealing: docker.sealing(),
                        },
                        None => Credential::Auth(None),
                    };
                    images.place(image, policy, credential).await?
                }
                (None, None, _) => images.pull_free(image, policy).await?,
            };
            Ok(PlacedHolder { admitted, id })
        })
        .await
    }

    /// A Writer's start: create its Container when absent, then start it, under one
    /// exclusive turn. A floor raised above `gen` starts nothing, since the newer lane
    /// may have started its own holder of the Volume already. A start ordered after
    /// the check waits for the turn, so it sees this holder running. A Create runs the
    /// image `placed` before the promote, and pulls nothing.
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
        placed: Option<PlacedHolder>,
    ) -> Result<Result<(), VolumeGen>, Error> {
        self.volume_holder(start, name, r#gen, placed, true).await
    }

    /// [`Self::start_volume_holder`]'s create alone: the Container is made and left
    /// stopped. A preview's Writer start makes it before the promote, so whatever refuses
    /// the Container refuses it with the copy as it was.
    ///
    /// # Errors
    ///
    /// As [`Self::start_volume_holder`].
    pub(crate) async fn create_volume_holder(
        &self,
        start: Start,
        name: DockerVolumeName,
        r#gen: VolumeGen,
        placed: Option<PlacedHolder>,
    ) -> Result<Result<(), VolumeGen>, Error> {
        self.volume_holder(start, name, r#gen, placed, false).await
    }

    async fn volume_holder(
        &self,
        start: Start,
        name: DockerVolumeName,
        r#gen: VolumeGen,
        placed: Option<PlacedHolder>,
        run: bool,
    ) -> Result<Result<(), VolumeGen>, Error> {
        self.finish_mutation(move |local| async move {
            let containers = local.docker()?;
            if let Err(floor) = containers
                .lifecycle()
                .floor_turn()
                .await
                .admit(&name, r#gen)
                .await?
            {
                return Ok(Err(floor));
            }
            let container_id = match start {
                Start::Kept { container_id } => {
                    if run {
                        local.start_kept(containers, &container_id).await?;
                    }
                    return Ok(Ok(()));
                }
                Start::Create {
                    namespace,
                    resolved_spec,
                    creation_key,
                    r#gen: copy_gen,
                    ..
                } => {
                    let PlacedHolder { admitted, id } = placed.ok_or_else(unadmitted)?;
                    local
                        .create_container_admitted(CreationFacts {
                            kind: ContainerKind::ServiceContainer,
                            namespace: &namespace,
                            spec: &resolved_spec,
                            creation_key: creation_key.as_str().to_owned(),
                            deployment_id: None,
                            r#gen: copy_gen,
                            image: ImagePull::Placed(&id),
                            admitted,
                            replaces: None,
                        })
                        .await?
                        .container_id
                }
            };
            if run {
                local.start_checked(containers, &container_id).await?;
            }
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
        volumes: crate::docker::VolumeDisposal,
        force: bool,
    ) -> Result<(), Error> {
        self.finish_mutation(move |local| async move {
            local
                .docker()?
                .lifecycle()
                .remove(&container_id, volumes, force)
                .await?;
            Ok(())
        })
        .await
    }
}

impl Installed {
    /// Take one running managed Container out of ingress, wait `drain`, stop it.
    ///
    /// The helper takes the exclusive turn for the Docker stop itself, so a caller
    /// holding a turn deadlocks, and no other stop or start runs beside it.
    ///
    /// The stopping mark is set without a turn so replication tells every proxy and
    /// Internal DNS during the drain. It is set under the config lock, so a removal
    /// that took a replaced copy's anonymous volumes on this Container's word finished
    /// before it. An idle copy ends its drain early, though not
    /// before `WITHDRAWAL_PROPAGATION`, so Internal DNS has heard before the stop. A
    /// drain shorter than that, or a proxy slower to hear, may still dial the stopped
    /// copy and retry another. A daemon restart forgets the mark; a still-running
    /// Container returns to ingress and the resend redoes the drain.
    ///
    /// With a `fence`, the Container holds that Volume and the gen is checked against
    /// its floor once the drain ends, so a floor raised meanwhile returns the copy to
    /// ingress without waiting for a turn, and again under the turn: a Deploy that
    /// raised the floor meanwhile may already have answered Done on this holder, so a
    /// raised floor drops the Withdrawal and stops nothing. The floor lock is held from
    /// that check through the stop, so a floor raised mid-stop waits and its Deploy
    /// reads the holder stopped, not still running. A start ordered after that check
    /// waits for the turn, so it runs after the stop.
    ///
    /// Polls no proxy and reads no other Machine.
    pub(crate) async fn stop_behind_ingress(
        &self,
        container: &ContainerId,
        fence: Option<(&DockerVolumeName, VolumeGen)>,
        drain: Drain,
        stop: &StopOptions,
    ) -> Result<Result<ContainerObservation, VolumeGen>, Error> {
        let guard = Fence {
            containers: self.docker()?,
            container,
            fence,
        };
        self.stop_checked(container, drain, stop, guard).await
    }

    /// The rotation is read before the withdrawal, so a copy In is never taken out of
    /// routing, and again under a shared turn: an In write that won the exclusive turn
    /// since the stop was planned keeps the copy running. Stops of other copies run beside
    /// this one, while an In write or a start waits for the exclusive turn until it ends.
    async fn stop_out_of_rotation(
        &self,
        container: &ContainerId,
        target: RotationTarget,
        drain: Drain,
        stop: &StopOptions,
    ) -> Result<Result<ContainerObservation, ContainerRefusal>, Error> {
        let guard = Leaving {
            containers: self.docker()?,
            container,
            target: &target,
        };
        self.stop_checked(container, drain, stop, guard).await
    }

    /// Drains before the guard's turn, then stops under it while holding what the
    /// guard's `check` returned. The guard's `drained` may refuse once the drain ends,
    /// so a refusal it can already see returns the copy to ingress before the turn,
    /// which other work may hold long. A refusal from `withdraw`, `drained` or `check`
    /// stops nothing.
    ///
    /// With a guard that `records`, a stop that ended a run records it, so its exit
    /// reads as a stop. The run is read after the stop, under the turn, so it is the
    /// one the stop ended even when the Container restarted during the drain. A run
    /// that exited before the stop was issued ended on its own, so its exit stays a
    /// crash.
    async fn stop_checked<G: StopGuard>(
        &self,
        container: &ContainerId,
        drain: Drain,
        stop: &StopOptions,
        guard: G,
    ) -> Result<Result<ContainerObservation, G::Refusal>, Error> {
        let containers = self.docker()?;
        let machine_id = self.record().id();
        let observed = containers
            .observation()
            .inspect_managed(container, &machine_id)
            .await?;
        if !is_running(&observed.runtime) {
            return Ok(Ok(observed));
        }
        let _withdrawal = match guard.withdraw().await? {
            Ok(withdrawal) => withdrawal,
            Err(refused) => return Ok(Err(refused)),
        };
        let spread = tokio::time::Instant::now() + WITHDRAWAL_PROPAGATION;
        let _ = tokio::time::timeout(
            drain.as_duration(),
            self.until_idle(containers, container, spread),
        )
        .await;
        if let Err(refused) = guard.drained().await? {
            return Ok(Err(refused));
        }
        let turn = guard.turn(self).await?;
        let held = match guard.check().await? {
            Ok(held) => held,
            Err(refused) => return Ok(Err(refused)),
        };
        let issued = chrono::Utc::now();
        containers
            .lifecycle()
            .stop(
                container,
                stop.signal.as_deref(),
                stop.grace_period_seconds.map(i32::from),
            )
            .await?;
        let ended = if guard.records() {
            containers
                .observation()
                .run_ended_since(container, issued)
                .await?
        } else {
            None
        };
        drop(held);
        if let Some(run) = ended {
            containers.lifecycle().record_stop(container, &run).await?;
        }
        drop(turn);
        Ok(Ok(containers
            .observation()
            .inspect_managed(container, &machine_id)
            .await?))
    }
}

impl LocalMachine {
    /// Ends once the copy holds no connection it may still answer, per
    /// `drain_idle::holds_connection`, with the listening ports remembered across the
    /// drain's reads, and no earlier than `not_before`: a client may still connect
    /// until then, so idleness counts only when read at or after it. A copy with no
    /// connection from off its loopback reads idle, so its drain ends at `not_before`.
    /// A copy whose sockets cannot be read never reads idle, so its drain waits the
    /// full cap.
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
        let Ok(Some(pid)) = containers.observation().pid(container).await else {
            return std::future::pending().await;
        };
        let mut listening = std::collections::BTreeSet::new();
        loop {
            let late = tokio::time::Instant::now() >= not_before;
            let idle = matches!(
                drain_idle::holds_connection(&self.proc_root, pid, &mut listening),
                Ok(false)
            );
            if late && idle {
                return;
            }
            tokio::time::sleep(DRAIN_POLL).await;
        }
    }
}

impl SwitchGuard {
    /// The cut's Container half: drain and stop every holder of the Volume, then run
    /// `zfs` under the exclusive turn once no holder runs. The holders are listed again
    /// under the turn, so one started during the drain is `Busy` too, as is one the
    /// stop left running. A `Busy` cut leaves the root a writer.
    ///
    /// `gen` is checked against the floor before each holder's drain, after it (see
    /// `stop_behind_ingress`), and again under the floor lock, held from that last
    /// check through `zfs`, so a floor raised during the drain makes the cut `Stale` and
    /// a floor read never sees the flip half done. A `Stale` cut leaves the root a
    /// writer and stops no holder after the raise, since the raiser may have kept that
    /// holder or started it.
    ///
    /// With `only`, another running holder, a hook's or one Ployz didn't make included,
    /// refuses the cut `Held` before it stops anything, so a Move never interrupts a
    /// Container it didn't count, and only `only` is stopped: a stopped holder beside it
    /// stays as it is. When `only` also must run, a holder no longer running refuses
    /// the cut `Stopped`, so a Move doesn't start elsewhere a Service someone stopped.
    /// That cut records its stop before it makes it, so its resend at this `gen` goes
    /// on, and a holder beside it then is `Busy`, not `Held`: the cut stopped something.
    ///
    /// Detached from the RPC with the installation held, so a caller that hangs up
    /// does not abandon a half-stopped Container. Only a request inside the switch cuts,
    /// with its caller checked when it took the switch and again under each turn.
    pub(crate) async fn cut_volume_copy<Z, Fut>(
        &self,
        name: DockerVolumeName,
        r#gen: VolumeGen,
        drain: Drain,
        only: Option<(ContainerId, bool)>,
        zfs: Z,
    ) -> Result<Result<(), CutRefusal>, Error>
    where
        Z: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), Error>> + Send + 'static,
    {
        self.finish_installed(move |local| async move {
            let containers = local.docker()?;
            let stop = StopOptions::default();
            let mut holders = containers.volumes().volume_holder_ids(&name).await?;
            if let Some((only, must_run)) = &only {
                // Only a cut whose holder must run keeps a record of its stop: any other
                // cut naming a holder stops it again on a resend, as before.
                let mut floor = if *must_run {
                    Some(containers.lifecycle().floor_turn().await)
                } else {
                    None
                };
                let resent = match floor.as_mut() {
                    Some(floor) => floor.cut_stopped(&name, r#gen, only).await?,
                    None => false,
                };
                if let Some(other) = containers
                    .volumes()
                    .running_beside(&name, Some(only))
                    .await?
                {
                    return Ok(Err(if resent {
                        CutRefusal::Busy
                    } else {
                        CutRefusal::Held(other)
                    }));
                }
                holders.retain(|holder| holder == only);
                if let Some(floor) = floor.as_mut()
                    && !resent
                {
                    let machine_id = local.record().id();
                    let mut runs = false;
                    for holder in &holders {
                        let observed = containers
                            .observation()
                            .inspect_managed(holder, &machine_id)
                            .await?;
                        runs |= is_running(&observed.runtime);
                    }
                    if !runs {
                        return Ok(Err(CutRefusal::Stopped));
                    }
                    floor.note_cut_stop(&name, r#gen, only).await?;
                }
            }
            for holder in holders {
                if let Err(floor) = containers
                    .lifecycle()
                    .floor_turn()
                    .await
                    .admit(&name, r#gen)
                    .await?
                {
                    return Ok(Err(CutRefusal::Stale(floor)));
                }
                let observed = match local
                    .stop_behind_ingress(&holder, Some((&name, r#gen)), drain, &stop)
                    .await?
                {
                    Ok(observed) => observed,
                    Err(floor) => return Ok(Err(CutRefusal::Stale(floor))),
                };
                if is_running(&observed.runtime) {
                    return Ok(Err(CutRefusal::Busy));
                }
            }
            let _turn = local.exclusive_turn().await?;
            // Every holder, whatever made it: the cut needs the Volume idle.
            if containers
                .volumes()
                .running_beside(&name, None)
                .await?
                .is_some()
            {
                return Ok(Err(CutRefusal::Busy));
            }
            let mut floor = containers.lifecycle().floor_turn().await;
            if let Err(floor) = floor.admit(&name, r#gen).await? {
                return Ok(Err(CutRefusal::Stale(floor)));
            }
            zfs().await.map(Ok)
        })
        .await
    }
}

impl LocalMachine {
    /// The Machine this one accepts whose management address is `address`. Volume sends
    /// and pulls go only to and from one, so a Mirror holds only data another Machine of
    /// this Cluster sent.
    ///
    /// # Errors
    /// Returns a store error when the Machines cannot be read.
    pub(crate) async fn machine_at(
        &self,
        address: &ployz_core::ManagementAddress,
    ) -> Result<Option<ployz_core::MachineId>, Error> {
        let machines = self.replicated()?.machines().await?;
        Ok(machines
            .observations
            .into_iter()
            .find(|machine| machine.management_address() == *address)
            .map(|machine| machine.id))
    }

    /// Refuse a pull whose source is not [`Self::machine_at`] a Machine this one accepts.
    ///
    /// # Errors
    /// Returns [`Error::DeployRefused`] for any other address, and a store error when the
    /// Machines cannot be read.
    pub(crate) async fn admit_pull_source(
        &self,
        from: &ployz_core::ManagementAddress,
    ) -> Result<(), Error> {
        if self.machine_at(from).await?.is_some() {
            return Ok(());
        }
        tracing::warn!(
            "Volume pull refused: {} is no Machine of this Cluster",
            from.0
        );
        Err(Error::DeployRefused(
            ployz_core::authority::AuthorityRefusal::Mismatch {
                field: MismatchField::SourceServer,
            },
        ))
    }

    /// Refuse to create a Service Container on a Provisioned Volume root it doesn't share
    /// while another Container here runs on it, as [`crate::docker::holder::refuse_beside`]
    /// decides.
    /// Callers hold the switch lock, so no other start through Ployz lands between this
    /// read and Docker's create.
    ///
    /// # Errors
    /// Returns [`crate::docker::Error::VolumeHeld`] naming the running holder, or the
    /// error of a read that failed.
    async fn refuse_second_holder(
        &self,
        kind: ContainerKind,
        spec: &ResolvedServiceSpec,
    ) -> Result<(), Error> {
        if kind != ContainerKind::ServiceContainer {
            return Ok(());
        }
        let containers = self.docker()?;
        for name in crate::docker::holder::exclusive(spec) {
            containers.volumes().refuse_beside(&name).await?;
        }
        Ok(())
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
        self.finish_mutation(move |local| async move {
            local
                .docker()?
                .volumes()
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
        self.finish_mutation(move |local| async move {
            Ok(local
                .docker()?
                .images()
                .prune_images(&request.repositories)
                .await?)
        })
        .await
    }

    /// Remove the Free Builds' images that went idle, as each Free Build ends, so a
    /// Server only Cloud deploys to never keeps them all.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or Docker cannot list
    /// images or Containers.
    pub(crate) async fn prune_free_images(&self) -> Result<ImagesRemoved, Error> {
        self.finish_mutation(move |local| async move {
            Ok(local.docker()?.images().prune_free_images().await?)
        })
        .await
    }

    /// This Machine's Kept Volumes, each measured if there is time.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable or the record can't be read.
    pub(crate) async fn list_kept_volumes(
        &self,
        _request: ListKeptVolumesRequest,
    ) -> Result<KeptVolumes, Error> {
        Ok(KeptVolumes {
            volumes: self.docker()?.lifecycle().kept_volumes().await?,
        })
    }

    /// Remove the named Kept Volumes that no Container mounts, holding Machine mutation
    /// admission.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or the record or Docker's
    /// Container list can't be read.
    pub(crate) async fn remove_kept_volumes(
        &self,
        request: RemoveKeptVolumesRequest,
    ) -> Result<KeptVolumesRemoved, Error> {
        self.finish_mutation(move |local| async move {
            Ok(KeptVolumesRemoved {
                results: local
                    .docker()?
                    .lifecycle()
                    .remove_kept_volumes(request.volumes)
                    .await?,
            })
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

    /// Pull one image from its registry under shared admission, like a peer pull, as
    /// [`Self::pull_as`] lets the caller: refused before the turn at the scope the caller
    /// holds now, and checked again under it.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, an Operator names a tag, or
    /// the pull fails.
    pub(crate) async fn pull_image(&self, request: PullImageRequest) -> Result<ImagePulled, Error> {
        let request = self.pull_as(self.scope_now()?, request)?;
        self.finish_shared(move |local| async move {
            let request = local.pull_as(local.scope(), request)?;
            let docker = local.docker()?;
            let credential = match &request.registry_login {
                Some(login) => Credential::Sealed {
                    login,
                    sealing: docker.sealing(),
                },
                None => Credential::Auth(request.registry_auth.as_ref()),
            };
            let exposed_ports = docker
                .images()
                .pull_image(&request.image, request.policy, credential)
                .await?;
            Ok(ImagePulled { exposed_ports })
        })
        .await
    }

    /// `request` as a caller at `scope` may pull it. An Operator names the content: a tag
    /// is written only for a Full caller, and an image ID is only looked up.
    fn pull_as(
        &self,
        scope: Scope,
        mut request: PullImageRequest,
    ) -> Result<PullImageRequest, Error> {
        if let Scope::Operator = scope {
            if !ployz_core::authority::deploy::content_addressed(&request.image) {
                return Err(self.tag_refusal(ployz_core::PULL_IMAGE_CAPABILITY));
            }
            if request.image.starts_with("sha256:") {
                // An image ID names no registry to pull from; it is only looked up.
                request.policy = ployz_core::PullPolicy::Never;
            }
        }
        Ok(request)
    }

    /// Pull one image as the creation it serves does, under shared admission like
    /// [`Self::pull_image`]: a Full caller's creation runs the tag by name, so the tag is
    /// what this places, and an Operator's runs what the tag names, which this pulls
    /// without writing it.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, or the pull is refused or
    /// fails.
    pub(crate) async fn pull_free_image(
        &self,
        request: PullFreeImageRequest,
    ) -> Result<ImagePulled, Error> {
        // What a free pull places runs with no signature, so a login never makes it.
        let operator = matches!(self.scope_now()?, Scope::Operator);
        if request.registry_login.is_some() || (operator && request.registry_auth.is_some()) {
            return Err(Error::PullRefused(
                ployz_core::RpcError::invalid_argument(
                    "A free pull carries no registry login: sign the Service to pull its private image.",
                )
                .with_details(serde_json::json!({
                    "target": self.record().id().to_string(),
                    "capability": ployz_core::PULL_FREE_IMAGE_CAPABILITY,
                })),
            ));
        }
        self.finish_shared(move |local| async move {
            let images = local.docker()?.images();
            let exposed_ports = match local.scope() {
                Scope::Full(_) => {
                    images
                        .pull_image(
                            &request.image,
                            request.policy,
                            Credential::Auth(request.registry_auth.as_ref()),
                        )
                        .await?
                }
                Scope::Operator => {
                    images
                        .pull_free_image(&request.image, request.policy)
                        .await?
                }
            };
            Ok(ImagePulled { exposed_ports })
        })
        .await
    }

    /// Pull one image from peer ingest under shared admission: transfers run side by
    /// side, and exclusive work, such as Reset or image removal, waits for them. An
    /// Operator's names its content, so it writes no name, staging or final, that a
    /// creation could run: refused before the turn at the scope the caller holds now, and
    /// checked again under it.
    ///
    /// Unlike other admitted work, a transfer stops once its caller stops waiting, unless
    /// it has begun its tag, which it finishes. Staging names are swept off its turn: a
    /// cancelled Deployment's pull, or a stalled sweep, must not hold up the creates of
    /// the next one.
    ///
    /// # Errors
    ///
    /// Returns when Docker is unavailable, admission is busy, an Operator names a tag, or
    /// the image transfer fails.
    pub(crate) async fn pull_image_from_machine(
        &self,
        request: PullImageFromMachineRequest,
    ) -> Result<ImagePulled, Error> {
        self.peer_pull_as(self.scope_now()?, &request)?;
        self.finish_shared_with_caller(move |local, gone| async move {
            local.peer_pull_as(local.scope(), &request)?;
            let containers = local.docker()?;
            if let Err(error) = crate::docker::pull_from_ingest(
                &containers.local_docker(),
                &request.pull,
                request.source,
                &request.platform,
                &gone,
            )
            .await
            {
                return Err(containers.images().pull_failure(error).await.into());
            }
            // Every caller discards this reply; a deploy reads ports from its `PullImage`.
            Ok(ImagePulled::default())
        })
        .await
    }

    /// Whether a caller at `scope` may pull `request`: an Operator only by its content.
    fn peer_pull_as(
        &self,
        scope: Scope,
        request: &PullImageFromMachineRequest,
    ) -> Result<(), Error> {
        match scope {
            Scope::Operator if !names_content(&request.pull) => {
                Err(self.tag_refusal(ployz_core::PULL_IMAGE_FROM_MACHINE_CAPABILITY))
            }
            Scope::Operator | Scope::Full(_) => Ok(()),
        }
    }

    /// Why this Server writes no tag for an Operator, naming the route's `capability`.
    fn tag_refusal(&self, capability: &str) -> Error {
        Error::PullRefused(
            ployz_core::RpcError::unsupported(
                "This Server writes an image tag only for a caller with Full access.",
            )
            .with_details(serde_json::json!({
                "target": self.record().id().to_string(),
                "capability": capability,
            })),
        )
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
        self.finish_shared(move |local| async move {
            Ok(local.docker()?.update_limits(&container_id, &spec).await?)
        })
        .await
    }

    /// Record `spec` as Container `container_id`'s resolved spec under shared
    /// admission, when it differs in placement only and this Machine satisfies it.
    ///
    /// # Errors
    ///
    /// Returns when this Machine is not in a Cluster, Docker is unavailable,
    /// admission is busy, or the Machine refuses or fails the update.
    pub(crate) async fn update_container_placement(
        &self,
        container_id: ContainerId,
        spec: ResolvedServiceSpec,
    ) -> Result<ContainerPlacementUpdated, Error> {
        self.finish_shared(move |local| async move {
            let record = local.record();
            let machine = record.cluster_machine().ok_or(Error::NotParticipating)?;
            Ok(local
                .docker()?
                .update_placement(machine, &container_id, &spec, local.observe_storage())
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
        admitted: Option<AdmittedSpec>,
    ) -> Result<EnsuredContainer, Error> {
        let runs = matches!(want, ContainerWant::Running { .. });
        #[cfg(test)]
        self.pause_for_test(EnsurePausePoint::BeforeWork).await;
        let ensured = match want {
            // An older CLI deploys the Caddy spec itself. The doors route places it.
            ContainerWant::Running {
                container: RunTarget::New(new),
                ..
            } if QualifiedService::new(
                new.slot.namespace.clone(),
                new.resolved_spec.name.clone(),
            )
            .is_ingress_door() =>
            {
                self.translate_legacy_door(new).await
            }
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
            } => self.route_in(container, run, expect, admitted).await,
            want @ (ContainerWant::Running {
                rotation: RunRotation::Pending,
                ..
            }
            | ContainerWant::Stopped { .. }
            | ContainerWant::Gone { .. }) => self.ensure_docker(want, admitted).await,
        };
        if runs
            && self
                .containers
                .as_ref()
                .is_some_and(|containers| containers.lifecycle().departing())
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
                ..
            }) if health != HealthVerdict::Healthy => {
                if let HealthVerdict::Failed { departing, .. }
                | HealthVerdict::TimedOut { departing, .. } = &mut health
                {
                    *departing = Some(departure);
                }
                Ok(EnsuredContainer::Ensured {
                    now,
                    health: Some(health),
                    verified: None,
                })
            }
            other => other,
        }
    }

    /// A legacy Deploy of the Ingress Proxy, answered by placing the daemon's own doors.
    /// The CLI's Caddy spec never reaches Docker: only the daemon's Caddy does, in the
    /// holder's network, under the slot the CLI asked for.
    async fn translate_legacy_door(&self, new: NewContainer) -> Result<EnsuredContainer, Error> {
        let capability = Some(ployz_core::SET_INGRESS_DOORS_CAPABILITY.to_owned());
        let locator = ContainerLocator::Key(new.slot.clone());
        let legacy_caddy = new.slot.namespace == Namespace::system()
            && ployz_core::validate_ingress_service_spec(&new.resolved_spec).ok()
                == Some(ployz_core::IngressWiring::LegacyProxy);
        if !legacy_caddy {
            return Ok(EnsuredContainer::Refused {
                now: self.container_now(&locator).await?,
                refusal: ContainerRefusal {
                    reason: ContainerRefusalReason::InvalidIngress,
                    message: "upgrade the ployz CLI to change this Server's Ingress Proxy".into(),
                    capability,
                },
            });
        }
        let record = self.record();
        let Some(machine) = record.cluster_machine() else {
            return Ok(EnsuredContainer::Refused {
                now: self.container_now(&locator).await?,
                refusal: refuse(
                    ContainerRefusalReason::NotParticipating,
                    "Machine is not participating",
                ),
            });
        };
        if !machine.accepts_ingress {
            return Ok(EnsuredContainer::Refused {
                now: self.container_now(&locator).await?,
                refusal: refuse(
                    ContainerRefusalReason::NotEligible,
                    "this Server does not accept ingress",
                ),
            });
        }
        let set = self
            .converge_doors(
                &DoorSpecs::public(new.resolved_spec.service_id),
                super::ingress_doors::Legacy::Caddy,
                None,
            )
            .await?;
        match (set.proxy, set.failure) {
            (Some(proxy), None) => Ok(EnsuredContainer::Ensured {
                now: ContainerNow::Present(proxy.now),
                health: None,
                verified: None,
            }),
            (_, failure) => {
                let message = failure.map_or_else(
                    || "the Ingress Proxy was not placed".to_owned(),
                    |failure| {
                        let mut message = format!(
                            "{:?} stage failed: {}",
                            failure.stage, failure.error.message
                        );
                        for cause in &failure.error.cause {
                            message.push_str(": ");
                            message.push_str(cause);
                        }
                        message
                    },
                );
                Ok(EnsuredContainer::Refused {
                    now: self.container_now(&locator).await?,
                    refusal: ContainerRefusal {
                        reason: ContainerRefusalReason::InvalidIngress,
                        message: format!(
                            "{message}; upgrade the ployz CLI to change this Server's Ingress Proxy"
                        ),
                        capability,
                    },
                })
            }
        }
    }

    /// Whether `want` names a reserved identity, an Ingress door or the Private Access
    /// router, which its route may be replacing at the same time.
    async fn wants_reserved(&self, want: &ContainerWant) -> Result<bool, Error> {
        let identity = match want_locator(want) {
            ContainerLocator::Key(slot) => QualifiedService::new(slot.namespace, slot.service),
            ContainerLocator::Id(id) => {
                let containers = self.docker()?;
                match containers
                    .observation()
                    .inspect_managed(&id, &self.record().id())
                    .await
                {
                    Ok(observed) => observed.identity(),
                    Err(_) => return Ok(false),
                }
            }
        };
        Ok(identity.reserved().is_some())
    }

    /// Moves no rotation. An In want only re-checks itself before a start.
    async fn ensure_docker(
        &self,
        want: ContainerWant,
        admitted: Option<AdmittedSpec>,
    ) -> Result<EnsuredContainer, Error> {
        if !self.wants_reserved(&want).await? {
            // Boxed so the reserved check keeps every ensure's future the size it was.
            return Box::pin(self.ensure_unlocked(want, admitted)).await;
        }
        // A stop or remove of a door or the router waits for its route under way, so the
        // route never finds the Container it just started gone under it. The work is detached
        // with the lock, so a caller that stops waiting cannot free the lock while a
        // stop it began still runs.
        let containers = self.docker()?;
        let doors = containers.doors().hold().await;
        let local = self.clone();
        tokio::spawn(async move {
            let _doors = doors;
            local.ensure_unlocked(want, admitted).await
        })
        .await?
    }

    async fn ensure_unlocked(
        &self,
        want: ContainerWant,
        admitted: Option<AdmittedSpec>,
    ) -> Result<EnsuredContainer, Error> {
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
        let ensured = self.converge_container(want, admitted).await?;
        let Some((spec, deploy)) = settled_spec else {
            return Ok(ensured);
        };
        let EnsuredContainer::Ensured {
            now: ContainerNow::Present(observed),
            health: None,
            ..
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
            verified: None,
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
        let containers = self.docker()?;
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
                containers
                    .observation()
                    .inspect_managed_run(container_id, &machine_id),
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
                        last_exit_code: inspected.details.last_exit_code,
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
        let containers = self.docker()?;
        let details = containers
            .observation()
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

    async fn converge_container(
        &self,
        want: ContainerWant,
        admitted: Option<AdmittedSpec>,
    ) -> Result<EnsuredContainer, Error> {
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
                local.converge_now(want, admitted).await
            })
            .await?;
        }
        self.converge_now(want, admitted).await
    }

    /// The one image `want` may run. A pin and a receipt that name different images are
    /// refused. An unsigned creation's receipt must hold under this Machine's key; a signed
    /// creation ignores the receipt.
    fn runs(&self, want: &ContainerWant, admitted: Option<&AdmittedSpec>) -> Result<Runs, Error> {
        let ContainerWant::Running {
            container: RunTarget::New(new),
            ..
        } = want
        else {
            return Ok(Runs::Spec);
        };
        if let (Some(pinned), Some(receipt)) = (&new.image_id, &new.free_receipt)
            && *pinned != receipt.image
        {
            return Err(crate::docker::Error::FreeImage(
                crate::docker_image::FreeImageRefusal::Conflicts {
                    pinned: pinned.clone(),
                    receipt: receipt.image.clone(),
                },
            )
            .into());
        }
        let unsigned = admitted.is_some_and(|admitted| !admitted.signed());
        // A registry rule's pull, admitted already, governs over the caller's pin, which
        // admission refused when it named other content.
        let pinned = admitted
            .and_then(AdmittedSpec::pin)
            .or(new.image_id.as_ref());
        match (&new.free_receipt, pinned) {
            (Some(receipt), _) if unsigned => Ok(Runs::Free(
                crate::docker_image::FreeImage::check(receipt, self.docker()?.sealing())?,
            )),
            (_, Some(pinned)) => Ok(Runs::Pinned(pinned.clone())),
            (_, None) => Ok(Runs::Spec),
        }
    }

    async fn converge_now(
        &self,
        want: ContainerWant,
        admitted: Option<AdmittedSpec>,
    ) -> Result<EnsuredContainer, Error> {
        let locator = want_locator(&want);
        let fence = start_fence(&want);
        let replan = (!self.in_switch.held()).then(|| want.clone());
        let mut now = self.container_now(&locator).await?;
        // The one image this request may run, settled before any Container serves it:
        // every branch below reads it, never the pin or the receipt.
        let runs = match self.runs(&want, admitted.as_ref()) {
            Ok(runs) => runs,
            Err(error) => {
                return Ok(EnsuredContainer::Refused {
                    now: redacted(now),
                    refusal: create_refusal(error)?,
                });
            }
        };
        let admission = admitted
            .as_ref()
            .map_or(ployz_core::Admission::Free, AdmittedSpec::admission);
        let mut step = plan_container(want, &now, &self.record().phase(), admission);
        // Planned again under the switch lock, which the start keeps until it ends, so a
        // start never lands inside a switch of that Volume.
        let switch = match replan {
            Some(want) if starts_provisioned_holder(&step, &now) => {
                let guard = self.lock_switch().await?;
                now = self.container_now(&locator).await?;
                step = plan_container(want, &now, &self.record().phase(), admission);
                Some(guard)
            }
            Some(_) | None => None,
        };
        // The Container this request created, adopted or started, which the reply reads back.
        let mut acted = None;
        let refusal = match step {
            ContainerStep::Done => {
                let differs = match (runs.id(), now.present()) {
                    (Some(pin), Some(observed)) => {
                        self.image_differs(&observed.container_id, pin).await?
                    }
                    _ => None,
                };
                return Ok(match differs {
                    None => EnsuredContainer::Ensured {
                        now: redacted(now),
                        health: None,
                        verified: None,
                    },
                    Some(refusal) => EnsuredContainer::Refused {
                        now: redacted(now),
                        refusal,
                    },
                });
            }
            ContainerStep::Refuse(refusal) => {
                return Ok(EnsuredContainer::Refused {
                    now: redacted(now),
                    refusal,
                });
            }
            ContainerStep::CreateAndStart(new) => match self
                .create_and_start(new, runs, fence, admitted.ok_or_else(unadmitted)?, switch)
                .await?
            {
                Made::Refused(refusal) => Some(refusal),
                Made::Acted(id, refusal) => {
                    acted = Some(id);
                    refusal
                }
            },
            ContainerStep::Start(id) => {
                acted = Some(id);
                match runs.id() {
                    Some(pin) if let Some(refusal) = self.image_differs(&id, pin).await? => {
                        Some(refusal)
                    }
                    Some(_) | None => match fence {
                        Some((run, expect)) => self.start_routed(id, run, expect, switch).await?,
                        None => {
                            self.start_container(id, switch).await?;
                            None
                        }
                    },
                }
            }
            ContainerStep::Stop { id, drain, stop } => {
                self.finish_installed(move |local| async move {
                    Ok(local
                        .stop_out_of_rotation(&id, RotationTarget::Stopped, drain, &stop)
                        .await?
                        .err())
                })
                .await?
            }
            ContainerStep::Hold(id) => {
                self.finish_mutation(move |local| async move {
                    let containers = local.docker()?;
                    let turn =
                        match leave_rotation(containers, &id, &RotationTarget::Stopped).await? {
                            Ok(turn) => turn,
                            Err(refusal) => return Ok(Some(refusal)),
                        };
                    containers.lifecycle().stop(&id, None, None).await?;
                    drop(turn);
                    Ok(None)
                })
                .await?
            }
            ContainerStep::StopThenRemove {
                id,
                drain,
                anonymous_volumes,
                logs_to_read,
            } => {
                self.finish_installed(move |local| async move {
                    match local
                        .stop_out_of_rotation(
                            &id,
                            RotationTarget::Gone,
                            drain,
                            &StopOptions::default(),
                        )
                        .await
                    {
                        Ok(Ok(_)) => local.await_log_read(&id, logs_to_read).await,
                        Err(Error::Docker(crate::docker::Error::ContainerNotFound(_))) => {}
                        Ok(Err(refusal)) => return Ok(Some(refusal)),
                        Err(error) => return Err(error),
                    }
                    let turn = local.exclusive_turn().await?;
                    let containers = local.docker()?;
                    removed(
                        remove_out_of_rotation(&turn, containers, &id, anonymous_volumes.into())
                            .await,
                    )
                })
                .await?
            }
            ContainerStep::Remove {
                id,
                anonymous_volumes,
                logs_to_read,
            } => {
                self.finish_installed(move |local| async move {
                    local.await_log_read(&id, logs_to_read).await;
                    let turn = local.exclusive_turn().await?;
                    let containers = local.docker()?;
                    removed(
                        remove_out_of_rotation(&turn, containers, &id, anonymous_volumes.into())
                            .await,
                    )
                })
                .await?
            }
        };
        // Read back the Container acted on, never whatever holds the slot by now: another
        // request may have replaced it there since.
        let read = acted.map_or(locator, ContainerLocator::Id);
        let now = redacted(self.container_now(&read).await?);
        Ok(match refusal {
            None => EnsuredContainer::Ensured {
                now,
                health: None,
                verified: None,
            },
            Some(refusal) => EnsuredContainer::Refused { now, refusal },
        })
    }

    fn ships_logs(&self) -> bool {
        self.record().push_target().is_some()
    }

    /// Sleeps until [`LOG_READ_GRACE`] after the container exited, read once the stop has
    /// returned. No Collector, or a container Docker no longer has, waits for nothing; an
    /// exit time Docker cannot give waits the full grace.
    async fn await_log_read(&self, container: &ContainerId, logs_to_read: bool) {
        if !logs_to_read || !self.ships_logs() {
            return;
        }
        let exited_at = match self.docker() {
            Ok(containers) => match containers.observation().exited_at(container).await {
                Err(crate::docker::Error::ContainerNotFound(_)) => return,
                Ok(exited_at) => exited_at,
                Err(_) => None,
            },
            Err(_) => None,
        };
        tokio::time::sleep(log_read_remaining(exited_at, chrono::Utc::now())).await;
    }

    /// Why `id` may not serve a request pinned to `pin`: it runs another image.
    async fn image_differs(
        &self,
        id: &ContainerId,
        pin: &ployz_core::ImageDigest,
    ) -> Result<Option<ContainerRefusal>, Error> {
        let runs = self.docker()?.lifecycle().image_of(id).await?;
        Ok((runs.as_deref() != Some(pin.as_str())).then(|| {
            refuse(
                ContainerRefusalReason::ImageDiffers,
                format!(
                    "Container {id} runs image {}, not the pinned {pin}",
                    runs.as_deref().unwrap_or("unknown")
                ),
            )
        }))
    }

    pub(super) async fn container_now(
        &self,
        locator: &ContainerLocator,
    ) -> Result<ContainerNow, Error> {
        let containers = self.docker()?;
        let machine_id = self.record().id();
        let observed = match locator {
            ContainerLocator::Id(id) => match containers
                .observation()
                .inspect_managed(id, &machine_id)
                .await
            {
                Ok(observed) => Some(observed),
                Err(crate::docker::Error::ContainerNotFound(_)) => None,
                Err(error) => return Err(error.into()),
            },
            ContainerLocator::Key(slot) => {
                containers
                    .lifecycle()
                    .inspect_slot(&machine_id, slot)
                    .await?
            }
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
        runs: Runs,
        fence: Option<StartFence>,
        admitted: AdmittedSpec,
        switch: Option<SwitchGuard>,
    ) -> Result<Made, Error> {
        let outcome = self
            .finish_mutation(move |local| async move {
                let _switch = switch;
                let sealing = local.docker()?.sealing().clone();
                let created = local
                    .create_container_admitted(CreationFacts {
                        kind: new.slot.kind,
                        namespace: &new.slot.namespace,
                        spec: &new.resolved_spec,
                        creation_key: new.slot.creation_key.as_str().to_owned(),
                        deployment_id: new.deployment_id,
                        r#gen: new.r#gen,
                        image: match (&runs, new.image_prepared) {
                            (Runs::Free(free), _) => ImagePull::Free(free),
                            (Runs::Pinned(pin), _) => ImagePull::Pinned(pin),
                            (Runs::Spec, true) => ImagePull::Prepared,
                            // A sealed login opens only if this pull reaches a registry.
                            (Runs::Spec, false) => ImagePull::Policy(match &new.registry_login {
                                Some(login) => Credential::Sealed {
                                    login,
                                    sealing: &sealing,
                                },
                                None => Credential::Auth(new.registry_auth.as_ref()),
                            }),
                        },
                        admitted,
                        replaces: new.replaces,
                    })
                    .await;
                let created = match created {
                    Ok(created) => created,
                    // Nothing was made, so the reply reads the slot.
                    Err(error) => return create_refusal(error).map(Made::Refused),
                };
                let acted = |refusal| Made::Acted(created.container_id, refusal);
                // A create may adopt a Container another request made from the same spec,
                // which may run another image: it never starts for a pinned request.
                if let Some(pin) = runs.id()
                    && let Some(refusal) = local.image_differs(&created.container_id, pin).await?
                {
                    return Ok(acted(Some(refusal)));
                }
                let containers = local.docker()?;
                // A racing create adopted a hook another request already started: never run it twice.
                if new.slot.kind == ContainerKind::PreDeployHook {
                    let adopted = containers
                        .observation()
                        .inspect_managed(&created.container_id, &local.record().id())
                        .await?;
                    if !never_started(&adopted) {
                        return Ok(acted(None));
                    }
                    local
                        .approvals
                        .mark_hook_made(new.slot.creation_key.as_str())?;
                }
                if let Some((run, expect)) = fence {
                    let refusal = rotation::start_fenced(
                        &local,
                        containers,
                        &created.container_id,
                        run,
                        expect.as_ref(),
                    )
                    .await?;
                    return Ok(acted(refusal));
                }
                // A create may adopt a Container another request made, so its start is
                // checked as any other.
                local
                    .start_checked(containers, &created.container_id)
                    .await?;
                Ok(acted(None))
            })
            .await?;
        Ok(outcome)
    }

    pub(super) async fn create_container_admitted(
        &self,
        facts: CreationFacts<'_>,
    ) -> Result<ContainerCreated, Error> {
        let CreationFacts {
            kind,
            namespace,
            spec,
            creation_key,
            deployment_id,
            r#gen,
            image,
            admitted,
            replaces,
        } = facts;
        let containers = self.docker()?;
        let record = self.record();
        let machine = record.cluster_machine().ok_or(Error::NotParticipating)?;
        // Before its Volumes are ensured, so a refused preview leaves nothing here.
        // As the address part counts it: this Server's own socket vouches as a key does.
        ployz_core::admit_preview(
            &machine.id,
            namespace,
            admitted.by().admission(),
            &spec.to_requested(),
        )
        .map_err(Error::PreviewRefused)?;
        if ployz_core::PreviewNamespace::of(namespace).is_some() {
            self.approvals
                .open_preview(namespace)?
                .map_err(Error::PreviewRefused)?;
        }
        // Only a key or a rule keeps a name: Cloud never learns of a deploy through this
        // Server's own socket, so a name it kept would refuse every later Cloud deploy of a
        // Service that reaches no data. System Containers are skipped: the replica may be
        // one of them.
        let keeps_name = admitted.admission() == ployz_core::Admission::Signed
            && kind == ContainerKind::ServiceContainer
            && *namespace != Namespace::system();
        let by = admitted.by().clone();
        let hook = admitted.hook().cloned();
        containers
            .lifecycle()
            .create_with_admission(
                machine,
                ContainerRequest {
                    creation_key: &creation_key,
                    deployment_id: deployment_id.as_ref(),
                    r#gen,
                    replaces: replaces.as_ref(),
                    image,
                    kind,
                    namespace,
                    spec,
                    admitted,
                    admission: async {
                        let identity = QualifiedService::new(namespace.clone(), spec.name.clone());
                        match identity.reserved() {
                            Some(ReservedKind::Router) => {
                                let network = Box::pin(self.cluster_network()).await?;
                                admit_router(namespace, spec, network)?;
                            }
                            Some(_) => admit_door(namespace, spec)?,
                            None => admit_unreserved(spec)?,
                        }
                        // A deploy's hook runs once: a slot whose hook was made and
                        // is now gone is never made again, so it never reruns.
                        if kind == ContainerKind::PreDeployHook
                            && self.approvals.hook_made(&creation_key)?
                        {
                            return Err(Error::DeployRefused(
                                ployz_core::authority::AuthorityRefusal::Mismatch {
                                    field: MismatchField::HookRun,
                                },
                            ));
                        }
                        // Boxed: its Volume inspections would otherwise grow every create's
                        // future.
                        Box::pin(
                            containers
                                .volumes()
                                .refuse_managed_external(&machine.id, spec),
                        )
                        .await?;
                        Box::pin(self.refuse_second_holder(kind, spec)).await?;
                        // Before Docker makes it, so no signed copy exists whose name its
                        // own replica doesn't keep, even one removed before a scan sees it.
                        // A create that fails later leaves the name kept, and a failed write
                        // refuses the create. A Machine with no replica attached has no
                        // Internal DNS to keep the name for.
                        if keeps_name && let Ok(replicated) = self.replicated() {
                            replicated.keep_signed_service(&identity).await?;
                        }
                        // Read again in this turn: a signed create may have kept the name
                        // since the request's admission read it.
                        if self.name_refuses(&by, kind, &identity).await? {
                            return Err(Error::DeployRefused(
                                ployz_core::authority::AuthorityRefusal::SignedName {
                                    service: identity,
                                },
                            ));
                        }
                        // Taken last, under the turn that checked the caller, so neither an
                        // admission that never got here nor a create refused above keeps it.
                        if let Some(hook) = &hook {
                            hook.take(&self.approvals)?;
                        }
                        Ok(())
                    },
                    storage: self.observe_storage(),
                    plugin: &self.plugin,
                },
            )
            .await
            .map_err(|error| {
                let Error::Docker(crate::docker::Error::Unadmitted(refusal)) = error else {
                    return error;
                };
                tracing::warn!(
                    service = %spec.name,
                    "create refused right before Docker made it: {refusal}"
                );
                Error::DeployRefused(refusal)
            })
    }
}

/// What a creation did with the slot.
#[derive(Debug)]
enum Made {
    /// It made nothing, so the reply reads the slot.
    Refused(ContainerRefusal),
    /// It made or adopted this Container, and refused to go on with it when it says why.
    Acted(ContainerId, Option<ContainerRefusal>),
}

/// What a stop checks around its drain and its admission turn.
trait StopGuard {
    /// Why the stop is refused.
    type Refusal;
    /// The admission turn the stop runs under.
    type Turn;
    /// What `check` holds through the stop.
    type Held;

    /// Whether a stop that ended a run records it.
    fn records(&self) -> bool;
    /// Takes the copy out of ingress, held until the stop ends.
    async fn withdraw(&self) -> Result<Result<Withdrawal, Self::Refusal>, Error>;
    /// Checked once the drain ends, before the turn.
    async fn drained(&self) -> Result<Result<(), Self::Refusal>, Error>;
    /// Taken once `drained` passes and held through the stop.
    async fn turn(&self, local: &Installed) -> Result<Self::Turn, Error>;
    /// Checked under the turn.
    async fn check(&self) -> Result<Result<Self::Held, Self::Refusal>, Error>;
}

/// A stop of a Container that holds a Volume, refused once that Volume's floor is
/// raised above `gen`.
struct Fence<'a> {
    containers: &'a ContainerRuntime,
    container: &'a ContainerId,
    fence: Option<(&'a DockerVolumeName, VolumeGen)>,
}

impl<'a> StopGuard for Fence<'a> {
    type Refusal = VolumeGen;
    type Turn = ExclusiveTurn;
    type Held = Option<crate::docker::FloorTurn<'a>>;

    fn records(&self) -> bool {
        false
    }

    async fn withdraw(&self) -> Result<Result<Withdrawal, VolumeGen>, Error> {
        Ok(Ok(self
            .containers
            .lifecycle()
            .withdraw_locked(*self.container)
            .await))
    }

    async fn drained(&self) -> Result<Result<(), VolumeGen>, Error> {
        let Some((name, r#gen)) = self.fence else {
            return Ok(Ok(()));
        };
        Ok(self
            .containers
            .lifecycle()
            .floor_turn()
            .await
            .admit(name, r#gen)
            .await?)
    }

    async fn turn(&self, local: &Installed) -> Result<ExclusiveTurn, Error> {
        local.exclusive_turn().await
    }

    async fn check(&self) -> Result<Result<Self::Held, VolumeGen>, Error> {
        let Some((name, r#gen)) = self.fence else {
            return Ok(Ok(None));
        };
        let mut floor = self.containers.lifecycle().floor_turn().await;
        Ok(floor.admit(name, r#gen).await?.map(|()| Some(floor)))
    }
}

/// A stop of a Container taken out of routing for `target`, refused while a copy is
/// In.
struct Leaving<'a> {
    containers: &'a ContainerRuntime,
    container: &'a ContainerId,
    target: &'a RotationTarget,
}

impl<'a> StopGuard for Leaving<'a> {
    type Refusal = ContainerRefusal;
    type Turn = SharedTurn;
    type Held = ();

    fn records(&self) -> bool {
        *self.target == RotationTarget::Stopped
    }

    async fn withdraw(&self) -> Result<Result<Withdrawal, ContainerRefusal>, Error> {
        Ok(leave_rotation(self.containers, self.container, self.target)
            .await?
            .map(|_turn| self.containers.lifecycle().withdraw(*self.container)))
    }

    async fn drained(&self) -> Result<Result<(), ContainerRefusal>, Error> {
        Ok(Ok(()))
    }

    async fn turn(&self, local: &Installed) -> Result<SharedTurn, Error> {
        local.shared_turn().await
    }

    async fn check(&self) -> Result<Result<(), ContainerRefusal>, Error> {
        Ok(leave_rotation(self.containers, self.container, self.target)
            .await?
            .map(|_turn| ()))
    }
}

/// What a Container is created from: a NewContainer's facts, or those of a door or a
/// Volume holder, which have no NewContainer.
pub(super) struct CreationFacts<'a> {
    pub(super) kind: ContainerKind,
    pub(super) namespace: &'a Namespace,
    pub(super) spec: &'a ResolvedServiceSpec,
    pub(super) creation_key: String,
    pub(super) deployment_id: Option<DeploymentLogId>,
    pub(super) r#gen: Gen,
    pub(super) image: ImagePull<'a>,
    pub(super) admitted: AdmittedSpec,
    /// The copy on this Machine the new Container takes the place of.
    pub(super) replaces: Option<ContainerId>,
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
        logs_to_read: bool,
    },
    Remove {
        id: ContainerId,
        anonymous_volumes: AnonymousVolumes,
        logs_to_read: bool,
    },
}

/// Whether `step` starts a Container, the one `now` observes, that mounts a Provisioned
/// Volume.
fn starts_provisioned_holder(step: &ContainerStep, now: &ContainerNow) -> bool {
    let spec = match (step, now) {
        (ContainerStep::Start(_), ContainerNow::Present(observed)) => {
            observed.resolved_spec.to_requested()
        }
        (ContainerStep::CreateAndStart(new), _) => new.resolved_spec.to_requested(),
        _ => return false,
    };
    spec.volume_graph().mounted_volumes().any(|volume| {
        matches!(
            volume.source.kind(),
            ployz_core::RawVolumeSource::Provisioned { .. }
        )
    })
}

/// What to do for `want` against `now`. A reserved identity holds only a spec this daemon
/// builds, and none mounts a Provisioned Volume, so a start of one that would is refused
/// here: its ensure holds the doors lock, which a Volume step inside the switch takes too,
/// so it must never wait for the switch.
///
/// `admission` is how this Machine admitted the request. A signed request never adopts a
/// Container it didn't admit signed: that copy answers for no name a signed one keeps, and
/// adopting it would skip the write that keeps the name.
fn plan_container(
    want: ContainerWant,
    now: &ContainerNow,
    phase: &LocalMachinePhase,
    admission: ployz_core::Admission,
) -> ContainerStep {
    let step = plan_docker(want, now, phase, admission);
    let reserved = match (&step, now) {
        (ContainerStep::CreateAndStart(new), _) => {
            QualifiedService::new(new.slot.namespace.clone(), new.slot.service.clone()).reserved()
        }
        (ContainerStep::Start(_), ContainerNow::Present(observed)) => {
            observed.identity().reserved()
        }
        _ => None,
    };
    if reserved.is_some() && starts_provisioned_holder(&step, now) {
        return ContainerStep::Refuse(refuse(
            ContainerRefusalReason::InvalidIngress,
            "a reserved Service mounts no Provisioned Volume",
        ));
    }
    step
}

fn plan_docker(
    want: ContainerWant,
    now: &ContainerNow,
    phase: &LocalMachinePhase,
    admission: ployz_core::Admission,
) -> ContainerStep {
    let present = match now {
        ContainerNow::Present(observed) => Some(observed),
        ContainerNow::Absent { .. } => None,
    };
    match want {
        ContainerWant::Running { .. } if !has_cluster_identity(phase) => {
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
            Some(observed) if admission == ployz_core::Admission::Signed && !observed.signed() => {
                ContainerStep::Refuse(refuse(
                    ContainerRefusalReason::SlotOccupied,
                    "the slot holds a Container this server didn't admit from a signed deploy",
                ))
            }
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
                ContainerLocator::Id(id) => ContainerStep::Remove {
                    id,
                    anonymous_volumes,
                    logs_to_read: false,
                },
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
                logs_to_read: has_logs_to_read(observed),
            },
            Some(observed) => ContainerStep::Remove {
                id: observed.container_id,
                anonymous_volumes,
                logs_to_read: has_logs_to_read(observed),
            },
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

async fn leave_rotation<'a>(
    containers: &'a crate::docker::ContainerRuntime,
    container: &ContainerId,
    target: &RotationTarget,
) -> Result<Result<RotationTurn<'a>, ContainerRefusal>, Error> {
    let turn = containers.lifecycle().rotation_turn(container).await?;
    Ok(if in_rotation(turn.rotation(), target) {
        Err(in_rotation_refusal(container, turn.rotation()))
    } else {
        Ok(turn)
    })
}

/// A Container Docker no longer has serves nothing, so only its records go.
async fn remove_out_of_rotation(
    _turn: &ExclusiveTurn,
    containers: &crate::docker::ContainerRuntime,
    container: &ContainerId,
    volumes: crate::docker::VolumeDisposal,
) -> Result<Option<ContainerRefusal>, Error> {
    match leave_rotation(containers, container, &RotationTarget::Gone).await {
        Ok(Ok(turn)) => turn.remove(volumes, false).await?,
        Ok(Err(refusal)) => return Ok(Some(refusal)),
        Err(Error::Docker(crate::docker::Error::ContainerNotFound(_))) => {
            containers
                .lifecycle()
                .remove(container, volumes, false)
                .await?;
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
        capability: None,
    }
}

/// The one image a request may run: what its spec names, an image ID it is pinned to,
/// or a Free Build's image whose receipt held. Every Container the request names runs it.
#[derive(Debug)]
enum Runs {
    Spec,
    Pinned(ployz_core::ImageDigest),
    Free(crate::docker_image::FreeImage),
}

impl Runs {
    fn id(&self) -> Option<&ployz_core::ImageDigest> {
        match self {
            Self::Spec => None,
            Self::Pinned(id) => Some(id),
            Self::Free(free) => Some(free.id()),
        }
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

/// A creation reached Docker with no admission of its spec. Every caller's spec passes
/// [`LocalMachine::admit_spec`] first, so this is a path that skipped it.
fn unadmitted() -> Error {
    Error::Docker(crate::docker::Error::NotAdmitted)
}

fn create_refusal(error: Error) -> Result<ContainerRefusal, Error> {
    use crate::{docker::Error as Docker, docker_image::LoginRefused, sealed_env::OpenError};
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "only create-path errors a driver can act on become refusals"
    )]
    let reason = match &error {
        Error::NotParticipating => ContainerRefusalReason::NotParticipating,
        Error::IngressProxyServiceSpec(_) => ContainerRefusalReason::InvalidIngress,
        Error::Docker(
            Docker::IngressHolderNotRunning
            | Docker::TunnelOriginNotRunning
            | Docker::UnrecognizedNetwork,
        ) => {
            return Ok(ContainerRefusal {
                reason: ContainerRefusalReason::InvalidIngress,
                message: ployz_core::error_chain::inline(&error),
                capability: Some(ployz_core::SET_INGRESS_DOORS_CAPABILITY.to_owned()),
            });
        }
        Error::Docker(
            Docker::WorkNotAccepted
            | Docker::ServicePlacementMismatch
            | Docker::ProvisionedStorageUnsupported
            | Docker::StorageUnobservable,
        ) => ContainerRefusalReason::NotEligible,
        Error::Docker(Docker::SlotNameOccupied(_)) => ContainerRefusalReason::SlotOccupied,
        Error::Docker(
            Docker::ImagePull(_)
            | Docker::ImageAbsent { .. }
            | Docker::PlacedImageGone { .. }
            | Docker::PlacedImageDisagrees { .. }
            | Docker::UnsignedImage(_)
            | Docker::FreeImage(_)
            | Docker::DiskFull { .. }
            | Docker::RegistryLogin(
                LoginRefused::Uppercase(_)
                | LoginRefused::OtherHost { .. }
                | LoginRefused::Insecure(_)
                | LoginRefused::Mirrored,
            ),
        ) => ContainerRefusalReason::ImagePullFailed,
        Error::Docker(
            Docker::ExternalVolumeNotFound(_)
            | Docker::VolumeShapeMismatch { .. }
            | Docker::VolumeNotWritable(_)
            | Docker::VolumeHeld { .. },
        ) => ContainerRefusalReason::VolumesNotPrepared,
        Error::Docker(Docker::ForeignVolume { .. }) => ContainerRefusalReason::ForeignVolume,
        Error::Docker(Docker::Sealed(open)) => match open {
            OpenError::KeyMissing => ContainerRefusalReason::SealingKeyMissing,
            OpenError::KeyUnreadable(_) => return Err(error),
            OpenError::MissingIdentity(_)
            | OpenError::SealedPort
            | OpenError::BadName(_)
            | OpenError::Unbound(_)
            | OpenError::Unopenable(_)
            | OpenError::TunnelToken
            | OpenError::PrivateAccessKey
            | OpenError::RegistryLogin(_)
            | OpenError::ShortLine(_)
            | OpenError::Unsigned(_)
            | OpenError::OutOfReach(_) => ContainerRefusalReason::SealedValueUnopenable,
        },
        _ => return Err(error),
    };
    Ok(refuse(reason, ployz_core::error_chain::inline(&error)))
}

/// Whether a peer pull names its content: an image by digest, kept under no tag or only
/// the tag Ployz keeps for that digest.
pub(crate) fn names_content(pull: &ployz_core::PeerImagePull) -> bool {
    let Some(digest) = ployz_core::ImageRef::new(pull.image()).digest else {
        return false;
    };
    pull.tag().is_none_or(|tag| {
        ployz_core::retained_digest(tag).is_some_and(|kept| kept.as_str() == digest)
    })
}

/// A Writer start's Create, admitted, with the ID of the image it placed before its
/// promote, so the Create after the promote reaches no registry.
#[derive(Clone)]
pub(crate) struct PlacedHolder {
    admitted: AdmittedSpec,
    id: String,
}

impl PlacedHolder {
    /// The record a promote to `epoch` writes, so a resend after it at that epoch, for
    /// the same admitted spec, takes the same image.
    pub(crate) fn record(&self, epoch: ployz_core::WriterEpoch) -> Option<ployz_core::PlacedImage> {
        ployz_core::PlacedImage::new(epoch, self.admitted.placement(), &self.id).ok()
    }
}

#[path = "drain_idle.rs"]
mod drain_idle;
#[path = "rotation.rs"]
mod rotation;

#[cfg(test)]
#[path = "container_tests.rs"]
mod tests;
