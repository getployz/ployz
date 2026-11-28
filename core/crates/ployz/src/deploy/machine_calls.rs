use std::time::Duration;

use std::sync::Arc;

use crate::cluster::{Call, Carried, Deadline};
use crate::connect::{Client, ConnectError, TARGET_RPC_TIMEOUT, stop_rpc_timeout};
use ployz_core::{
    ContainerDetails, ContainerId, ContainerKind, ContainerLimitsUpdated, ContainerNow,
    ContainerObservation, ContainerPlacementUpdated, ContainerWant, CreationKey, DeployWarning,
    DockerVolumeId, EnsuredContainer, EnvironmentValues, HealthVerdict, ImagePulled,
    IngressDoorsSet, InspectContainerRequest, ListContainersRequest, MachineFailure, MachineId,
    MachineName, MachineTarget, MembershipObservation, Namespace, ObservationGap,
    ObservationGapReason, ObservationKind, PrivateAccessSet, QualifiedService, ReadRoutesRequest,
    RemoveVolumeRequest, ResolvedServiceSpec, RoutesRead, RpcError, RunTarget, SealedLogin,
    SetIngressDoorsRequest, SetPrivateAccessRequest, SlotKey, UpdateContainerLimitsRequest,
    UpdateContainerPlacementRequest, op,
};

use super::approvals::Approvals;
use super::progress::Progress;

mod set_each;
mod transport;

pub(crate) use set_each::{Answer, set_each};
pub(crate) use transport::RestartTolerant;
pub(in crate::deploy) use transport::{Outstanding, RESTART_WAIT};
#[cfg(test)]
pub(in crate::deploy) use transport::{RESTART_POLL, wait_out_restart};

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
    /// It reads Up but its daemon didn't answer, as while it is stopped or frozen, and
    /// no path dropping large packets explains that: its proxy keeps serving its last
    /// routes and nothing writes its ack.
    pub(crate) acks_unwritten: bool,
}

