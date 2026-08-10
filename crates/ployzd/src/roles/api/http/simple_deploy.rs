//! One whole deploy run for the preferred controller.
//!
//! Every invocation observes Corrosion and target hosts, derives one complete
//! placement, performs coarse idempotent host effects, and publishes one row
//! flip. There is deliberately no phase journal or recovery state machine.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::future::join_all;
use ployz_core::corrosion::{
    ContainerDocument, ControllerDocument, ControllerRevision, CorrosionDeployFailure,
    CorrosionDeployOutcome, CorrosionDeployWarning, CorrosionDocumentVersion, CorrosionTimestamp,
    HostPortBindings, MachineLoadBand, NamespaceDocument, OperationDocument, OperationInitiator,
    OperatorWriteProvenance, RouteBindingDocument, ServiceDocument, ServicePlacement,
    ServiceReplicaCount, V2ManagedContainerIdentity, fingerprint_env_value, managed_container_key,
    owns_current_controller_appointment,
};
use ployz_core::deploy::{ReplicaSlot, ReplicatedReplicaSlot};
use ployz_core::ids::{ContainerId, CorrosionNamespaceName, DeployName, RouteHostname};
use ployz_core::machine::{MachineLifecycle, MachineName};
use ployz_core::placement::{
    PlacementBid, PlacementPickInputs, PlacementRefusal, ServiceContainerObservation,
    pick_placement,
};
use ployz_core::{
    DeployDesiredReplica, DeployInspectOutcome, DeployInspectRequest, DeployObservedContainer,
    DeployPrepareOutcome, DeployPrepareRequest, DeployPreparedReplica, DeployRefusal,
    DeployRequest, DeployRetireOutcome, DeployRetireRequest, DeployServiceRequest,
    HealthGatePolicy, RequestedPins, RequestedPlacement,
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
        machine_id: &MachineName,
        request: DeployInspectRequest,
    ) -> Result<DeployInspectOutcome, DeployHostError>;

    async fn prepare(
        &self,
        machine_id: &MachineName,
        request: DeployPrepareRequest,
    ) -> Result<DeployPrepareOutcome, DeployHostError>;

    async fn retire(
        &self,
        machine_id: &MachineName,
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

    async fn observe(&self, command: &DeployCommand) -> Result<DeployReality, DeployStartError>;

    /// Inserts the one-shot named deploy attempt. `false` means the name is already used.
    async fn create_operation(&self, document: &OperationDocument) -> Result<bool, String>;

    async fn write_operation(&self, document: &OperationDocument) -> Result<(), String>;

    /// Atomically replaces the service's serving projection.
    async fn commit(&self, commit: DeployCommit) -> Result<(), String>;
}

/// A controller's complete view needed to decide one deploy.
#[derive(Debug, Clone)]
pub(super) struct DeployReality {
    pub(super) namespace_id: CorrosionNamespaceName,
    pub(super) namespace: NamespaceDocument,
    pub(super) services: Vec<ObservedServiceRow>,
    /// Deterministic automatic routes that do not exist yet.
    pub(super) missing_automatic_routes: Vec<DesiredRouteRow>,
    pub(super) roster: Vec<DeployRosterMachine>,
}

#[derive(Debug, Clone)]
pub(super) struct ObservedServiceRow {
    pub(super) document: ServiceDocument,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DesiredRouteRow {
    pub(super) id: RouteHostname,
    pub(super) document: RouteBindingDocument,
}

#[derive(Debug, Clone)]
pub(super) struct DeployRosterMachine {
    pub(super) id: MachineName,
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
    pub(super) operation_id: DeployName,
    pub(super) request: DeployRequest,
    pub(super) initiator: OperationInitiator,
    pub(super) appointment_id: ControllerRevision,
}

#[derive(Debug)]
pub(super) enum DeployStartError {
    Refused(DeployRefusal),
    Unavailable(String),
}

impl From<String> for DeployStartError {
    fn from(error: String) -> Self {
        Self::Unavailable(error)
    }
}

pub(super) struct StartedDeploy {
    command: DeployCommand,
    context: DeployContext,
    created: OperationDocument,
}

#[derive(Debug, Clone)]
pub(super) struct DeployCommit {
    pub(super) namespace_id: CorrosionNamespaceName,
    pub(super) services: Vec<DesiredServiceRow>,
    pub(super) containers: Vec<DesiredContainerRow>,
    pub(super) missing_automatic_routes: Vec<DesiredRouteRow>,
}

#[derive(Debug, Clone)]
pub(super) struct DesiredServiceRow {
    pub(super) key: String,
    pub(super) document: ServiceDocument,
}

#[derive(Debug, Clone)]
pub(super) struct DesiredContainerRow {
    pub(super) id: String,
    pub(super) document: ContainerDocument,
}

/// One concrete preferred-controller deploy. There are no knobs for alternate
/// orchestration strategies: the one implementation is the product policy.
/// Independent target-machine calls fan out together. Each machine still
/// serializes its own mutations through its one node workflow worker.
pub(super) struct SimpleDeploy {
    machine_id: MachineName,
    store: Arc<dyn SimpleDeployStore>,
    hosts: Arc<dyn DeployHosts>,
}

impl SimpleDeploy {
    #[must_use]
    pub(super) fn new(
        machine_id: MachineName,
        store: Arc<dyn SimpleDeployStore>,
        hosts: Arc<dyn DeployHosts>,
    ) -> Self {
        Self {
            machine_id,
            store,
            hosts,
        }
    }

