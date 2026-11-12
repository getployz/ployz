use std::time::Duration;

use crate::cluster::{Call, Deadline};
use crate::connect::{Client, ConnectError, TARGET_RPC_TIMEOUT, stop_rpc_timeout};
use ployz_core::{
    ContainerDetails, ContainerId, ContainerKind, ContainerLimitsUpdated, ContainerNow,
    ContainerObservation, ContainerWant, CreationKey, DeployWarning, DockerVolumeId,
    EnsuredContainer, EnvironmentValues, HealthVerdict, ImagePulled, IngressDoorsSet,
    InspectContainerRequest, ListContainersRequest, MachineFailure, MachineId, MachineName,
    MachineTarget, MembershipObservation, ObservationGap, ObservationGapReason, ObservationKind,
    PrivateAccessSet, QualifiedService, ReadRoutesRequest, RemoveVolumeRequest,
    ResolvedServiceSpec, RoutesRead, RpcError, RunTarget, SetIngressDoorsRequest,
    SetPrivateAccessRequest, SlotKey, UpdateContainerLimitsRequest, op,
};

use super::{Progress, SlotPolicy, naming_machine};

/// One Service's copies on the Machines that answered, and each Machine whose silence
/// is a [`crate::deploy::listing_gap`].
pub(crate) struct Listed {
    pub(crate) copies: Vec<ContainerObservation>,
    pub(crate) unanswered: Vec<Unanswered>,
}

/// A Machine whose listing went unanswered, a [`crate::deploy::listing_gap`].
pub(crate) struct Unanswered {
    pub(crate) name: MachineName,
    pub(crate) failure: MachineFailure<RpcError>,
    /// The failure as planning words it too, so their notices are one.
    pub(crate) message: String,
    /// It reads Up but the entry's router couldn't dial it, as while its daemon is
    /// stopped: its proxy keeps serving its last routes and nothing writes its ack.
    pub(crate) acks_unwritten: bool,
}

impl Unanswered {
    /// Report this Machine as an observation gap, which leaves the Deploy partial.
    pub(super) fn warn(&self, progress: &Progress) {
        progress.warn(DeployWarning::ObservationFailed {
            gap: Some(ObservationGap {
                machine_name: self.name.clone(),
                reason: ObservationGapReason::Failed,
            }),
            kind: ObservationKind::Container,
            machine_id: self.failure.machine_id,
            machine_name: None,
            message: self.message.clone(),
        });
    }
}

/// Why the entry Machine couldn't be dialed again.
#[derive(Clone, Debug)]
pub(crate) enum RedialFailure {
    /// A later dial may still reach the entry.
    Failed,
    /// The entry answered as another Machine, or as one in another Cluster. No later dial
    /// changes that, so a wait for the entry ends on it.
    Refused(RpcError),
}

/// What every Machine call rides on: the one entry connection.
pub(crate) trait Session {
    /// Dial the entry Machine again on the same connection, after the session a call
    /// went out on closed. None when this client can't.
    async fn redial(&self) -> Option<Result<(), RedialFailure>> {
        None
    }
}