impl Unanswered {
    /// Report this Machine as an observation gap, which leaves the Deploy partial.
    pub(in crate::deploy) fn warn(&self, progress: &Progress) {
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
    /// Create `want`'s new Container and keep the answer typed, refusals included. A
    /// Container pinned to an image ID goes on the pinned route.
    async fn ensure_new(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
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
    /// Record `spec`, which differs in placement only, as the spec of the running
    /// `container_id`. The Machine refuses a placement it does not satisfy.
    async fn update_container_placement(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerPlacementUpdated, RpcError>;
    /// Whether `machine_id` already runs a Container of this Global spec, which a
    /// start into `slot` adopts instead of creating.
    async fn runs_global(
        &self,
        machine_id: &MachineId,
        slot: &SlotKey,
        spec: &ResolvedServiceSpec,
    ) -> Result<bool, RpcError>;
    /// Make the image `spec` runs present on `machine_id`, pulled as its policy
    /// asks with `auth`, before any Container changes, and answer what the Machine
    /// reports of it.
    async fn ensure_image(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
        auth: Option<SealedLogin>,
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

/// Everything the executor asks of a Machine.
pub(crate) trait MachineOperations: CopyLifecycle + DeployOperations {}

impl<T: CopyLifecycle + DeployOperations> MachineOperations for T {}

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
                Some(machine) if crate::deploy::listing_gap(&failure.error) => {
                    unanswered.push(Unanswered {
                        name: machine.machine.name.clone(),
                        message: crate::deploy::unanswered_message(machine, &failure.error),
                        acks_unwritten: machine.membership == MembershipObservation::Up
                            && !crate::deploy::dropped_large_packets(machine, &failure.error),
                        failure,
                    });
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
                Call::target_read(&MachineTarget::from(machine_id)),
            )
            .await
            .map_err(crate::cluster::unanswered_kept);
        let listed = match listed {
            Ok(listed) => listed.containers,
            // The Server's name and id ride on the error, as they did when every Machine was listed.
            Err(error) if error.target_unanswered() => {
                let mut client = self.clone();
                let machines = client.machines().await.map_err(RpcError::from)?;
                return Err(
                    match machines
                        .iter()
                        .find(|machine| machine.machine.id == *machine_id)
                    {
                        Some(machine) => naming_machine(
                            crate::cluster::unanswered_by(&machine.machine.name, error),
                            machine_id,
                        ),
                        None => error,
                    },
                );
            }
            Err(error) => return Err(error),
        };
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
        want: ContainerWant,
    ) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError> {
        self.ensure_settled(machine_id, want, None).await
    }

    async fn ensure_unsettled(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        timeout: Option<Duration>,
    ) -> Result<EnsuredContainer, RpcError> {
        self.ensure_approved::<op::EnsureContainer>(machine_id, want, None, timeout)
            .await
    }

    async fn ensure_new(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
    ) -> Result<EnsuredContainer, RpcError> {
        self.ensure_created(machine_id, want, None).await
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

    async fn update_container_placement(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerPlacementUpdated, RpcError> {
        self.call_shared::<op::UpdateContainerPlacement>(
            UpdateContainerPlacementRequest {
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
        auth: Option<SealedLogin>,
    ) -> Result<ImagePulled, RpcError> {
        crate::image::pull_cluster_image(
            self,
            machine_id,
            &spec.container.image,
            spec.container.pull_policy,
            auth,
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

pub(in crate::deploy) fn omission_requires_observation(
    membership: Option<&MembershipObservation>,
) -> bool {
    membership.is_none_or(MembershipObservation::invites_rpc)
}

/// A Client whose Container changes carry a Deploy's or a drain's approvals to each
/// Server.
#[derive(Clone)]
pub(crate) struct Approved {
    pub(crate) client: Client,
    approvals: Arc<Approvals>,
}

impl Approved {
    pub(crate) fn new(client: Client, approvals: Approvals) -> Self {
        Self {
            client,
            approvals: Arc::new(approvals),
        }
    }

    /// Whether a new creation of `spec` in `namespace` carries a Free receipt.
    pub(crate) async fn carries_free(
        &self,
        namespace: &ployz_core::Namespace,
        spec: &ployz_core::ResolvedServiceSpec,
    ) -> bool {
        self.approvals.carries_free(namespace, spec).await
    }
}

impl Session for Approved {
    async fn redial(&self) -> Option<Result<(), RedialFailure>> {
        self.client.redial().await
    }
}

impl CopyLifecycle for Approved {
    async fn service_containers(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError> {
        self.client.service_containers(service).await
    }

    async fn listed_copies(&self, service: &QualifiedService) -> Result<Listed, RpcError> {
        self.client.listed_copies(service).await
    }

    async fn target_copies(
        &self,
        machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError> {
        self.client.target_copies(machine_id, service).await
    }

    async fn replicated_containers(&self) -> Result<Vec<ContainerObservation>, RpcError> {
        self.client.replicated_containers().await
    }

    async fn ensure_container(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
    ) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError> {
        self.client
            .ensure_settled(machine_id, want, Some(&self.approvals))
            .await
    }

    async fn ensure_unsettled(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        timeout: Option<Duration>,
    ) -> Result<EnsuredContainer, RpcError> {
        self.client
            .ensure_approved::<op::EnsureContainer>(
                machine_id,
                want,
                Some(&self.approvals),
                timeout,
            )
            .await
    }

    async fn ensure_new(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
    ) -> Result<EnsuredContainer, RpcError> {
        self.client
            .ensure_created(machine_id, want, Some(&self.approvals))
            .await
    }

    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerDetails, RpcError> {
        self.client
            .inspect_container(machine_id, container_id)
            .await
    }

    async fn read_routes(
        &self,
        machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<RoutesRead, RpcError> {
        self.client.read_routes(machine_id, service).await
    }

    async fn read_entry_routes(&self, service: &QualifiedService) -> Result<RoutesRead, RpcError> {
        self.client.read_entry_routes(service).await
    }
}

impl DeployOperations for Approved {
    async fn prepare_volumes(
        &self,
        machine_id: &MachineId,
        specs: &[ployz_core::ServiceStorageSpec],
    ) -> Result<ployz_core::PreparedVolumes, RpcError> {
        self.client.prepare_volumes(machine_id, specs).await
    }

    async fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError> {
        self.client.remove_volume(id).await
    }

    async fn update_container_limits(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerLimitsUpdated, RpcError> {
        self.client
            .update_container_limits(machine_id, container_id, spec)
            .await
    }

    async fn update_container_placement(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerPlacementUpdated, RpcError> {
        self.client
            .update_container_placement(machine_id, container_id, spec)
            .await
    }

    async fn runs_global(
        &self,
        machine_id: &MachineId,
        slot: &SlotKey,
        spec: &ResolvedServiceSpec,
    ) -> Result<bool, RpcError> {
        self.client.runs_global(machine_id, slot, spec).await
    }

    async fn ensure_image(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
        auth: Option<SealedLogin>,
    ) -> Result<ImagePulled, RpcError> {
        self.client.ensure_image(machine_id, spec, auth).await
    }
}

impl Client {
    /// [`CopyLifecycle::ensure_container`], carrying what `approvals` hand the Server.
    async fn ensure_settled(
        &self,
        machine_id: &MachineId,
        mut want: ContainerWant,
        approvals: Option<&Approvals>,
    ) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError> {
        let timeout = match &mut want {
            ContainerWant::Running {
                container: RunTarget::New(new),
                ..
            } => {
                self.prepare_new(machine_id, new).await?;
                None
            }
            ContainerWant::Running { .. }
            | ContainerWant::Out { .. }
            | ContainerWant::Held { .. } => Some(TARGET_RPC_TIMEOUT),
            ContainerWant::Stopped { stop, .. } => stop_rpc_timeout(stop.grace_period_seconds, 1),
            ContainerWant::Gone { .. } => None,
        };
        self.ensure_approved::<op::EnsureContainer>(machine_id, want, approvals, timeout)
            .await?
            .into_settled()
    }

    /// [`CopyLifecycle::ensure_new`], carrying what `approvals` hand the Server.
    async fn ensure_created(
        &self,
        machine_id: &MachineId,
        mut want: ContainerWant,
        approvals: Option<&Approvals>,
    ) -> Result<EnsuredContainer, RpcError> {
        let mut pinned = false;
        if let ContainerWant::Running {
            container: RunTarget::New(new),
            ..
        } = &mut want
        {
            self.prepare_new(machine_id, new).await?;
            pinned = new.image_id.is_some();
        }
        if pinned {
            self.ensure_approved::<op::EnsurePinnedContainer>(machine_id, want, approvals, None)
                .await
        } else {
            self.ensure_approved::<op::EnsureContainer>(machine_id, want, approvals, None)
                .await
        }
    }

    /// Send `want` on route `R` with what `approvals` hand the Server, and keep the
    /// verified build it issued for a Service the change creates.
    async fn ensure_approved<R>(
        &self,
        machine_id: &MachineId,
        mut want: ContainerWant,
        approvals: Option<&Approvals>,
        timeout: Option<Duration>,
    ) -> Result<EnsuredContainer, RpcError>
    where
        R: ployz_core::Rpc<
                Request = ployz_core::EnsureContainerRequest,
                Response = EnsuredContainer,
            >,
    {
        let created = if let ContainerWant::Running {
            container: RunTarget::New(new),
            ..
        } = &mut want
        {
            Some(new.slot.service.clone())
        } else {
            None
        };
        // Boxed as a trait object so the Store and Cloud reads behind a kept version stay
        // out of the type of every Deploy future, which would otherwise outgrow the
        // compiler's recursion limit when the napi SDK checks it is Send.
        let carry = async |want: &ContainerWant| match approvals {
            Some(approvals) => {
                let carried: std::pin::Pin<Box<dyn Future<Output = Carried> + Send + '_>> =
                    Box::pin(approvals.carried(want));
                carried.await
            }
            None => Carried::default(),
        };
        let ensured = self.ensure::<R>(machine_id, want, carry, timeout).await?;
        if let (
            Some(service),
            EnsuredContainer::Ensured {
                verified: Some(verified),
                ..
            },
            Some(approvals),
        ) = (&created, &ensured, approvals)
        {
            approvals.note_issued(service, *verified);
        }
        Ok(ensured)
    }

    /// A retained Global's key for a new creation, so a retry lands on the Container
    /// already there.
    async fn prepare_new(
        &self,
        machine_id: &MachineId,
        new: &mut ployz_core::NewContainer,
    ) -> Result<(), RpcError> {
        if let Some(key) = self
            .retained_global_key(machine_id, &new.slot, &new.resolved_spec)
            .await?
        {
            new.slot.creation_key = key;
        }
        Ok(())
    }

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
                Call::target_read(&MachineTarget::from(machine_id)),
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

/// How a new Container's slot is keyed. A Global Service Container is keyed by
/// its spec, so a retry or another run lands on the Container already there;
/// every other Container is keyed by the run that creates it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::deploy) enum SlotPolicy {
    BySpec,
    PerRun,
}

impl SlotPolicy {
    pub(in crate::deploy) fn of(kind: ContainerKind, mode: &ployz_core::ServiceMode) -> Self {
        if kind == ContainerKind::ServiceContainer && *mode == ployz_core::ServiceMode::Global {
            Self::BySpec
        } else {
            Self::PerRun
        }
    }

    pub(in crate::deploy) fn keyed_by_spec(self) -> bool {
        self == Self::BySpec
    }

    /// The slot of a new Container of `spec`. `per_run` names it when the spec does not.
    pub(in crate::deploy) fn slot(
        self,
        namespace: &Namespace,
        kind: ContainerKind,
        spec: &ResolvedServiceSpec,
        replacing: Option<ContainerId>,
        per_run: impl FnOnce() -> CreationKey,
    ) -> SlotKey {
        let creation_key = match self {
            Self::BySpec => {
                let key = crate::cluster::global_creation_key(spec);
                // Explicit replacement can overlap even an identical spec.
                let key = match replacing {
                    Some(old) => format!("{key}:replace:{old}"),
                    None => key,
                };
                CreationKey::try_from(key).expect("creation keys stay within 512 bytes")
            }
            Self::PerRun => per_run(),
        };
        SlotKey {
            namespace: namespace.clone(),
            service: spec.name.clone(),
            kind,
            creation_key,
        }
    }
}

/// The Machine that answered `error`, for a failure sentence that names the Server.
pub(in crate::deploy) fn naming_machine(mut error: RpcError, machine_id: &MachineId) -> RpcError {
    if let serde_json::Value::Object(details) = &mut error.details {
        details.insert("machine_id".into(), serde_json::json!(machine_id));
    } else {
        error.details = serde_json::json!({ "machine_id": machine_id });
    }
    error
}