    pub(super) async fn start(
        &self,
        command: DeployCommand,
    ) -> Result<StartedDeploy, DeployStartError> {
        let reality = self.store.observe(&command).await?;
        let context = classify_services(&command.request, reality)?;
        let created_at = now()?;
        let created = OperationDocument::deploy_created(
            CorrosionDocumentVersion::V1,
            context.reality.namespace.cluster_id.clone(),
            self.machine_id.clone(),
            command.initiator.clone(),
            context.reality.namespace_id.clone(),
            command.operation_id.clone(),
            created_at,
        );
        if !self.store.create_operation(&created).await? {
            return Err(DeployStartError::Refused(
                DeployRefusal::DeployNameAlreadyUsed {
                    namespace_name: command.request.namespace_name,
                    deploy_name: command.operation_id,
                },
            ));
        }
        Ok(StartedDeploy {
            command,
            context,
            created,
        })
    }

    pub(super) async fn run(
        &self,
        started: StartedDeploy,
    ) -> Result<CorrosionDeployOutcome, String> {
        let StartedDeploy {
            command,
            context,
            created,
        } = started;
        let command = &command;

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
        let mut prepared_cutovers = Vec::new();
        let mut prepared_services = Vec::with_capacity(command.request.services.len());
        for (service_name, service) in command.request.services.iter() {
            if !service.runtime.volume_mounts.is_empty()
                && let Some(machine_id) = context.reality.roster.iter().find_map(|machine| {
                    inspections.get(&machine.id).and_then(|inspection| {
                        inspection
                            .containers
                            .iter()
                            .any(|container| {
                                container.identity.namespace_id == context.reality.namespace_id
                                    && container.identity.service_name == *service_name
                                    && container.identity.operation_id != command.operation_id
                            })
                            .then(|| machine.id.clone())
                    })
                })
            {
                return self
                    .fail_before_commit(
                        command,
                        &created,
                        &context,
                        &prepared_cutovers,
                        CorrosionDeployFailure::PrepareRefused { machine_id },
                    )
                    .await;
            }

            let placement = match derive_placement(service_name, service, &context, &inspections) {
                Ok(placement) => placement,
                Err(refusal) => {
                    return self
                        .fail_before_commit(
                            command,
                            &created,
                            &context,
                            &prepared_cutovers,
                            CorrosionDeployFailure::Placement { refusal },
                        )
                        .await;
                }
            };
            let desired = match desired_replicas(
                &command.operation_id,
                &context.reality.namespace_id,
                service_name,
                &placement,
            ) {
                Ok(desired) => desired,
                Err(error) => {
                    self.rollback_prepared(command, &context, &prepared_cutovers)
                        .await;
                    return Err(error);
                }
            };
            let mut prepared = Vec::new();
            let mut resolved_image = None;
            match self.appointment_is_current(command).await {
                Ok(true) => {}
                Ok(false) => {
                    return self
                        .interrupt_before_commit(command, &created, &context, &prepared_cutovers)
                        .await;
                }
                Err(error) => {
                    self.rollback_prepared(command, &context, &prepared_cutovers)
                        .await;
                    return Err(error);
                }
            }
            let appointment_id = command.appointment_id;
            let operation_id = command.operation_id.clone();
            let namespace_name = context.reality.namespace.name.clone();
            let service_name = service_name.clone();
            let image = service.image.clone();
            let credential = service.credential.clone();
            let runtime = service.runtime.clone();
            let health_gate = service.health_gate;
            let prepare_calls =
                group_by_machine(&desired)
                    .into_iter()
                    .map(|(machine_id, replicas)| {
                        let operation_id = operation_id.clone();
                        let namespace_name = namespace_name.clone();
                        let service_name = service_name.clone();
                        let image = image.clone();
                        let credential = credential.clone();
                        let runtime = runtime.clone();
                        let stop_before_start = inspections
                            .get(&machine_id)
                            .map(|inspection| {
                                conflicting_incumbents(&replicas, &inspection.containers)
                            })
                            .unwrap_or_default();
                        async move {
                            let request = DeployPrepareRequest {
                                appointment_id,
                                operation_id,
                                namespace_name,
                                service_name,
                                image,
                                credential,
                                runtime,
                                health_gate,
                                stop_before_start,
                                replicas,
                            };
                            let outcome = self.hosts.prepare(&machine_id, request).await;
                            (machine_id, outcome)
                        }
                    });
            let prepare_outcomes = join_all(prepare_calls).await;
            let mut stale_controller = false;
            let mut prepare_failure = None;
            for (machine_id, outcome) in prepare_outcomes {
                let (image, replicas, displaced_incumbents) = match outcome {
                    Ok(DeployPrepareOutcome::Prepared {
                        image,
                        replicas,
                        displaced_incumbents,
                    }) => (image, replicas, displaced_incumbents),
                    Ok(DeployPrepareOutcome::Refused { .. }) => {
                        prepare_failure
                            .get_or_insert(CorrosionDeployFailure::PrepareRefused { machine_id });
                        continue;
                    }
                    Err(DeployHostError::StaleController) => {
                        stale_controller = true;
                        continue;
                    }
                    Ok(DeployPrepareOutcome::Failed { .. }) | Err(DeployHostError::Failed) => {
                        prepare_failure
                            .get_or_insert(CorrosionDeployFailure::PrepareFailed { machine_id });
                        continue;
                    }
                };
                prepared_cutovers.push(PreparedCutover {
                    machine_id: machine_id.clone(),
                    candidates: replicas
                        .iter()
                        .map(|replica| DeployObservedContainer {
                            container_id: replica.container_id.clone(),
                            identity: replica.identity.clone(),
                            running: true,
                            host_ports: replicas_for(&desired, &machine_id)
                                .into_iter()
                                .find(|desired| desired.identity == replica.identity)
                                .map(|desired| desired.host_ports)
                                .unwrap_or_default(),
                        })
                        .collect(),
                    displaced_incumbents,
                });
                if !prepared_matches(&replicas, &replicas_for(&desired, &machine_id)) {
                    prepare_failure.get_or_insert(
                        CorrosionDeployFailure::PreparedReplicaMismatch { machine_id },
                    );
                    continue;
                }
                if resolved_image
                    .as_ref()
                    .is_some_and(|resolved| resolved != &image)
                {
                    prepare_failure.get_or_insert(CorrosionDeployFailure::ResolvedImageMismatch);
                    continue;
                }
                resolved_image = Some(image);
                prepared.extend(replicas.into_iter().map(|replica| PlacedReplica {
                    machine_id: machine_id.clone(),
                    replica,
                }));
            }
            if stale_controller {
                return self
                    .interrupt_before_commit(command, &created, &context, &prepared_cutovers)
                    .await;
            }
            if let Some(failure) = prepare_failure {
                return self
                    .fail_before_commit(command, &created, &context, &prepared_cutovers, failure)
                    .await;
            }
            let Some(resolved_image) = resolved_image else {
                return self
                    .fail_before_commit(
                        command,
                        &created,
                        &context,
                        &prepared_cutovers,
                        CorrosionDeployFailure::RuntimeRealityUnavailable,
                    )
                    .await;
            };
            prepared_services.push(PreparedService {
                service_name,
                request: service.clone(),
                placement,
                resolved_image,
                replicas: prepared,
            });
        }
        match self.appointment_is_current(command).await {
            Ok(true) => {}
            Ok(false) => {
                return self
                    .interrupt_before_commit(command, &created, &context, &prepared_cutovers)
                    .await;
            }
            Err(error) => {
                self.rollback_prepared(command, &context, &prepared_cutovers)
                    .await;
                return Err(error);
            }
        }

        let written_at = match now() {
            Ok(written_at) => written_at,
            Err(error) => {
                self.rollback_prepared(command, &context, &prepared_cutovers)
                    .await;
                return Err(error);
            }
        };
        let commit = match build_commit(command, &context, &prepared_services, written_at) {
            Ok(commit) => commit,
            Err(error) => {
                self.rollback_prepared(command, &context, &prepared_cutovers)
                    .await;
                return Err(error);
            }
        };
        if self.store.commit(commit).await.is_err() {
            // A lost write reply cannot distinguish rejection from a committed
            // local transaction. End honestly and let a new command re-plan
            // from the service/container rows that actually exist.
            return self.interrupt(command, &created).await;
        }

        let keep = prepared_services
            .iter()
            .flat_map(|service| &service.replicas)
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
        appointment_id: &ControllerRevision,
    ) -> Option<BTreeMap<MachineName, HostInspection>> {
        let calls = reality.roster.iter().map(|machine| async move {
            let request = DeployInspectRequest {
                appointment_id: *appointment_id,
            };
            let outcome = self.hosts.inspect(&machine.id, request).await;
            (machine, outcome)
        });
        let mut inspections = BTreeMap::new();
        for (machine, outcome) in join_all(calls).await {
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

    async fn rollback_prepared(
        &self,
        command: &DeployCommand,
        context: &DeployContext,
        prepared: &[PreparedCutover],
    ) {
        let mut by_machine = BTreeMap::<
            MachineName,
            (Vec<DeployObservedContainer>, Vec<DeployObservedContainer>),
        >::new();
        for cutover in prepared {
            let (candidates, incumbents) =
                by_machine.entry(cutover.machine_id.clone()).or_default();
            extend_unique_containers(candidates, &cutover.candidates);
            extend_unique_containers(incumbents, &cutover.displaced_incumbents);
        }
        let calls = by_machine.into_iter().map(
            |(machine_id, (containers, restart_after_retire))| async move {
                let outcome = self
                    .hosts
                    .retire(
                        &machine_id,
                        DeployRetireRequest {
                            appointment_id: command.appointment_id,
                            operation_id: command.operation_id.clone(),
                            namespace_name: context.reality.namespace.name.clone(),
                            containers,
                            restart_after_retire,
                        },
                    )
                    .await;
                (machine_id, outcome)
            },
        );
        for (machine_id, outcome) in join_all(calls).await {
            if !matches!(outcome, Ok(DeployRetireOutcome::Retired)) {
                tracing::warn!(%machine_id, "pre-commit deploy rollback did not complete");
            }
        }
    }

    async fn fail_before_commit(
        &self,
        command: &DeployCommand,
        created: &OperationDocument,
        context: &DeployContext,
        prepared: &[PreparedCutover],
        failure: CorrosionDeployFailure,
    ) -> Result<CorrosionDeployOutcome, String> {
        self.rollback_prepared(command, context, prepared).await;
        self.fail(command, created, failure).await
    }

    async fn interrupt_before_commit(
        &self,
        command: &DeployCommand,
        created: &OperationDocument,
        context: &DeployContext,
        prepared: &[PreparedCutover],
    ) -> Result<CorrosionDeployOutcome, String> {
        self.rollback_prepared(command, context, prepared).await;
        self.interrupt(command, created).await
    }

    async fn finish_after_flip(
        &self,
        command: &DeployCommand,
        created: &OperationDocument,
        context: &DeployContext,
        inspections: &BTreeMap<MachineName, HostInspection>,
        keep: &BTreeSet<(MachineName, ContainerId)>,
    ) -> Result<CorrosionDeployOutcome, String> {
        let retire_calls = inspections.iter().filter_map(|(machine_id, inspection)| {
            let containers = inspection
                .containers
                .iter()
                .filter(|container| {
                    container.identity.namespace_id == context.reality.namespace_id
                        && !keep.contains(&(machine_id.clone(), container.container_id.clone()))
                })
                .cloned()
                .collect::<Vec<_>>();
            (!containers.is_empty()).then_some(async move {
                let outcome = self
                    .hosts
                    .retire(
                        machine_id,
                        DeployRetireRequest {
                            appointment_id: command.appointment_id,
                            operation_id: command.operation_id.clone(),
                            namespace_name: context.reality.namespace.name.clone(),
                            containers,
                            restart_after_retire: Vec::new(),
                        },
                    )
                    .await;
                (machine_id, outcome)
            })
        });
        let mut cleanup_failed = Vec::new();
        for (machine_id, outcome) in join_all(retire_calls).await {
            if !matches!(outcome, Ok(DeployRetireOutcome::Retired)) {
                cleanup_failed.push(machine_id.clone());
            }
        }
        let mut warnings = Vec::new();
        if command
            .request
            .services
            .values()
            .any(|service| service.health_gate == HealthGatePolicy::Skip)
        {
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
        _command: &DeployCommand,
        created: &OperationDocument,
        outcome: CorrosionDeployOutcome,
    ) -> Result<CorrosionDeployOutcome, String> {
        let completed_at = now()?;
        let operation = created.clone().into_terminal(completed_at, outcome.clone());
        self.store.write_operation(&operation).await?;
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
    incumbents: BTreeMap<ployz_core::corrosion::CorrosionServiceName, ObservedServiceRow>,
    admission_failure: Option<CorrosionDeployFailure>,
}

fn classify_services(
    request: &DeployRequest,
    reality: DeployReality,
) -> Result<DeployContext, DeployStartError> {
    let incumbents = reality
        .services
        .iter()
        .cloned()
        .map(|service| (service.document.name.clone(), service))
        .collect::<BTreeMap<_, _>>();
    let admission_failure =
        request
            .services
            .iter()
            .fold(None, |failure, (service_name, service)| {
                failure
                    .or_else(|| replicas_on_global(service, incumbents.get(service_name)))
                    .or_else(|| unknown_pin(service, &reality))
            });
    let context = DeployContext {
        reality,
        incumbents,
        admission_failure,
    };
    validate_effective_host_ports(request, &context)?;
    Ok(context)
}

fn validate_effective_host_ports(
    request: &DeployRequest,
    context: &DeployContext,
) -> Result<(), DeployStartError> {
    let mut claimed = BTreeMap::new();
    for (service_name, service) in request.services.iter() {
        let ServicePlacement::Global { host_ports } =
            effective_placement(service_name, service, context)
        else {
            continue;
        };
        for binding in host_ports.iter() {
            let claim = (binding.protocol, binding.host_port);
            if let Some(first_service) = claimed.insert(claim, service_name.clone()) {
                return Err(DeployStartError::Refused(DeployRefusal::HostPortConflict {
                    host_port: binding.host_port.get(),
                    protocol: binding.protocol,
                    first_service,
                    second_service: service_name.clone(),
                }));
            }
        }
    }
    Ok(())
}

fn replicas_on_global(
    request: &DeployServiceRequest,
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

fn unknown_pin(
    request: &DeployServiceRequest,
    reality: &DeployReality,
) -> Option<CorrosionDeployFailure> {
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
    pinned_machines: BTreeSet<MachineName>,
    targets: Vec<MachineName>,
}

fn derive_placement(
    service_name: &ployz_core::corrosion::CorrosionServiceName,
    request: &DeployServiceRequest,
    context: &DeployContext,
    inspections: &BTreeMap<MachineName, HostInspection>,
) -> Result<EffectivePlacement, PlacementRefusal> {
    let placement = effective_placement(service_name, request, context);
    let pinned_machines = resolve_pins(service_name, request, context);
    let active_deploy = context
        .incumbents
        .get(service_name)
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
            service_containers: observed_service_containers(
                &inspection.containers,
                &context.reality.namespace_id,
                service_name,
            ),
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

fn observed_service_containers(
    containers: &[DeployObservedContainer],
    namespace_id: &CorrosionNamespaceName,
    service_name: &ployz_core::corrosion::CorrosionServiceName,
) -> Vec<ServiceContainerObservation> {
    containers
        .iter()
        .filter(|container| {
            &container.identity.namespace_id == namespace_id
                && &container.identity.service_name == service_name
        })
        .map(|container| ServiceContainerObservation {
            deploy: container.identity.operation_id.clone(),
        })
        .collect()
}

fn effective_placement(
    service_name: &ployz_core::corrosion::CorrosionServiceName,
    request: &DeployServiceRequest,
    context: &DeployContext,
) -> ServicePlacement {
    let inherited = context
        .incumbents
        .get(service_name)
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

fn resolve_pins(
    service_name: &ployz_core::corrosion::CorrosionServiceName,
    request: &DeployServiceRequest,
    context: &DeployContext,
) -> BTreeSet<MachineName> {
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
            .incumbents
            .get(service_name)
            .as_ref()
            .map(|service| service.document.pinned_machines.clone())
            .unwrap_or_default(),
    }
}

#[derive(Clone)]
struct DesiredReplica {
    machine_id: MachineName,
    desired: DeployDesiredReplica,
}

fn desired_replicas(
    operation_id: &DeployName,
    namespace_id: &CorrosionNamespaceName,
    service_name: &ployz_core::corrosion::CorrosionServiceName,
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
                        service_name: service_name.clone(),
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
) -> BTreeMap<MachineName, Vec<DeployDesiredReplica>> {
    let mut grouped = BTreeMap::new();
    for replica in desired {
        grouped
            .entry(replica.machine_id.clone())
            .or_insert_with(Vec::new)
            .push(replica.desired.clone());
    }
    grouped
}

fn replicas_for(desired: &[DesiredReplica], machine_id: &MachineName) -> Vec<DeployDesiredReplica> {
    desired
        .iter()
        .filter(|replica| &replica.machine_id == machine_id)
        .map(|replica| replica.desired.clone())
        .collect()
}

fn conflicting_incumbents(
    desired: &[DeployDesiredReplica],
    observed: &[DeployObservedContainer],
) -> Vec<DeployObservedContainer> {
    let Some(namespace_id) = desired
        .first()
        .map(|replica| &replica.identity.namespace_id)
    else {
        return Vec::new();
    };
    observed
        .iter()
        .filter(|container| {
            &container.identity.namespace_id == namespace_id
                && !desired
                    .iter()
                    .any(|replica| replica.identity == container.identity)
                && desired
                    .iter()
                    .any(|replica| host_ports_overlap(&replica.host_ports, &container.host_ports))
        })
        .cloned()
        .collect()
}

fn host_ports_overlap(left: &HostPortBindings, right: &HostPortBindings) -> bool {
    left.iter().any(|left| {
        right
            .iter()
            .any(|right| left.protocol == right.protocol && left.host_port == right.host_port)
    })
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
    machine_id: MachineName,
    replica: DeployPreparedReplica,
}

struct PreparedService {
    service_name: ployz_core::corrosion::CorrosionServiceName,
    request: DeployServiceRequest,
    placement: EffectivePlacement,
    resolved_image: ployz_core::deploy::ImageReference,
    replicas: Vec<PlacedReplica>,
}

struct PreparedCutover {
    machine_id: MachineName,
    candidates: Vec<DeployObservedContainer>,
    displaced_incumbents: Vec<DeployObservedContainer>,
}

fn extend_unique_containers(
    target: &mut Vec<DeployObservedContainer>,
    additions: &[DeployObservedContainer],
) {
    for container in additions {
        if !target
            .iter()
            .any(|existing| existing.container_id == container.container_id)
        {
            target.push(container.clone());
        }
    }
}

fn build_commit(
    command: &DeployCommand,
    context: &DeployContext,
    prepared: &[PreparedService],
    written_at: CorrosionTimestamp,
) -> Result<DeployCommit, String> {
    let mut services = Vec::with_capacity(prepared.len());
    let mut containers = Vec::new();
    for prepared_service in prepared {
        let service_name = &prepared_service.service_name;
        let request = &prepared_service.request;
        let env_fingerprints = request
            .runtime
            .environment
            .iter()
            .map(|(name, value)| {
                fingerprint_env_value(value)
                    .map(|fingerprint| (name.as_str().to_owned(), fingerprint))
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<_, _>>()?;
        services.push(DesiredServiceRow {
            key: ployz_core::corrosion::service_key(&context.reality.namespace_id, service_name),
            document: ServiceDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id: context.reality.namespace.cluster_id.clone(),
                provenance: OperatorWriteProvenance {
                    written_by: command.initiator.clone(),
                    written_at,
                },
                namespace_id: context.reality.namespace_id.clone(),
                name: service_name.clone(),
                image: prepared_service.resolved_image.clone(),
                env_fingerprints,
                placement: prepared_service.placement.placement.clone(),
                pinned_machines: prepared_service.placement.pinned_machines.clone(),
                active_deploy: command.operation_id.clone(),
                previous_image: context
                    .incumbents
                    .get(service_name)
                    .map(|incumbent| incumbent.document.image.clone()),
                deployed_at: written_at,
            },
        });
        containers.extend(
            prepared_service
                .replicas
                .iter()
                .map(|placed| DesiredContainerRow {
                    id: managed_container_key(&placed.replica.identity, &placed.machine_id),
                    document: ContainerDocument {
                        v: CorrosionDocumentVersion::V1,
                        cluster_id: context.reality.namespace.cluster_id.clone(),
                        runtime_id: placed.replica.container_id.clone(),
                        machine_id: placed.machine_id.clone(),
                        namespace_id: context.reality.namespace_id.clone(),
                        service_name: service_name.clone(),
                        replica_slot: placed.replica.identity.replica_slot,
                        ip: placed.replica.ip,
                        deploy: command.operation_id.clone(),
                    },
                }),
        );
    }
    Ok(DeployCommit {
        namespace_id: context.reality.namespace_id.clone(),
        services,
        containers,
        missing_automatic_routes: context.reality.missing_automatic_routes.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::num::NonZeroU16;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use ployz_core::DeployServices;
    use ployz_core::corrosion::{
        CorrosionDeployState, CorrosionNamespaceName, CorrosionServiceName, Principal,
    };
    use ployz_core::deploy::{ContainerRuntimeSpec, ImageReference};
    use ployz_core::ids::{ClusterName, PeerName};
    use ployz_core::image::OciDigest;
    use tokio::sync::{Barrier, Mutex};

    use super::*;

    struct FakeStore {
        controller: Mutex<ControllerDocument>,
        create_operation: AtomicBool,
        operation: Mutex<Option<OperationDocument>>,
        reality: Mutex<DeployReality>,
        commits: Mutex<Vec<DeployCommit>>,
    }

    #[async_trait]
    impl SimpleDeployStore for FakeStore {
        async fn controller(&self) -> Result<ControllerDocument, String> {
            Ok(self.controller.lock().await.clone())
        }

        async fn observe(
            &self,
            _command: &DeployCommand,
        ) -> Result<DeployReality, DeployStartError> {
            Ok(self.reality.lock().await.clone())
        }

        async fn create_operation(&self, document: &OperationDocument) -> Result<bool, String> {
            if !self.create_operation.load(Ordering::SeqCst) {
                return Ok(false);
            }
            *self.operation.lock().await = Some(document.clone());
            Ok(true)
        }

        async fn write_operation(&self, document: &OperationDocument) -> Result<(), String> {
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
        Inspect(MachineName),
        Prepare(MachineName),
        Retire(MachineName),
    }

    struct FakeHosts {
        calls: Mutex<Vec<HostCall>>,
        inspections: Mutex<Vec<DeployObservedContainer>>,
        prepare_requests: Mutex<Vec<DeployPrepareRequest>>,
        retire_requests: Mutex<Vec<DeployRetireRequest>>,
        fail_prepare_service: Option<CorrosionServiceName>,
        stale_prepare: AtomicBool,
        next_container: AtomicUsize,
        fanout_barrier: Option<Arc<Barrier>>,
    }

    #[async_trait]
    impl DeployHosts for FakeHosts {
        async fn inspect(
            &self,
            machine_id: &MachineName,
            _request: DeployInspectRequest,
        ) -> Result<DeployInspectOutcome, DeployHostError> {
            self.calls
                .lock()
                .await
                .push(HostCall::Inspect(machine_id.clone()));
            if let Some(barrier) = &self.fanout_barrier {
                barrier.wait().await;
            }
            Ok(DeployInspectOutcome::Inspected {
                bridge_ready: true,
                containers: self.inspections.lock().await.clone(),
            })
        }

        async fn prepare(
            &self,
            machine_id: &MachineName,
            request: DeployPrepareRequest,
        ) -> Result<DeployPrepareOutcome, DeployHostError> {
            self.calls
                .lock()
                .await
                .push(HostCall::Prepare(machine_id.clone()));
            self.prepare_requests.lock().await.push(request.clone());
            if let Some(barrier) = &self.fanout_barrier {
                barrier.wait().await;
            }
            if self.stale_prepare.load(Ordering::SeqCst) {
                return Err(DeployHostError::StaleController);
            }
            if self
                .fail_prepare_service
                .as_ref()
                .is_some_and(|service| service == &request.service_name)
            {
                return Ok(DeployPrepareOutcome::Failed {});
            }
            let digest = OciDigest::try_new(format!("sha256:{}", "c".repeat(64)))
                .map_err(|_| DeployHostError::Failed)?;
            let image = request
                .image
                .with_digest(&digest)
                .map_err(|_| DeployHostError::Failed)?;
            let displaced_incumbents = request.stop_before_start.clone();
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
            Ok(DeployPrepareOutcome::Prepared {
                image,
                replicas,
                displaced_incumbents,
            })
        }

        async fn retire(
            &self,
            machine_id: &MachineName,
            request: DeployRetireRequest,
        ) -> Result<DeployRetireOutcome, DeployHostError> {
            self.calls
                .lock()
                .await
                .push(HostCall::Retire(machine_id.clone()));
            self.retire_requests.lock().await.push(request);
            Ok(DeployRetireOutcome::Retired)
        }
    }

    struct Fixture {
        executor: SimpleDeploy,
        store: Arc<FakeStore>,
        hosts: Arc<FakeHosts>,
        command: DeployCommand,
        machines: Vec<MachineName>,
    }

    fn fixture(roster_members: usize) -> Fixture {
        let cluster_id = ClusterName::try_new("main").expect("cluster");
        let machine_ids = ["node-1", "node-2", "node-3"]
            .into_iter()
            .take(roster_members)
            .map(|id| MachineName::try_new(id).expect("machine"))
            .collect::<Vec<_>>();
        let machine_id = machine_ids.first().cloned().expect("roster member");
        let appointment_id = ControllerRevision::INITIAL;
        let namespace_id = CorrosionNamespaceName::try_new("production").expect("namespace");
        let operation_id = DeployName::try_new("release-1").expect("operation");
        let initiator = Principal::Peer {
            peer_id: PeerName::try_new("operator").expect("peer"),
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
            missing_automatic_routes: Vec::new(),
            roster,
        };
        let store = Arc::new(FakeStore {
            controller: Mutex::new(ControllerDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id,
                preferred_machine_id: machine_id.clone(),
                appointment_id,
                heartbeat_at: at,
            }),
            create_operation: AtomicBool::new(true),
            operation: Mutex::new(None),
            reality: Mutex::new(reality),
            commits: Mutex::new(Vec::new()),
        });
        let hosts = Arc::new(FakeHosts {
            calls: Mutex::new(Vec::new()),
            inspections: Mutex::new(Vec::new()),
            prepare_requests: Mutex::new(Vec::new()),
            retire_requests: Mutex::new(Vec::new()),
            fail_prepare_service: None,
            stale_prepare: AtomicBool::new(false),
            next_container: AtomicUsize::new(1),
            fanout_barrier: None,
        });
        let request = DeployRequest {
            namespace_name: CorrosionNamespaceName::try_new("production").expect("namespace"),
            deploy_name: operation_id.clone(),
            services: DeployServices::try_new([(
                CorrosionServiceName::try_new("api").expect("service"),
                DeployServiceRequest {
                    image: ImageReference::try_new("nginx:1.27-alpine").expect("image"),
                    credential: None,
                    runtime: ContainerRuntimeSpec::image_defaults(),
                    health_gate: HealthGatePolicy::Enforce,
                    placement: None,
                    machines: None,
                },
            )])
            .expect("unique services"),
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
    async fn a_used_deploy_name_is_refused_before_host_effects() {
        let fixture = fixture(1);
        fixture
            .store
            .create_operation
            .store(false, Ordering::SeqCst);

        let Err(error) = fixture.executor.start(fixture.command.clone()).await else {
            panic!("used deploy name must be refused");
        };

        assert!(matches!(
            error,
            DeployStartError::Refused(DeployRefusal::DeployNameAlreadyUsed {
                namespace_name,
                deploy_name,
            }) if namespace_name.as_str() == "production" && deploy_name.as_str() == "release-1"
        ));
        assert!(fixture.hosts.calls.lock().await.is_empty());
    }

    #[tokio::test]
    async fn singleton_deploy_flips_one_stable_replica_and_finishes() {
        let fixture = fixture(1);
        let namespace_id = fixture.store.reality.lock().await.namespace_id.clone();
        let [machine_id] = fixture.machines.as_slice() else {
            panic!("singleton fixture must have one machine")
        };
        let machine_id = machine_id.clone();

        let started = fixture
            .executor
            .start(fixture.command.clone())
            .await
            .expect("start deploy");
        assert!(matches!(
            fixture
                .store
                .operation
                .lock()
                .await
                .as_ref()
                .expect("created operation")
                .deploy_state(),
            CorrosionDeployState::Created
        ));
        assert!(fixture.hosts.calls.lock().await.is_empty());
        let outcome = fixture.executor.run(started).await.expect("deploy");

        assert!(outcome.is_success());
        let commits = fixture.store.commits.lock().await;
        let [commit] = commits.as_slice() else {
            panic!("successful deploy must publish one commit")
        };
        assert_eq!(commit.namespace_id, namespace_id);
        assert_eq!(commit.services.len(), 1);
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
    async fn namespace_snapshot_reconciles_multiple_named_services_together() {
        let mut fixture = fixture(1);
        assert!(
            fixture
                .command
                .request
                .services
                .insert(
                    CorrosionServiceName::try_new("worker").expect("service"),
                    DeployServiceRequest {
                        image: ImageReference::try_new("busybox:1.37").expect("image"),
                        credential: None,
                        runtime: ContainerRuntimeSpec::image_defaults(),
                        health_gate: HealthGatePolicy::Enforce,
                        placement: None,
                        machines: None,
                    },
                )
                .is_none()
        );

        let started = fixture
            .executor
            .start(fixture.command.clone())
            .await
            .expect("start deploy");
        let outcome = fixture.executor.run(started).await.expect("deploy");

        assert!(outcome.is_success());
        let commits = fixture.store.commits.lock().await;
        let [commit] = commits.as_slice() else {
            panic!("namespace deploy must publish one commit")
        };
        assert_eq!(commit.services.len(), 2);
        assert_eq!(commit.containers.len(), 2);
        assert_eq!(
            commit
                .services
                .iter()
                .map(|row| row.document.name.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["api", "worker"]),
        );
        assert!(commit.containers.iter().all(|row| {
            row.document.namespace_id == commit.namespace_id
                && row.document.deploy == fixture.command.operation_id
        }));
        assert_eq!(
            commit
                .containers
                .iter()
                .map(|row| row.document.service_name.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["api", "worker"]),
        );
    }

    #[tokio::test]
    async fn namespace_snapshot_refuses_cross_service_global_host_port_conflicts() {
        let mut fixture = fixture(1);
        let binding = ployz_core::corrosion::HostPortBinding {
            host_port: NonZeroU16::new(8080).expect("host port"),
            container_port: NonZeroU16::new(80).expect("container port"),
            protocol: ployz_core::corrosion::HostPortProtocol::Tcp,
        };
        let host_ports = HostPortBindings::try_new([binding]).expect("host ports");
        fixture
            .command
            .request
            .services
            .get_mut(&CorrosionServiceName::try_new("api").expect("service"))
            .expect("api")
            .placement = Some(RequestedPlacement::Global {
            host_ports: host_ports.clone(),
        });
        fixture.command.request.services.insert(
            CorrosionServiceName::try_new("worker").expect("service"),
            DeployServiceRequest {
                image: ImageReference::try_new("busybox:1.37").expect("image"),
                credential: None,
                runtime: ContainerRuntimeSpec::image_defaults(),
                health_gate: HealthGatePolicy::Enforce,
                placement: Some(RequestedPlacement::Global { host_ports }),
                machines: None,
            },
        );

        let Err(error) = fixture.executor.start(fixture.command).await else {
            panic!("conflicting services must be refused before execution");
        };
        assert!(matches!(
            error,
            DeployStartError::Refused(DeployRefusal::HostPortConflict {
                host_port: 8080,
                protocol: ployz_core::corrosion::HostPortProtocol::Tcp,
                ref first_service,
                ref second_service,
            }) if first_service.as_str() == "api" && second_service.as_str() == "worker"
        ));
        assert!(fixture.store.operation.lock().await.is_none());
        assert!(fixture.hosts.calls.lock().await.is_empty());
    }

    #[tokio::test]
    async fn global_host_port_replacement_stops_only_the_exact_conflicting_incumbent() {
        let mut fixture = fixture(1);
        let host_ports = HostPortBindings::try_new([ployz_core::corrosion::HostPortBinding {
            host_port: NonZeroU16::new(8080).expect("host port"),
            container_port: NonZeroU16::new(80).expect("container port"),
            protocol: ployz_core::corrosion::HostPortProtocol::Tcp,
        }])
        .expect("host ports");
        fixture
            .command
            .request
            .services
            .get_mut(&CorrosionServiceName::try_new("api").expect("service"))
            .expect("api")
            .placement = Some(RequestedPlacement::Global {
            host_ports: host_ports.clone(),
        });
        let incumbent = DeployObservedContainer {
            container_id: ContainerId::try_new("incumbent").expect("container"),
            identity: V2ManagedContainerIdentity {
                namespace_id: CorrosionNamespaceName::try_new("production").expect("namespace"),
                service_name: CorrosionServiceName::try_new("api").expect("service"),
                operation_id: DeployName::try_new("release-0").expect("deploy"),
                replica_slot: ReplicaSlot::Global,
            },
            running: true,
            host_ports,
        };
        fixture
            .hosts
            .inspections
            .lock()
            .await
            .push(incumbent.clone());

        let started = fixture
            .executor
            .start(fixture.command.clone())
            .await
            .expect("start deploy");
        let outcome = fixture.executor.run(started).await.expect("deploy");

        assert!(outcome.is_success());
        let requests = fixture.hosts.prepare_requests.lock().await;
        let [request] = requests.as_slice() else {
            panic!("one host preparation expected")
        };
        assert_eq!(request.stop_before_start, vec![incumbent]);
    }

    #[tokio::test]
    async fn later_service_failure_removes_candidates_and_restarts_displaced_incumbents() {
        let mut fixture = fixture(1);
        let host_ports = HostPortBindings::try_new([ployz_core::corrosion::HostPortBinding {
            host_port: NonZeroU16::new(8080).expect("host port"),
            container_port: NonZeroU16::new(80).expect("container port"),
            protocol: ployz_core::corrosion::HostPortProtocol::Tcp,
        }])
        .expect("host ports");
        fixture
            .command
            .request
            .services
            .get_mut(&CorrosionServiceName::try_new("api").expect("service"))
            .expect("api")
            .placement = Some(RequestedPlacement::Global {
            host_ports: host_ports.clone(),
        });
        fixture.command.request.services.insert(
            CorrosionServiceName::try_new("worker").expect("service"),
            DeployServiceRequest {
                image: ImageReference::try_new("busybox:1.37").expect("image"),
                credential: None,
                runtime: ContainerRuntimeSpec::image_defaults(),
                health_gate: HealthGatePolicy::Enforce,
                placement: None,
                machines: None,
            },
        );
        let incumbent = DeployObservedContainer {
            container_id: ContainerId::try_new("incumbent").expect("container"),
            identity: V2ManagedContainerIdentity {
                namespace_id: CorrosionNamespaceName::try_new("production").expect("namespace"),
                service_name: CorrosionServiceName::try_new("api").expect("service"),
                operation_id: DeployName::try_new("release-0").expect("deploy"),
                replica_slot: ReplicaSlot::Global,
            },
            running: true,
            host_ports,
        };
        fixture.hosts = Arc::new(FakeHosts {
            calls: Mutex::new(Vec::new()),
            inspections: Mutex::new(vec![incumbent.clone()]),
            prepare_requests: Mutex::new(Vec::new()),
            retire_requests: Mutex::new(Vec::new()),
            fail_prepare_service: Some(CorrosionServiceName::try_new("worker").expect("service")),
            stale_prepare: AtomicBool::new(false),
            next_container: AtomicUsize::new(1),
            fanout_barrier: None,
        });
        fixture.executor = SimpleDeploy::new(
            fixture
                .machines
                .first()
                .cloned()
                .expect("fixture has one machine"),
            fixture.store.clone(),
            fixture.hosts.clone(),
        );

        let started = fixture
            .executor
            .start(fixture.command.clone())
            .await
            .expect("start deploy");
        let outcome = fixture.executor.run(started).await.expect("deploy");

        assert!(matches!(
            outcome,
            CorrosionDeployOutcome::Failed {
                failure: CorrosionDeployFailure::PrepareFailed { .. }
            }
        ));
        let requests = fixture.hosts.retire_requests.lock().await;
        let [rollback] = requests.as_slice() else {
            panic!("one rollback expected")
        };
        assert_eq!(rollback.restart_after_retire, vec![incumbent]);
        let [candidate] = rollback.containers.as_slice() else {
            panic!("one candidate rollback expected")
        };
        assert_eq!(candidate.identity.service_name.as_str(), "api");
    }

    #[tokio::test]
    async fn target_host_inspection_and_prepare_fan_out_concurrently() {
        let mut fixture = fixture(2);
        fixture.hosts = Arc::new(FakeHosts {
            calls: Mutex::new(Vec::new()),
            inspections: Mutex::new(Vec::new()),
            prepare_requests: Mutex::new(Vec::new()),
            retire_requests: Mutex::new(Vec::new()),
            fail_prepare_service: None,
            stale_prepare: AtomicBool::new(false),
            next_container: AtomicUsize::new(1),
            fanout_barrier: Some(Arc::new(Barrier::new(2))),
        });
        fixture.executor = SimpleDeploy::new(
            fixture
                .machines
                .first()
                .cloned()
                .expect("two-machine fixture has a first machine"),
            fixture.store.clone(),
            fixture.hosts.clone(),
        );
        fixture
            .command
            .request
            .services
            .get_mut(&CorrosionServiceName::try_new("api").expect("service"))
            .expect("fixture has one service")
            .placement = Some(RequestedPlacement::Replicas {
            replicas: ServiceReplicaCount::try_new(2).expect("two replicas"),
        });
        let started = fixture
            .executor
            .start(fixture.command.clone())
            .await
            .expect("start deploy");

        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            fixture.executor.run(started),
        )
        .await
        .expect("target calls should overlap instead of waiting serially")
        .expect("deploy");

        assert!(outcome.is_success());
    }

    #[test]
    fn placement_stickiness_uses_only_the_exact_namespace_service() {
        let production = CorrosionNamespaceName::try_new("production").expect("namespace");
        let staging = CorrosionNamespaceName::try_new("staging").expect("namespace");
        let api = CorrosionServiceName::try_new("api").expect("service");
        let worker = CorrosionServiceName::try_new("worker").expect("service");
        let release = DeployName::try_new("release-1").expect("deploy");
        let observed =
            |container: &str,
             namespace_id: CorrosionNamespaceName,
             service_name: CorrosionServiceName| DeployObservedContainer {
                container_id: ContainerId::try_new(container).expect("container"),
                identity: V2ManagedContainerIdentity {
                    namespace_id,
                    service_name,
                    operation_id: release.clone(),
                    replica_slot: ReplicaSlot::Global,
                },
                running: true,
                host_ports: HostPortBindings::default(),
            };
        let containers = vec![
            observed("api", production.clone(), api.clone()),
            observed("worker", production.clone(), worker),
            observed("staging-api", staging, api.clone()),
        ];

        assert_eq!(
            observed_service_containers(&containers, &production, &api),
            vec![ServiceContainerObservation { deploy: release }]
        );
    }

    #[tokio::test]
    async fn foreign_appointment_interrupts_before_any_host_effect() {
        let fixture = fixture(1);
        fixture.store.controller.lock().await.appointment_id =
            ControllerRevision::try_new(2).expect("appointment");

        let started = fixture
            .executor
            .start(fixture.command.clone())
            .await
            .expect("start deploy");
        let outcome = fixture.executor.run(started).await.expect("deploy");

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

        let started = fixture
            .executor
            .start(fixture.command.clone())
            .await
            .expect("start deploy");
        let outcome = fixture.executor.run(started).await.expect("deploy");

        assert!(matches!(outcome, CorrosionDeployOutcome::Interrupted));
    }
}
