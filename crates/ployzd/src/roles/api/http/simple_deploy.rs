//! One whole deploy run for the preferred controller.
//!
//! Every invocation observes Corrosion and target hosts, derives one complete
//! placement, performs coarse idempotent host effects, and publishes one row
//! flip. There is deliberately no phase journal or recovery state machine.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use ployz_core::corrosion::{
    ContainerDocument, ControllerAppointmentId, ControllerDocument, CorrosionDeployFailure,
    CorrosionDeployOutcome, CorrosionDeployWarning, CorrosionDocumentVersion, CorrosionTimestamp,
    HostPortBindings, MachineLoadBand, NamespaceDocument, OperationDocument, OperationInitiator,
    OperatorWriteProvenance, RouteBindingDocument, ServiceDocument, ServicePlacement,
    ServiceReplicaCount, V2ManagedContainerIdentity, fingerprint_env_value,
    owns_current_controller_appointment,
};
use ployz_core::deploy::{ReplicaSlot, ReplicatedReplicaSlot};
use ployz_core::ids::{
    ContainerId, MachineRowId, NamespaceRowId, OperationRowId, RouteBindingRowId, ServiceRowId,
};
use ployz_core::machine::{MachineLifecycle, MachineName};
use ployz_core::placement::{
    PlacementBid, PlacementPickInputs, PlacementRefusal, ServiceContainerObservation,
    pick_placement,
};
use ployz_core::{
    DeployDesiredReplica, DeployInspectOutcome, DeployInspectRequest, DeployObservedContainer,
    DeployPrepareOutcome, DeployPrepareRequest, DeployPreparedReplica, DeployRequest,
    DeployRetireOutcome, DeployRetireRequest, HealthGatePolicy, RequestedPins, RequestedPlacement,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// The only machine-addressed seam used by the controller.
///
/// Its production adapter chooses local execution or bounded mesh HTTP. Both
/// paths expose the same three coarse target-host operations.
#[async_trait]
pub(super) trait DeployHosts: Send + Sync {
    async fn inspect(
        &self,
        machine_id: &MachineRowId,
        request: DeployInspectRequest,
    ) -> Result<DeployInspectOutcome, DeployHostError>;

    async fn prepare(
        &self,
        machine_id: &MachineRowId,
        request: DeployPrepareRequest,
    ) -> Result<DeployPrepareOutcome, DeployHostError>;

    async fn retire(
        &self,
        machine_id: &MachineRowId,
        request: DeployRetireRequest,
    ) -> Result<DeployRetireOutcome, DeployHostError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DeployHostError {
    StaleController,
    Failed,
}

/// Row reads and writes kept behind one deploy-shaped seam.
#[async_trait]
pub(super) trait SimpleDeployStore: Send + Sync {
    async fn controller(&self) -> Result<ControllerDocument, String>;

    async fn observe(&self, command: &DeployCommand) -> Result<DeployReality, String>;

    async fn write_operation(
        &self,
        operation_id: &OperationRowId,
        document: &OperationDocument,
    ) -> Result<(), String>;

    /// Atomically replaces the service's serving projection.
    async fn commit(&self, commit: DeployCommit) -> Result<(), String>;
}

/// A controller's complete view needed to decide one deploy.
#[derive(Debug, Clone)]
pub(super) struct DeployReality {
    pub(super) namespace_id: NamespaceRowId,
    pub(super) namespace: NamespaceDocument,
    pub(super) services: Vec<ObservedServiceRow>,
    /// A deterministic automatic route needed by this deploy, if any.
    pub(super) automatic_route: Option<DesiredRouteRow>,
    /// True only when an observed route already occupies an otherwise empty
    /// namespace. A merely planned automatic route does not set this flag.
    pub(super) routes_without_service: bool,
    pub(super) roster: Vec<DeployRosterMachine>,
}

#[derive(Debug, Clone)]
pub(super) struct ObservedServiceRow {
    pub(super) id: ServiceRowId,
    pub(super) document: ServiceDocument,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DesiredRouteRow {
    pub(super) id: RouteBindingRowId,
    pub(super) document: RouteBindingDocument,
}

#[derive(Debug, Clone)]
pub(super) struct DeployRosterMachine {
    pub(super) id: MachineRowId,
    pub(super) name: MachineName,
    pub(super) lifecycle: MachineLifecycle,
    /// Missing status makes the machine ineligible for placement.
    pub(super) status: Option<DeployMachineStatus>,
}

#[derive(Debug, Clone)]
pub(super) struct DeployMachineStatus {
    pub(super) free_disk_bytes: u64,
    pub(super) load: MachineLoadBand,
}

/// One admitted controller command. Target hosts persist their own coarse
/// effects; the preferred controller itself owns no workflow history.
#[derive(Clone)]
pub(super) struct DeployCommand {
    pub(super) operation_id: OperationRowId,
    pub(super) request: DeployRequest,
    pub(super) initiator: OperationInitiator,
    pub(super) appointment_id: ControllerAppointmentId,
}

#[derive(Debug, Clone)]
pub(super) struct DeployCommit {
    pub(super) service_id: ServiceRowId,
    pub(super) service: ServiceDocument,
    pub(super) containers: Vec<DesiredContainerRow>,
    pub(super) automatic_route: Option<DesiredRouteRow>,
}

#[derive(Debug, Clone)]
pub(super) struct DesiredContainerRow {
    pub(super) id: ContainerId,
    pub(super) document: ContainerDocument,
}

/// One concrete preferred-controller deploy. There are no knobs for alternate
/// orchestration strategies: the one implementation is the product policy.
/// `ponytail:` target calls stay serial until measured small-cluster latency
/// justifies parallel fan-out.
pub(super) struct SimpleDeploy {
    machine_id: MachineRowId,
    store: Arc<dyn SimpleDeployStore>,
    hosts: Arc<dyn DeployHosts>,
}

impl SimpleDeploy {
    #[must_use]
    pub(super) fn new(
        machine_id: MachineRowId,
        store: Arc<dyn SimpleDeployStore>,
        hosts: Arc<dyn DeployHosts>,
    ) -> Self {
        Self {
            machine_id,
            store,
            hosts,
        }
    }

    pub(super) async fn run(
        &self,
        command: &DeployCommand,
    ) -> Result<CorrosionDeployOutcome, String> {
        let reality = self.store.observe(command).await?;
        let context = classify_service(&command.request, reality)?;
        let created_at = now()?;
        let created = OperationDocument::deploy_created(
            CorrosionDocumentVersion::V1,
            context.reality.namespace.cluster_id.clone(),
            self.machine_id.clone(),
            command.initiator.clone(),
            context.reality.namespace_id.clone(),
            context.service_id.clone(),
            created_at,
        );
        self.store
            .write_operation(&command.operation_id, &created)
            .await?;

        if !self.appointment_is_current(command).await? {
            return self.interrupt(command, &created).await;
        }

        if let Some(failure) = context.admission_failure {
            return self.fail(command, &created, failure).await;
        }
        let Some(inspections) = self
            .inspect_roster(&context.reality, &command.appointment_id)
            .await
        else {
            return self.interrupt(command, &created).await;
        };
        if !command.request.runtime.volume_mounts.is_empty()
            && let Some(machine_id) = context.reality.roster.iter().find_map(|machine| {
                inspections.get(&machine.id).and_then(|inspection| {
                    inspection
                        .containers
                        .iter()
                        .any(|container| {
                            container.identity.namespace_id == context.reality.namespace_id
                                && container.identity.operation_id != command.operation_id
                        })
                        .then(|| machine.id.clone())
                })
            })
        {
            return self
                .fail(
                    command,
                    &created,
                    CorrosionDeployFailure::PrepareRefused { machine_id },
                )
                .await;
        }

        let placement = match derive_placement(&command.request, &context, &inspections) {
            Ok(placement) => placement,
            Err(refusal) => {
                return self
                    .fail(
                        command,
                        &created,
                        CorrosionDeployFailure::Placement { refusal },
                    )
                    .await;
            }
        };
        let desired = desired_replicas(
            &command.operation_id,
            &context.reality.namespace_id,
            &context.service_id,
            &placement,
        )?;

        let mut prepared = Vec::new();
        let mut resolved_image = None;
        for (machine_id, replicas) in group_by_machine(&desired) {
            if !self.appointment_is_current(command).await? {
                return self.interrupt(command, &created).await;
            }
            let request = DeployPrepareRequest {
                appointment_id: command.appointment_id.clone(),
                operation_id: command.operation_id.clone(),
                namespace_name: context.reality.namespace.name.clone(),
                image: command.request.image.clone(),
                credential: command.request.credential.clone(),
                runtime: command.request.runtime.clone(),
                health_gate: command.request.health_gate,
                replicas: replicas.clone(),
            };
            let outcome = self.hosts.prepare(&machine_id, request).await;
            let (image, replicas) = match outcome {
                Ok(DeployPrepareOutcome::Prepared { image, replicas }) => (image, replicas),
                Ok(DeployPrepareOutcome::Refused { .. }) => {
                    return self
                        .fail(
                            command,
                            &created,
                            CorrosionDeployFailure::PrepareRefused { machine_id },
                        )
                        .await;
                }
                Err(DeployHostError::StaleController) => {
                    return self.interrupt(command, &created).await;
                }
                Ok(DeployPrepareOutcome::Failed { .. }) | Err(DeployHostError::Failed) => {
                    return self
                        .fail(
                            command,
                            &created,
                            CorrosionDeployFailure::PrepareFailed { machine_id },
                        )
                        .await;
                }
            };
            if !prepared_matches(&replicas, &replicas_for(&desired, &machine_id)) {
                return self
                    .fail(
                        command,
                        &created,
                        CorrosionDeployFailure::PreparedReplicaMismatch { machine_id },
                    )
                    .await;
            }
            if resolved_image
                .as_ref()
                .is_some_and(|resolved| resolved != &image)
            {
                return self
                    .fail(
                        command,
                        &created,
                        CorrosionDeployFailure::ResolvedImageMismatch,
                    )
                    .await;
            }
            resolved_image = Some(image);
            prepared.extend(replicas.into_iter().map(|replica| PlacedReplica {
                machine_id: machine_id.clone(),
                replica,
            }));
        }
        let Some(resolved_image) = resolved_image else {
            return self
                .fail(
                    command,
                    &created,
                    CorrosionDeployFailure::RuntimeRealityUnavailable,
                )
                .await;
        };
        if !self.appointment_is_current(command).await? {
            return self.interrupt(command, &created).await;
        }

        let written_at = now()?;
        let commit = build_commit(
            command,
            &context,
            &placement,
            resolved_image,
            &prepared,
            written_at,
        )?;
        if self.store.commit(commit).await.is_err() {
            // A lost write reply cannot distinguish rejection from a committed
            // local transaction. End honestly and let a new command re-plan
            // from the service/container rows that actually exist.
            return self.interrupt(command, &created).await;
        }

        let keep = prepared
            .iter()
            .map(|placed| {
                (
                    placed.machine_id.clone(),
                    placed.replica.container_id.clone(),
                )
            })
            .collect();
        self.finish_after_flip(command, &created, &context, &inspections, &keep)
            .await
    }

    async fn appointment_is_current(&self, command: &DeployCommand) -> Result<bool, String> {
        let controller = self.store.controller().await?;
        Ok(owns_current_controller_appointment(
            &controller,
            &self.machine_id,
            &command.appointment_id,
        ))
    }

    async fn inspect_roster(
        &self,
        reality: &DeployReality,
        appointment_id: &ControllerAppointmentId,
    ) -> Option<BTreeMap<MachineRowId, HostInspection>> {
        let mut inspections = BTreeMap::new();
        for machine in &reality.roster {
            let request = DeployInspectRequest {
                appointment_id: appointment_id.clone(),
            };
            let outcome = self.hosts.inspect(&machine.id, request).await;
            match outcome {
                Err(DeployHostError::StaleController) => return None,
                Ok(DeployInspectOutcome::Inspected {
                    bridge_ready,
                    containers,
                }) => {
                    inspections.insert(
                        machine.id.clone(),
                        HostInspection {
                            bridge_ready,
                            containers,
                        },
                    );
                }
                Ok(DeployInspectOutcome::Failed { .. }) | Err(DeployHostError::Failed) => {}
            }
        }
        Some(inspections)
    }

    async fn finish_after_flip(
        &self,
        command: &DeployCommand,
        created: &OperationDocument,
        context: &DeployContext,
        inspections: &BTreeMap<MachineRowId, HostInspection>,
        keep: &BTreeSet<(MachineRowId, ContainerId)>,
    ) -> Result<CorrosionDeployOutcome, String> {
        let mut cleanup_failed = Vec::new();
        for (machine_id, inspection) in inspections {
            let containers = inspection
                .containers
                .iter()
                .filter(|container| {
                    container.identity.namespace_id == context.reality.namespace_id
                        && container.identity.service_id == context.service_id
                        && !keep.contains(&(machine_id.clone(), container.container_id.clone()))
                })
                .cloned()
                .collect::<Vec<_>>();
            if containers.is_empty() {
                continue;
            }
            let outcome = self
                .hosts
                .retire(
                    machine_id,
                    DeployRetireRequest {
                        appointment_id: command.appointment_id.clone(),
                        operation_id: command.operation_id.clone(),
                        containers,
                    },
                )
                .await;
            if !matches!(outcome, Ok(DeployRetireOutcome::Retired)) {
                cleanup_failed.push(machine_id.clone());
            }
        }
        let mut warnings = Vec::new();
        if command.request.health_gate == HealthGatePolicy::Skip {
            warnings.push(CorrosionDeployWarning::HealthGateSkipped);
        }
        if !cleanup_failed.is_empty() {
            warnings.push(CorrosionDeployWarning::CleanupIncomplete {
                machines: cleanup_failed,
            });
        }
        self.finish(
            command,
            created,
            CorrosionDeployOutcome::Completed { warnings },
        )
        .await
    }

    async fn fail(
        &self,
        command: &DeployCommand,
        created: &OperationDocument,
        failure: CorrosionDeployFailure,
    ) -> Result<CorrosionDeployOutcome, String> {
        self.finish(command, created, CorrosionDeployOutcome::Failed { failure })
            .await
    }

    async fn interrupt(
        &self,
        command: &DeployCommand,
        created: &OperationDocument,
    ) -> Result<CorrosionDeployOutcome, String> {
        self.finish(command, created, CorrosionDeployOutcome::Interrupted)
            .await
    }

    async fn finish(
        &self,
        command: &DeployCommand,
        created: &OperationDocument,
        outcome: CorrosionDeployOutcome,
    ) -> Result<CorrosionDeployOutcome, String> {
        let completed_at = now()?;
        let operation = created.clone().into_terminal(completed_at, outcome.clone());
        self.store
            .write_operation(&command.operation_id, &operation)
            .await?;
        Ok(outcome)
    }
}

fn now() -> Result<CorrosionTimestamp, String> {
    let value = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| error.to_string())?;
    CorrosionTimestamp::try_new(value).map_err(|error| error.to_string())
}

struct DeployContext {
    reality: DeployReality,
    service_id: ServiceRowId,
    incumbent: Option<ObservedServiceRow>,
    admission_failure: Option<CorrosionDeployFailure>,
}

fn classify_service(
    request: &DeployRequest,
    reality: DeployReality,
) -> Result<DeployContext, String> {
    // A namespace holds at most one service, so its row id is also the stable
    // identity for that service. Resubmitting after a controller loss must
    // rediscover prepared containers under the same service identity even
    // though the new operation has a different id.
    let generated_service = ServiceRowId::try_new(reality.namespace_id.as_str().to_owned())
        .map_err(|error| format!("namespace id could not become a stable service id: {error}"))?;
    let (service_id, incumbent, admission_failure) = match reality.services.as_slice() {
        [] if reality.routes_without_service => (
            generated_service,
            None,
            Some(CorrosionDeployFailure::RoutesWithoutService),
        ),
        [] => (generated_service, None, None),
        [service] if service.document.name == request.service_name => {
            (service.id.clone(), Some(service.clone()), None)
        }
        [service] => (
            generated_service,
            Some(service.clone()),
            Some(CorrosionDeployFailure::DifferentService {
                incumbent_name: service.document.name.clone(),
            }),
        ),
        [_, _, ..] => (
            generated_service,
            None,
            Some(CorrosionDeployFailure::MultipleServices),
        ),
    };
    let admission_failure = admission_failure
        .or_else(|| replicas_on_global(request, incumbent.as_ref()))
        .or_else(|| unknown_pin(request, &reality));
    Ok(DeployContext {
        reality,
        service_id,
        incumbent,
        admission_failure,
    })
}

fn replicas_on_global(
    request: &DeployRequest,
    incumbent: Option<&ObservedServiceRow>,
) -> Option<CorrosionDeployFailure> {
    if !matches!(
        &request.placement,
        Some(RequestedPlacement::Replicas { .. })
    ) {
        return None;
    }
    incumbent
        .filter(|service| matches!(service.document.placement, ServicePlacement::Global { .. }))
        .map(|_| CorrosionDeployFailure::ReplicasOnGlobalService)
}

fn unknown_pin(request: &DeployRequest, reality: &DeployReality) -> Option<CorrosionDeployFailure> {
    let Some(RequestedPins::Machines { names }) = &request.machines else {
        return None;
    };
    names
        .iter()
        .find(|name| !reality.roster.iter().any(|machine| &machine.name == *name))
        .cloned()
        .map(|machine_name| CorrosionDeployFailure::UnknownPinnedMachine { machine_name })
}

#[derive(Debug, Clone)]
struct HostInspection {
    bridge_ready: bool,
    containers: Vec<DeployObservedContainer>,
}

struct EffectivePlacement {
    placement: ServicePlacement,
    pinned_machines: BTreeSet<MachineRowId>,
    targets: Vec<MachineRowId>,
}

fn derive_placement(
    request: &DeployRequest,
    context: &DeployContext,
    inspections: &BTreeMap<MachineRowId, HostInspection>,
) -> Result<EffectivePlacement, PlacementRefusal> {
    let placement = effective_placement(request, context);
    let pinned_machines = resolve_pins(request, context);
    let active_deploy = context
        .incumbent
        .as_ref()
        .map(|service| service.document.active_deploy.clone());
    let mut bids = Vec::new();
    for machine in &context.reality.roster {
        let Some(inspection) = inspections.get(&machine.id) else {
            continue;
        };
        let Some(status) = &machine.status else {
            continue;
        };
        if !inspection.bridge_ready {
            continue;
        }
        bids.push(PlacementBid {
            machine_id: machine.id.clone(),
            machine_name: machine.name.clone(),
            lifecycle: machine.lifecycle,
            free_disk_bytes: status.free_disk_bytes,
            load: status.load,
            total_container_count: inspection.containers.len(),
            service_containers: inspection
                .containers
                .iter()
                .map(|container| ServiceContainerObservation {
                    deploy: container.identity.operation_id.clone(),
                })
                .collect(),
        });
    }
    let pick = pick_placement(&PlacementPickInputs {
        placement: placement.clone(),
        pinned_machines: pinned_machines.clone(),
        has_named_volumes: !request.runtime.volume_mounts.is_empty(),
        active_deploy,
        bids,
    })?;
    Ok(EffectivePlacement {
        placement,
        pinned_machines,
        targets: pick,
    })
}

fn effective_placement(request: &DeployRequest, context: &DeployContext) -> ServicePlacement {
    let inherited = context
        .incumbent
        .as_ref()
        .map(|service| service.document.placement.clone());
    let one = || ServiceReplicaCount::try_new(1).expect("one replica is valid");
    match (&request.placement, inherited) {
        (Some(RequestedPlacement::Replicas { replicas }), _)
        | (
            Some(RequestedPlacement::Replicated {
                replicas: Some(replicas),
            }),
            _,
        ) => ServicePlacement::Replicated {
            replicas: *replicas,
        },
        (
            Some(RequestedPlacement::Replicated { replicas: None }),
            Some(ServicePlacement::Replicated { replicas }),
        ) => ServicePlacement::Replicated { replicas },
        (Some(RequestedPlacement::Replicated { replicas: None }), _) => {
            ServicePlacement::Replicated { replicas: one() }
        }
        (Some(RequestedPlacement::Global { host_ports }), _) => ServicePlacement::Global {
            host_ports: host_ports.clone(),
        },
        (None, Some(placement)) => placement,
        (None, None) => ServicePlacement::Replicated { replicas: one() },
    }
}

fn resolve_pins(request: &DeployRequest, context: &DeployContext) -> BTreeSet<MachineRowId> {
    match &request.machines {
        Some(RequestedPins::Machines { names }) => context
            .reality
            .roster
            .iter()
            .filter(|machine| names.iter().any(|name| name == &machine.name))
            .map(|machine| machine.id.clone())
            .collect(),
        Some(RequestedPins::Any) => BTreeSet::new(),
        None => context
            .incumbent
            .as_ref()
            .map(|service| service.document.pinned_machines.clone())
            .unwrap_or_default(),
    }
}

#[derive(Clone)]
struct DesiredReplica {
    machine_id: MachineRowId,
    desired: DeployDesiredReplica,
}

fn desired_replicas(
    operation_id: &OperationRowId,
    namespace_id: &NamespaceRowId,
    service_id: &ServiceRowId,
    placement: &EffectivePlacement,
) -> Result<Vec<DesiredReplica>, String> {
    let host_ports = match &placement.placement {
        ServicePlacement::Global { host_ports } => host_ports.clone(),
        ServicePlacement::Replicated { .. } => HostPortBindings::default(),
    };
    placement
        .targets
        .iter()
        .enumerate()
        .map(|(index, machine_id)| {
            let replica_slot = match &placement.placement {
                ServicePlacement::Global { .. } => ReplicaSlot::Global,
                ServicePlacement::Replicated { .. } => {
                    let number = u16::try_from(index + 1)
                        .map_err(|_| "deploy produced too many replica slots".to_owned())?;
                    ReplicaSlot::Replicated {
                        number: ReplicatedReplicaSlot::try_new(number)
                            .map_err(|error| error.to_string())?,
                    }
                }
            };
            Ok(DesiredReplica {
                machine_id: machine_id.clone(),
                desired: DeployDesiredReplica {
                    identity: V2ManagedContainerIdentity {
                        namespace_id: namespace_id.clone(),
                        service_id: service_id.clone(),
                        operation_id: operation_id.clone(),
                        replica_slot,
                    },
                    host_ports: host_ports.clone(),
                },
            })
        })
        .collect()
}

fn group_by_machine(
    desired: &[DesiredReplica],
) -> BTreeMap<MachineRowId, Vec<DeployDesiredReplica>> {
    let mut grouped = BTreeMap::new();
    for replica in desired {
        grouped
            .entry(replica.machine_id.clone())
            .or_insert_with(Vec::new)
            .push(replica.desired.clone());
    }
    grouped
}

fn replicas_for(
    desired: &[DesiredReplica],
    machine_id: &MachineRowId,
) -> Vec<DeployDesiredReplica> {
    desired
        .iter()
        .filter(|replica| &replica.machine_id == machine_id)
        .map(|replica| replica.desired.clone())
        .collect()
}

fn prepared_matches(prepared: &[DeployPreparedReplica], desired: &[DeployDesiredReplica]) -> bool {
    prepared.len() == desired.len()
        && prepared.iter().all(|prepared| {
            desired
                .iter()
                .any(|desired| desired.identity == prepared.identity)
        })
        && desired.iter().all(|desired| {
            prepared
                .iter()
                .filter(|prepared| prepared.identity == desired.identity)
                .count()
                == 1
        })
}

struct PlacedReplica {
    machine_id: MachineRowId,
    replica: DeployPreparedReplica,
}

fn build_commit(
    command: &DeployCommand,
    context: &DeployContext,
    placement: &EffectivePlacement,
    resolved_image: ployz_core::deploy::ImageReference,
    prepared: &[PlacedReplica],
    written_at: CorrosionTimestamp,
) -> Result<DeployCommit, String> {
    let env_fingerprints = command
        .request
        .runtime
        .environment
        .iter()
        .map(|(name, value)| {
            fingerprint_env_value(value)
                .map(|fingerprint| (name.as_str().to_owned(), fingerprint))
                .map_err(|error| error.to_string())
        })
        .collect::<Result<_, _>>()?;
    let service = ServiceDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: context.reality.namespace.cluster_id.clone(),
        provenance: OperatorWriteProvenance {
            written_by: command.initiator.clone(),
            written_at,
        },
        namespace_id: context.reality.namespace_id.clone(),
        name: command.request.service_name.clone(),
        image: resolved_image,
        env_fingerprints,
        placement: placement.placement.clone(),
        pinned_machines: placement.pinned_machines.clone(),
        active_deploy: command.operation_id.clone(),
        previous_image: context
            .incumbent
            .as_ref()
            .map(|incumbent| incumbent.document.image.clone()),
        deployed_at: written_at,
        operation_id: command.operation_id.clone(),
    };
    let containers = prepared
        .iter()
        .map(|placed| DesiredContainerRow {
            id: placed.replica.container_id.clone(),
            document: ContainerDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id: context.reality.namespace.cluster_id.clone(),
                machine_id: placed.machine_id.clone(),
                service_id: context.service_id.clone(),
                namespace_id: context.reality.namespace_id.clone(),
                replica_slot: placed.replica.identity.replica_slot,
                ip: placed.replica.ip,
                deploy: command.operation_id.clone(),
            },
        })
        .collect();
    Ok(DeployCommit {
        service_id: context.service_id.clone(),
        service,
        containers,
        automatic_route: context.reality.automatic_route.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use ployz_core::corrosion::{
        CorrosionDeployState, CorrosionNamespaceName, CorrosionServiceName, Principal,
    };
    use ployz_core::deploy::{ContainerRuntimeSpec, ImageReference};
    use ployz_core::ids::{ClusterId, PeerId};
    use ployz_core::image::OciDigest;
    use tokio::sync::Mutex;

    use super::*;

    struct FakeStore {
        controller: Mutex<ControllerDocument>,
        operation: Mutex<Option<OperationDocument>>,
        reality: Mutex<DeployReality>,
        commits: Mutex<Vec<DeployCommit>>,
    }

    #[async_trait]
    impl SimpleDeployStore for FakeStore {
        async fn controller(&self) -> Result<ControllerDocument, String> {
            Ok(self.controller.lock().await.clone())
        }

        async fn observe(&self, _command: &DeployCommand) -> Result<DeployReality, String> {
            Ok(self.reality.lock().await.clone())
        }

        async fn write_operation(
            &self,
            _operation_id: &OperationRowId,
            document: &OperationDocument,
        ) -> Result<(), String> {
            *self.operation.lock().await = Some(document.clone());
            Ok(())
        }

        async fn commit(&self, commit: DeployCommit) -> Result<(), String> {
            self.commits.lock().await.push(commit);
            Ok(())
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum HostCall {
        Inspect(MachineRowId),
        Prepare(MachineRowId),
        Retire(MachineRowId),
    }

    struct FakeHosts {
        calls: Mutex<Vec<HostCall>>,
        stale_prepare: AtomicBool,
        next_container: AtomicUsize,
    }

    #[async_trait]
    impl DeployHosts for FakeHosts {
        async fn inspect(
            &self,
            machine_id: &MachineRowId,
            _request: DeployInspectRequest,
        ) -> Result<DeployInspectOutcome, DeployHostError> {
            self.calls
                .lock()
                .await
                .push(HostCall::Inspect(machine_id.clone()));
            Ok(DeployInspectOutcome::Inspected {
                bridge_ready: true,
                containers: Vec::new(),
            })
        }

        async fn prepare(
            &self,
            machine_id: &MachineRowId,
            request: DeployPrepareRequest,
        ) -> Result<DeployPrepareOutcome, DeployHostError> {
            self.calls
                .lock()
                .await
                .push(HostCall::Prepare(machine_id.clone()));
            if self.stale_prepare.load(Ordering::SeqCst) {
                return Err(DeployHostError::StaleController);
            }
            let digest = OciDigest::try_new(format!("sha256:{}", "c".repeat(64)))
                .map_err(|_| DeployHostError::Failed)?;
            let image = request
                .image
                .with_digest(&digest)
                .map_err(|_| DeployHostError::Failed)?;
            let replicas = request
                .replicas
                .into_iter()
                .map(|desired| {
                    let number = self.next_container.fetch_add(1, Ordering::SeqCst);
                    Ok(DeployPreparedReplica {
                        container_id: ContainerId::try_new(format!("prepared-{number}"))
                            .map_err(|_| DeployHostError::Failed)?,
                        identity: desired.identity,
                        ip: Ipv4Addr::new(10, 210, 20, 2),
                    })
                })
                .collect::<Result<Vec<_>, DeployHostError>>()?;
            Ok(DeployPrepareOutcome::Prepared { image, replicas })
        }

        async fn retire(
            &self,
            machine_id: &MachineRowId,
            _request: DeployRetireRequest,
        ) -> Result<DeployRetireOutcome, DeployHostError> {
            self.calls
                .lock()
                .await
                .push(HostCall::Retire(machine_id.clone()));
            Ok(DeployRetireOutcome::Retired)
        }
    }

    struct Fixture {
        executor: SimpleDeploy,
        store: Arc<FakeStore>,
        hosts: Arc<FakeHosts>,
        command: DeployCommand,
        machines: Vec<MachineRowId>,
    }

    fn fixture(roster_members: usize) -> Fixture {
        let cluster_id = ClusterId::try_new("01J00000000000000000000001").expect("cluster");
        let machine_ids = [
            "01J00000000000000000000002",
            "01J00000000000000000000003",
            "01J00000000000000000000004",
        ]
        .into_iter()
        .take(roster_members)
        .map(|id| MachineRowId::try_new(id).expect("machine"))
        .collect::<Vec<_>>();
        let machine_id = machine_ids.first().cloned().expect("roster member");
        let appointment_id =
            ControllerAppointmentId::try_new("01J00000000000000000000005").expect("appointment");
        let namespace_id =
            NamespaceRowId::try_new("01J00000000000000000000006").expect("namespace");
        let operation_id =
            OperationRowId::try_new("01J00000000000000000000007").expect("operation");
        let initiator = Principal::Peer {
            peer_id: PeerId::try_new("01J00000000000000000000008").expect("peer"),
        };
        let at = CorrosionTimestamp::try_new("2026-08-08T00:00:00Z").expect("time");
        let namespace = NamespaceDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id.clone(),
            provenance: OperatorWriteProvenance {
                written_by: initiator.clone(),
                written_at: at,
            },
            name: CorrosionNamespaceName::try_new("production").expect("namespace name"),
        };
        let roster = machine_ids
            .iter()
            .enumerate()
            .map(|(index, id)| DeployRosterMachine {
                id: id.clone(),
                name: MachineName::try_new(format!("node-{}", index + 1)).expect("name"),
                lifecycle: MachineLifecycle::Active,
                status: Some(DeployMachineStatus {
                    free_disk_bytes: 10 * 1024 * 1024 * 1024,
                    load: MachineLoadBand::Idle,
                }),
            })
            .collect();
        let reality = DeployReality {
            namespace_id: namespace_id.clone(),
            namespace,
            services: Vec::new(),
            automatic_route: None,
            routes_without_service: false,
            roster,
        };
        let store = Arc::new(FakeStore {
            controller: Mutex::new(ControllerDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id,
                preferred_machine_id: machine_id.clone(),
                appointment_id: appointment_id.clone(),
            }),
            operation: Mutex::new(None),
            reality: Mutex::new(reality),
            commits: Mutex::new(Vec::new()),
        });
        let hosts = Arc::new(FakeHosts {
            calls: Mutex::new(Vec::new()),
            stale_prepare: AtomicBool::new(false),
            next_container: AtomicUsize::new(1),
        });
        let request = DeployRequest {
            namespace_name: CorrosionNamespaceName::try_new("production").expect("namespace"),
            service_name: CorrosionServiceName::try_new("api").expect("service"),
            image: ImageReference::try_new("nginx:1.27-alpine").expect("image"),
            credential: None,
            runtime: ContainerRuntimeSpec::image_defaults(),
            health_gate: HealthGatePolicy::Enforce,
            placement: None,
            machines: None,
        };
        let command = DeployCommand {
            operation_id,
            request,
            initiator,
            appointment_id,
        };
        let executor = SimpleDeploy::new(machine_id, store.clone(), hosts.clone());
        Fixture {
            executor,
            store,
            hosts,
            command,
            machines: machine_ids,
        }
    }

    #[tokio::test]
    async fn singleton_deploy_flips_one_stable_replica_and_finishes() {
        let fixture = fixture(1);
        let namespace_id = fixture.store.reality.lock().await.namespace_id.clone();
        let [machine_id] = fixture.machines.as_slice() else {
            panic!("singleton fixture must have one machine")
        };
        let machine_id = machine_id.clone();

        let outcome = fixture
            .executor
            .run(&fixture.command)
            .await
            .expect("deploy");

        assert!(outcome.is_success());
        let commits = fixture.store.commits.lock().await;
        let [commit] = commits.as_slice() else {
            panic!("successful deploy must publish one commit")
        };
        assert_eq!(commit.service_id.as_str(), namespace_id.as_str());
        let [container] = commit.containers.as_slice() else {
            panic!("singleton deploy must publish one container")
        };
        assert!(matches!(
            container.document.replica_slot,
            ReplicaSlot::Replicated { number } if number.get() == 1
        ));
        assert_eq!(
            fixture.hosts.calls.lock().await.as_slice(),
            &[
                HostCall::Inspect(machine_id.clone()),
                HostCall::Prepare(machine_id),
            ]
        );
    }

    #[tokio::test]
    async fn foreign_appointment_interrupts_before_any_host_effect() {
        let fixture = fixture(1);
        fixture.store.controller.lock().await.appointment_id =
            ControllerAppointmentId::try_new("01J00000000000000000000009").expect("appointment");

        let outcome = fixture
            .executor
            .run(&fixture.command)
            .await
            .expect("deploy");

        assert!(!outcome.is_success());
        assert!(fixture.hosts.calls.lock().await.is_empty());
        let operation = fixture
            .store
            .operation
            .lock()
            .await
            .clone()
            .expect("operation");
        assert!(matches!(
            operation.deploy_state(),
            CorrosionDeployState::Terminal {
                outcome: CorrosionDeployOutcome::Interrupted,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn target_observing_a_foreign_appointment_interrupts_the_deploy() {
        let fixture = fixture(1);
        fixture.hosts.stale_prepare.store(true, Ordering::SeqCst);

        let outcome = fixture
            .executor
            .run(&fixture.command)
            .await
            .expect("deploy");

        assert!(matches!(outcome, CorrosionDeployOutcome::Interrupted));
    }
}