/// Listing, reading and settling one Container, which Drain, catch-up and cleanup do
/// without any Deploy in flight.
pub(crate) trait CopyLifecycle: Session {
    async fn service_containers(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError>;
    /// [`Self::service_containers`], keeping each Machine whose silence is a
    /// [`crate::deploy::listing_gap`] beside the copies the others list instead of
    /// failing on it. An answered error, or any other silence, still fails. The default
    /// reports no Machine as unanswered.
    async fn listed_copies(&self, service: &QualifiedService) -> Result<Listed, RpcError> {
        Ok(Listed {
            copies: self.service_containers(service).await?,
            unanswered: Vec::new(),
        })
    }
    /// `service`'s copies on `machine_id`, listed by that Machine alone, so no other
    /// Machine's silence fails it. The default asks every Machine and keeps its copies.
    async fn target_copies(
        &self,
        machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError> {
        let mut copies = self.service_containers(service).await?;
        copies.retain(|copy| copy.machine_id == *machine_id);
        Ok(copies)
    }
    /// Every Container row in the entry Machine's store, which keeps the rows of
    /// a Machine that does not answer.
    async fn replicated_containers(&self) -> Result<Vec<ContainerObservation>, RpcError>;
    async fn ensure_container(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
    ) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError>;
    /// Converge one Container within `timeout`, keeping a refusal as typed.
    async fn ensure_unsettled(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        timeout: Option<Duration>,
    ) -> Result<EnsuredContainer, RpcError>;
    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerDetails, RpcError>;
    /// What each Machine's proxy routes for `service`, as `machine_id` reads it.
    async fn read_routes(
        &self,
        machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<RoutesRead, RpcError>;
    /// [`Self::read_routes`] as the entry Machine this client reaches reads it.
    async fn read_entry_routes(&self, service: &QualifiedService) -> Result<RoutesRead, RpcError>;
}

/// What a Deploy does to a Machine beside settling Containers.
pub(crate) trait DeployOperations: Session {
    async fn prepare_volumes(
        &self,
        machine_id: &MachineId,
        specs: &[ployz_core::ServiceStorageSpec],
    ) -> Result<ployz_core::PreparedVolumes, RpcError>;
    async fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError>;
    /// Apply `spec`'s CPU and memory limits to the running `container_id` in place.
    async fn update_container_limits(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerLimitsUpdated, RpcError>;
    /// Whether `machine_id` already runs a Container of this Global spec, which a
    /// start into `slot` adopts instead of creating.
    async fn runs_global(
        &self,
        machine_id: &MachineId,
        slot: &SlotKey,
        spec: &ResolvedServiceSpec,
    ) -> Result<bool, RpcError>;
    /// Make the image `spec` runs present on `machine_id`, pulled as its policy
    /// asks, before any Container changes, and answer what the Machine reports of it.
    async fn ensure_image(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ImagePulled, RpcError>;
}

/// Placing the Ingress Proxy's doors and the Private Access router on a Machine.
pub(crate) trait ProxyPlacement: Session {
    /// Bring `machine_id`'s Ingress Proxy to its daemon's pinned wiring for `request`.
    async fn set_ingress_doors(
        &self,
        machine_id: &MachineId,
        request: SetIngressDoorsRequest,
    ) -> Result<IngressDoorsSet, RpcError>;
    /// Place or remove `machine_id`'s Private Access router for `request`.
    async fn set_private_access(
        &self,
        machine_id: &MachineId,
        request: SetPrivateAccessRequest,
    ) -> Result<PrivateAccessSet, RpcError>;
}

/// Everything the executor asks of a Machine, for clients that serve all three.
pub(crate) trait MachineOperations:
    CopyLifecycle + DeployOperations + ProxyPlacement
{
}

impl<T: CopyLifecycle + DeployOperations + ProxyPlacement> MachineOperations for T {}

impl Session for Client {
    async fn redial(&self) -> Option<Result<(), RedialFailure>> {
        Some(self.redial_shared().await.map_err(|error| {
            let refused = matches!(
                error,
                ConnectError::IdentityMismatch { .. } | ConnectError::ClusterMismatch { .. }
            );
            if refused {
                RedialFailure::Refused(crate::connect::rpc_error(error))
            } else {
                RedialFailure::Failed
            }
        }))
    }
}

impl CopyLifecycle for Client {
    async fn service_containers(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError> {
        let listed = self.listed_copies(service).await?;
        match listed.unanswered.into_iter().next() {
            Some(silent) => Err(naming_machine(
                crate::cluster::unanswered_by(&silent.name, silent.failure.error),
                &silent.failure.machine_id,
            )),
            None => Ok(listed.copies),
        }
    }

    async fn listed_copies(&self, service: &QualifiedService) -> Result<Listed, RpcError> {
        let mut client = self.clone();
        let machines = client.machines().await.map_err(RpcError::from)?;
        let listed = client
            .live_services_from(&machines, EnvironmentValues::Redacted)
            .await
            .map_err(RpcError::from)?
            .containers;
        let mut unanswered = Vec::new();
        for failure in listed.failures {
            match machines
                .iter()
                .find(|machine| machine.machine.id == failure.machine_id)
            {
                Some(machine) if crate::deploy::listing_gap(machine, &failure.error) => {
                    unanswered.push(Unanswered {
                        name: machine.machine.name.clone(),
                        message: crate::deploy::unanswered_message(machine, &failure.error),
                        acks_unwritten: machine.membership == MembershipObservation::Up
                            && failure.error.undialed(),
                        failure,
                    });
                }
                Some(machine) if failure.error.target_unanswered() => {
                    return Err(naming_machine(
                        crate::cluster::unanswered_by(&machine.machine.name, failure.error),
                        &failure.machine_id,
                    ));
                }
                Some(_) | None => return Err(failure.error),
            }
        }
        if let Some(machine_id) = listed.omissions.into_iter().find(|machine_id| {
            omission_requires_observation(
                machines
                    .iter()
                    .find(|machine| machine.machine.id == *machine_id)
                    .map(|machine| &machine.membership),
            )
        }) {
            return Err(RpcError::unavailable(format!(
                "container observation omitted {machine_id}"
            )));
        }
        let copies = listed
            .successes
            .into_iter()
            .flat_map(|success| success.value)
            .filter(|container| {
                container.kind == ContainerKind::ServiceContainer
                    && container.namespace == service.namespace
                    && container.resolved_spec.name == service.name
            })
            .collect();
        Ok(Listed { copies, unanswered })
    }

    async fn target_copies(
        &self,
        machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError> {
        let listed = self
            .clone()
            .call::<op::ListContainers>(
                ListContainersRequest {
                    environment: EnvironmentValues::Redacted,
                },
                Call::read()
                    .to(&MachineTarget::from(machine_id))
                    .budget(crate::connect::target_read_timeout()),
            )
            .await
            .map_err(crate::connect::rpc_error)?
            .containers;
        Ok(listed
            .into_iter()
            .filter(|observed| {
                observed.machine_id == *machine_id
                    && observed.kind == ContainerKind::ServiceContainer
                    && observed.identity() == *service
            })
            .collect())
    }

    async fn replicated_containers(&self) -> Result<Vec<ContainerObservation>, RpcError> {
        self.runtime_frame().await.map(|frame| frame.containers)
    }

    async fn ensure_container(
        &self,
        machine_id: &MachineId,
        mut want: ContainerWant,
    ) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError> {
        let timeout = match &mut want {
            ContainerWant::Running {
                container: RunTarget::New(new),
                ..
            } => {
                if let Some(key) = self
                    .retained_global_key(machine_id, &new.slot, &new.resolved_spec)
                    .await?
                {
                    new.slot.creation_key = key;
                }
                // A replacement joining a named Deploy keeps that Deploy's label.
                if new.deployment_id.is_none() {
                    new.deployment_id.clone_from(&self.deploy.id);
                }
                None
            }
            ContainerWant::Running { .. }
            | ContainerWant::Out { .. }
            | ContainerWant::Held { .. } => Some(TARGET_RPC_TIMEOUT),
            ContainerWant::Stopped { stop, .. } => stop_rpc_timeout(stop.grace_period_seconds, 1),
            ContainerWant::Gone { .. } => None,
        };
        Client::ensure_settled(self, machine_id, want, timeout).await
    }

    async fn ensure_unsettled(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        timeout: Option<Duration>,
    ) -> Result<EnsuredContainer, RpcError> {
        Client::ensure_unsettled(self, machine_id, want, timeout).await
    }

    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerDetails, RpcError> {
        self.call_shared::<op::InspectContainer>(
            InspectContainerRequest {
                container_id: *container_id,
            },
            Call::once()
                .to(&MachineTarget::from(machine_id))
                .within(Deadline::Target(TARGET_RPC_TIMEOUT)),
        )
        .await
        .map_err(crate::connect::rpc_error)
    }

    async fn read_routes(
        &self,
        machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<RoutesRead, RpcError> {
        self.call_shared::<op::ReadRoutes>(
            ReadRoutesRequest {
                service: service.clone(),
            },
            Call::once()
                .to(&MachineTarget::from(machine_id))
                .within(Deadline::Target(TARGET_RPC_TIMEOUT)),
        )
        .await
        .map_err(crate::connect::rpc_error)
    }

    async fn read_entry_routes(&self, service: &QualifiedService) -> Result<RoutesRead, RpcError> {
        self.call_shared::<op::ReadRoutes>(
            ReadRoutesRequest {
                service: service.clone(),
            },
            Call::once().within(Deadline::Target(TARGET_RPC_TIMEOUT)),
        )
        .await
        .map_err(crate::connect::rpc_error)
    }
}

impl DeployOperations for Client {
    async fn prepare_volumes(
        &self,
        machine_id: &MachineId,
        specs: &[ployz_core::ServiceStorageSpec],
    ) -> Result<ployz_core::PreparedVolumes, RpcError> {
        self.call_shared::<op::PrepareVolumes>(
            ployz_core::PrepareVolumesRequest {
                specs: specs.to_vec(),
            },
            Call::once()
                .to(&MachineTarget::from(machine_id))
                .within(Deadline::Target(Duration::from_secs(120))),
        )
        .await
        .map_err(crate::connect::rpc_error)
    }

    async fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError> {
        self.call_shared::<op::RemoveVolume>(
            RemoveVolumeRequest {
                teardown: self.deploy.teardown(),
                name: id.name.clone(),
                force: false,
            },
            Call::once()
                .to(&MachineTarget::from(&id.machine_id))
                .within(Deadline::Target(TARGET_RPC_TIMEOUT)),
        )
        .await
        .map_err(crate::connect::rpc_error)
        .map(|_| ())
    }

    async fn update_container_limits(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerLimitsUpdated, RpcError> {
        self.call_shared::<op::UpdateContainerLimits>(
            UpdateContainerLimitsRequest {
                container_id: *container_id,
                spec: spec.clone(),
            },
            Call::once()
                .to(&MachineTarget::from(machine_id))
                .within(Deadline::Target(TARGET_RPC_TIMEOUT)),
        )
        .await
        .map_err(crate::connect::rpc_error)
    }

    async fn runs_global(
        &self,
        machine_id: &MachineId,
        slot: &SlotKey,
        spec: &ResolvedServiceSpec,
    ) -> Result<bool, RpcError> {
        Ok(self
            .retained_global_key(machine_id, slot, spec)
            .await?
            .is_some())
    }

    async fn ensure_image(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ImagePulled, RpcError> {
        crate::image::pull_cluster_image(
            self,
            machine_id,
            &spec.container.image,
            spec.container.pull_policy,
            self.deploy.registry_auth.get(&spec.name).cloned(),
        )
        .await
    }
}

impl ProxyPlacement for Client {
    async fn set_ingress_doors(
        &self,
        machine_id: &MachineId,
        request: SetIngressDoorsRequest,
    ) -> Result<IngressDoorsSet, RpcError> {
        Client::set_ingress_doors(self, machine_id, request).await
    }

    async fn set_private_access(
        &self,
        machine_id: &MachineId,
        request: SetPrivateAccessRequest,
    ) -> Result<PrivateAccessSet, RpcError> {
        Client::set_private_access(self, machine_id, request).await
    }
}

pub(super) fn omission_requires_observation(membership: Option<&MembershipObservation>) -> bool {
    membership.is_none_or(MembershipObservation::invites_rpc)
}

impl Client {
    /// The creation key of a retained Global replacement that a retry replans as a run,
    /// so the run starts that Container rather than making a second one.
    async fn retained_global_key(
        &self,
        machine_id: &MachineId,
        slot: &SlotKey,
        spec: &ResolvedServiceSpec,
    ) -> Result<Option<CreationKey>, RpcError> {
        if !SlotPolicy::of(slot.kind, &spec.mode).keyed_by_spec()
            || slot.creation_key.as_str() != crate::cluster::global_creation_key(spec)
        {
            return Ok(None);
        }
        Ok(self
            .clone()
            .call::<op::ListContainers>(
                ListContainersRequest {
                    environment: EnvironmentValues::Included,
                },
                Call::read()
                    .to(&MachineTarget::from(machine_id))
                    .budget(crate::connect::target_read_timeout()),
            )
            .await
            .map_err(|error| crate::cluster::answered_by(machine_id, error))?
            .containers
            .into_iter()
            .find_map(|container| {
                (container.machine_id == *machine_id
                    && container.kind == slot.kind
                    && container.namespace == slot.namespace
                    && container.resolved_spec == *spec)
                    .then(|| container.creation_key().map(str::to_owned))
                    .flatten()
            })
            .and_then(|key| CreationKey::try_from(key).ok()))
    }
}
