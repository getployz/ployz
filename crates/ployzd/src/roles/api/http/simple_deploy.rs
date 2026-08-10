//! One whole deploy run for the preferred controller.
//!
//! Every invocation observes Corrosion and target hosts, derives one complete
//! placement, performs coarse idempotent host effects, and publishes one row
//! flip. There is deliberately no phase journal or recovery state machine.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::future::join_all;
use ployz_core::corrosion::{
    ControllerDocument, CorrosionDeployFailure, CorrosionDeployOutcome, CorrosionDeployWarning,
    CorrosionDocumentVersion, CorrosionServiceName, CorrosionTimestamp, HostPortBindings,
    MachineLoadBand, NamespaceDocument, OperationDocument, OperationInitiator,
    OperatorWriteProvenance, PublishedService, RouteBindingDocument, ServicePlacement,
    ServiceReplicaCount, V2ManagedContainerIdentity, fingerprint_env_value,
    is_preferred_controller,
};
use ployz_core::deploy::{ReplicaSlot, ReplicatedReplicaSlot};
use ployz_core::ids::{CorrosionNamespaceName, DeployName};
use ployz_core::machine::{MachineLifecycle, MachineName};
use ployz_core::placement::{
    PlacementBid, PlacementPickInputs, PlacementRefusal, ServiceContainerObservation,
    pick_placement,
};
use ployz_core::{
    DeployDesiredReplica, DeployInspectOutcome, DeployObservedContainer, DeployPrepareOutcome,
    DeployPrepareRequest, DeployPreparedReplica, DeployRefusal, DeployRequest, DeployRetireOutcome,
    DeployRetireRequest, DeployServiceRequest, HealthGatePolicy, RequestedPlacement,
};

/// The only machine-addressed seam used by the controller.
///
/// Its production adapter chooses local execution or bounded mesh HTTP. Both
/// paths expose the same three coarse target-host operations.
#[async_trait]
pub(super) trait DeployHosts: Send + Sync {
    async fn inspect(
        &self,
        machine_id: &MachineName,
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

    async fn observe(&self, command: &DeployCommand) -> Result<DeployProjection, DeployStartError>;

    /// Inserts the one-shot named deploy attempt. `false` means the name is already used.
    async fn create_operation(&self, document: &OperationDocument) -> Result<bool, String>;

    async fn write_operation(&self, document: &OperationDocument) -> Result<(), String>;

    /// Atomically replaces the namespace's complete serving projection.
    async fn commit(&self, commit: DeployCommit) -> Result<(), String>;
}

/// Converged namespace intent and serving projection needed to plan one deploy.
/// Docker execution reality is observed separately through [`DeployHosts`].
#[derive(Debug, Clone)]
pub(super) struct DeployProjection {
    pub(super) namespace: NamespaceDocument,
    /// Deterministic automatic routes that do not exist yet.
    pub(super) missing_automatic_routes: Vec<RouteBindingDocument>,
    pub(super) roster: Vec<DeployRosterMachine>,
}

#[derive(Debug, Clone)]
pub(super) struct DeployRosterMachine {
    pub(super) name: MachineName,
    pub(super) lifecycle: MachineLifecycle,
}

/// One admitted controller command. Target hosts persist their own coarse
/// effects; the preferred controller itself owns no workflow history.
#[derive(Clone)]
pub(super) struct DeployCommand {
    pub(super) request: DeployRequest,
    pub(super) initiator: OperationInitiator,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeployCommit {
    pub(super) namespace: NamespaceDocument,
    pub(super) missing_automatic_routes: Vec<RouteBindingDocument>,
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
        let created_at = CorrosionTimestamp::now_utc();
        let created = OperationDocument::deploy_created(
            CorrosionDocumentVersion::V1,
            context.reality.namespace.cluster_id.clone(),
            self.machine_id.clone(),
            command.initiator.clone(),
            context.reality.namespace.name.clone(),
            command.request.deploy_name.clone(),
            created_at,
        );
        if !self.store.create_operation(&created).await? {
            return Err(DeployStartError::Refused(
                DeployRefusal::DeployNameAlreadyUsed {
                    namespace_name: command.request.namespace_name,
                    deploy_name: command.request.deploy_name,
                },
            ));
        }
        Ok(StartedDeploy {
            command,
            context,
            created,
        })
    }

    /// Removes exact locally-observed containers for one service from every
    /// machine in the caller's local roster view.
    pub(super) async fn retire_service_containers(
        &self,
        namespace_name: &CorrosionNamespaceName,
        service_name: &CorrosionServiceName,
        machines: &[MachineName],
    ) -> Vec<MachineName> {
        let inspections =
            join_all(machines.iter().map(|machine| async move {
                (machine.clone(), self.hosts.inspect(machine).await)
            }))
            .await;
        let mut failed = BTreeSet::new();
        let mut retire = Vec::new();
        for (machine, inspection) in inspections {
            let Ok(DeployInspectOutcome::Inspected { containers, .. }) = inspection else {
                failed.insert(machine);
                continue;
            };
            let mut by_deploy = BTreeMap::<DeployName, Vec<DeployObservedContainer>>::new();
            for container in containers.into_iter().filter(|container| {
                container.identity.namespace_id == *namespace_name
                    && container.identity.service_name == *service_name
            }) {
                by_deploy
                    .entry(container.identity.operation_id.clone())
                    .or_default()
                    .push(container);
            }
            retire.extend(by_deploy.into_iter().map(|(deploy, containers)| {
                let request = DeployRetireRequest {
                    operation_id: service_removal_operation_id(service_name, &deploy),
                    namespace_name: namespace_name.clone(),
                    containers,
                    restart_after_retire: Vec::new(),
                };
                (machine.clone(), request)
            }));
        }
        let outcomes = join_all(retire.into_iter().map(|(machine, request)| async move {
            let outcome = self.hosts.retire(&machine, request).await;
            (machine, outcome)
        }))
        .await;
        for (machine, outcome) in outcomes {
            if !matches!(outcome, Ok(DeployRetireOutcome::Retired)) {
                failed.insert(machine);
            }
        }
        failed.into_iter().collect()
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

        if !self.local_machine_is_preferred().await? {
            return self.interrupt(command, &created).await;
        }

        if let Some(failure) = context.admission_failure {
            return self.fail(command, &created, failure).await;
        }
        let inspections = self.inspect_roster(&context.reality).await;
        let mut prepared_cutovers = Vec::new();
        let mut planned_counts = BTreeMap::new();
        let mut prepared_services = Vec::with_capacity(command.request.services.len());
        for (service_name, service) in command.request.services.iter() {
            if !service.runtime.volume_mounts.is_empty()
                && let Some(machine_id) = context.reality.roster.iter().find_map(|machine| {
                    inspections.get(&machine.name).and_then(|inspection| {
                        inspection
                            .containers
                            .iter()
                            .any(|container| {
                                container.identity.namespace_id == context.reality.namespace.name
                                    && container.identity.service_name == *service_name
                                    && container.identity.operation_id
                                        != command.request.deploy_name
                            })
                            .then(|| machine.name.clone())
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

            let placement = match derive_placement(
                service_name,
                service,
                &context,
                &inspections,
                &planned_counts,
            ) {
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
                &command.request.deploy_name,
                &context.reality.namespace.name,
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
            let operation_id = command.request.deploy_name.clone();
            let namespace_name = context.reality.namespace.name.clone();
            let service_name = service_name.clone();
            let image = service.image.clone();
            let credential = service.credential.clone();
            let runtime = service.runtime.clone();
            let health_gate = service.health_gate;
            let grouped = group_by_machine(&desired);
            let prepare_calls = grouped.into_iter().map(|(machine_id, replicas)| {
                let operation_id = operation_id.clone();
                let namespace_name = namespace_name.clone();
                let service_name = service_name.clone();
                let image = image.clone();
                let credential = credential.clone();
                let runtime = runtime.clone();
                let stop_before_start = inspections
                    .get(&machine_id)
                    .map(|inspection| conflicting_incumbents(&replicas, &inspection.containers))
                    .unwrap_or_default();
                async move {
                    let request = DeployPrepareRequest {
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
                    Ok(DeployPrepareOutcome::Refused) => {
                        prepare_failure
                            .get_or_insert(CorrosionDeployFailure::PrepareRefused { machine_id });
                        continue;
                    }
                    Err(DeployHostError::StaleController) => {
                        stale_controller = true;
                        continue;
                    }
                    Ok(DeployPrepareOutcome::Failed) | Err(DeployHostError::Failed) => {
                        prepare_failure
                            .get_or_insert(CorrosionDeployFailure::PrepareFailed { machine_id });
                        continue;
                    }
                };
                let expected = replicas_for(&desired, &machine_id);
                prepared_cutovers.push(PreparedCutover {
                    machine_id: machine_id.clone(),
                    candidates: replicas
                        .iter()
                        .map(|replica| DeployObservedContainer {
                            identity: replica.identity.clone(),
                            host_ports: expected
                                .iter()
                                .find(|desired| desired.identity == replica.identity)
                                .map(|desired| desired.host_ports.clone())
                                .unwrap_or_default(),
                        })
                        .collect(),
                    displaced_incumbents,
                });
                if !prepared_matches(&replicas, &expected) {
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
            for machine in &placement.targets {
                *planned_counts.entry(machine.clone()).or_insert(0) += 1;
            }
            prepared_services.push(PreparedService {
                service_name,
                request: service.clone(),
                placement,
                resolved_image,
                replicas: prepared,
            });
        }
        let written_at = CorrosionTimestamp::now_utc();
        let commit = match build_commit(command, &context, &prepared_services, written_at) {
            Ok(commit) => commit,
            Err(error) => {
                self.rollback_prepared(command, &context, &prepared_cutovers)
                    .await;
                return Err(error);
            }
        };
        if let Err(error) = self.store.commit(commit).await {
            tracing::warn!(%error, "deploy commit outcome is unknown; leaving host reality for the next full deploy to reconcile");
            return self.interrupt(command, &created).await;
        }

        let keep = prepared_services
            .iter()
            .flat_map(|service| &service.replicas)
            .map(|placed| (placed.machine_id.clone(), placed.replica.identity.clone()))
            .collect();
        self.finish_after_flip(command, &created, &context, &inspections, &keep)
            .await
    }

    async fn local_machine_is_preferred(&self) -> Result<bool, String> {
        let controller = self.store.controller().await?;
        Ok(is_preferred_controller(&controller, &self.machine_id))
    }

    async fn inspect_roster(
        &self,
        reality: &DeployProjection,
    ) -> BTreeMap<MachineName, HostInspection> {
        let calls = reality.roster.iter().map(|machine| async move {
            let outcome = self.hosts.inspect(&machine.name).await;
            (machine, outcome)
        });
        let mut inspections = BTreeMap::new();
        for (machine, outcome) in join_all(calls).await {
            match outcome {
                Ok(DeployInspectOutcome::Inspected {
                    bridge_ready,
                    free_disk_bytes,
                    load,
                    containers,
                }) => {
                    inspections.insert(
                        machine.name.clone(),
                        HostInspection {
                            bridge_ready,
                            free_disk_bytes,
                            load,
                            containers,
                        },
                    );
                }
                Ok(DeployInspectOutcome::Failed) | Err(_) => {}
            }
        }
        inspections
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
                            operation_id: command.request.deploy_name.clone(),
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
        keep: &HashSet<(MachineName, V2ManagedContainerIdentity)>,
    ) -> Result<CorrosionDeployOutcome, String> {
        let retire_calls = inspections.iter().filter_map(|(machine_id, inspection)| {
            let containers = inspection
                .containers
                .iter()
                .filter(|container| {
                    container.identity.namespace_id == context.reality.namespace.name
                        && !keep.contains(&(machine_id.clone(), container.identity.clone()))
                })
                .cloned()
                .collect::<Vec<_>>();
            (!containers.is_empty()).then_some(async move {
                let outcome = self
                    .hosts
                    .retire(
                        machine_id,
                        DeployRetireRequest {
                            operation_id: command.request.deploy_name.clone(),
                            namespace_name: context.reality.namespace.name.clone(),
                            containers,
                            restart_after_retire: Vec::new(),
                        },
                    )
                    .await;
                (machine_id, outcome)
            })
        });
        let mut cleanup_failed = context
            .reality
            .roster
            .iter()
            .filter(|machine| !inspections.contains_key(&machine.name))
            .map(|machine| machine.name.clone())
            .collect::<Vec<_>>();
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
        let completed_at = CorrosionTimestamp::now_utc();
        let operation = created.clone().into_terminal(completed_at, outcome.clone());
        self.store.write_operation(&operation).await?;
        Ok(outcome)
    }
}

fn service_removal_operation_id(
    service_name: &CorrosionServiceName,
    deploy: &DeployName,
) -> DeployName {
    DeployName::try_new(format!(
        "remove-{}-{}-{}",
        service_name.as_str().len(),
        service_name.as_str(),
        deploy.as_str()
    ))
    .expect("validated names produce a valid deterministic removal name")
}

struct DeployContext {
    reality: DeployProjection,
    admission_failure: Option<CorrosionDeployFailure>,
}

fn classify_services(
    request: &DeployRequest,
    reality: DeployProjection,
) -> Result<DeployContext, DeployStartError> {
    let admission_failure = request
        .services
        .iter()
        .fold(None, |failure, (_service_name, service)| {
            failure.or_else(|| unknown_pin(service, &reality))
        });
    let context = DeployContext {
        reality,
        admission_failure,
    };
    validate_effective_host_ports(request)?;
    Ok(context)
}

fn validate_effective_host_ports(request: &DeployRequest) -> Result<(), DeployStartError> {
    let mut claimed = BTreeMap::new();
    for (service_name, service) in request.services.iter() {
        let ServicePlacement::Global { host_ports } = effective_placement(service) else {
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

fn unknown_pin(
    request: &DeployServiceRequest,
    reality: &DeployProjection,
) -> Option<CorrosionDeployFailure> {
    let Some(names) = &request.machines else {
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
    free_disk_bytes: u64,
    load: MachineLoadBand,
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
    planned_counts: &BTreeMap<MachineName, usize>,
) -> Result<EffectivePlacement, PlacementRefusal> {
    let placement = effective_placement(request);
    let pinned_machines = resolve_pins(request, context);
    let active_deploy = context
        .reality
        .namespace
        .services
        .get(service_name)
        .map(|service| service.active_deploy.clone());
    let mut bids = Vec::new();
    for machine in &context.reality.roster {
        let Some(inspection) = inspections.get(&machine.name) else {
            continue;
        };
        bids.push(PlacementBid {
            machine_name: machine.name.clone(),
            lifecycle: machine.lifecycle,
            endpoint_network_ready: inspection.bridge_ready,
            free_disk_bytes: inspection.free_disk_bytes,
            load: inspection.load,
            total_container_count: inspection
                .containers
                .len()
                .saturating_add(planned_counts.get(&machine.name).copied().unwrap_or(0)),
            service_containers: observed_service_containers(
                &inspection.containers,
                &context.reality.namespace.name,
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

fn effective_placement(request: &DeployServiceRequest) -> ServicePlacement {
    let one = || ServiceReplicaCount::try_new(1).expect("one replica is valid");
    match &request.placement {
        Some(RequestedPlacement::Replicated { replicas }) => ServicePlacement::Replicated {
            replicas: *replicas,
        },
        Some(RequestedPlacement::Global { host_ports }) => ServicePlacement::Global {
            host_ports: host_ports.clone(),
        },
        None => ServicePlacement::Replicated { replicas: one() },
    }
}

fn resolve_pins(request: &DeployServiceRequest, context: &DeployContext) -> BTreeSet<MachineName> {
    match &request.machines {
        Some(names) => context
            .reality
            .roster
            .iter()
            .filter(|machine| names.iter().any(|name| name == &machine.name))
            .map(|machine| machine.name.clone())
            .collect(),
        None => BTreeSet::new(),
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
            .any(|existing| existing.identity == container.identity)
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
    let mut services = BTreeMap::new();
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
        services.insert(
            service_name.clone(),
            PublishedService {
                image: prepared_service.resolved_image.clone(),
                env_fingerprints,
                placement: prepared_service.placement.placement.clone(),
                pinned_machines: prepared_service.placement.pinned_machines.clone(),
                active_deploy: command.request.deploy_name.clone(),
                previous_image: context
                    .reality
                    .namespace
                    .services
                    .get(service_name)
                    .map(|incumbent| incumbent.image.clone()),
                deployed_at: written_at,
            },
        );
    }
    Ok(DeployCommit {
        namespace: NamespaceDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: context.reality.namespace.cluster_id.clone(),
            provenance: OperatorWriteProvenance {
                written_by: command.initiator.clone(),
                written_at,
            },
            name: context.reality.namespace.name.clone(),
            services,
        },
        missing_automatic_routes: context.reality.missing_automatic_routes.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::num::NonZeroU16;
    use std::sync::atomic::{AtomicBool, Ordering};

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
        reality: Mutex<DeployProjection>,
        commits: Mutex<Vec<DeployCommit>>,
        reject_commit: AtomicBool,
    }

    #[async_trait]
    impl SimpleDeployStore for FakeStore {
        async fn controller(&self) -> Result<ControllerDocument, String> {
            Ok(self.controller.lock().await.clone())
        }

        async fn observe(
            &self,
            _command: &DeployCommand,
        ) -> Result<DeployProjection, DeployStartError> {
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
            if self.reject_commit.load(Ordering::SeqCst) {
                return Err("commit rejected".to_owned());
            }
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
        stale_inspection_on: Mutex<Option<MachineName>>,
        prepare_requests: Mutex<Vec<DeployPrepareRequest>>,
        retire_requests: Mutex<Vec<DeployRetireRequest>>,
        fail_prepare_service: Option<CorrosionServiceName>,
        stale_prepare: AtomicBool,
        fanout_barrier: Option<Arc<Barrier>>,
    }

    #[async_trait]
    impl DeployHosts for FakeHosts {
        async fn inspect(
            &self,
            machine_id: &MachineName,
        ) -> Result<DeployInspectOutcome, DeployHostError> {
            self.calls
                .lock()
                .await
                .push(HostCall::Inspect(machine_id.clone()));
            if let Some(barrier) = &self.fanout_barrier {
                barrier.wait().await;
            }
            if self.stale_inspection_on.lock().await.as_ref() == Some(machine_id) {
                return Err(DeployHostError::StaleController);
            }
            Ok(DeployInspectOutcome::Inspected {
                bridge_ready: true,
                free_disk_bytes: 10 * 1024 * 1024 * 1024,
                load: MachineLoadBand::Idle,
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
                return Ok(DeployPrepareOutcome::Failed);
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
                .map(|desired| DeployPreparedReplica {
                    identity: desired.identity,
                    ip: Ipv4Addr::new(10, 210, 20, 2),
                })
                .collect();
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
            name: namespace_id.clone(),
            services: BTreeMap::new(),
        };
        let roster = machine_ids
            .iter()
            .map(|id| DeployRosterMachine {
                name: id.clone(),
                lifecycle: MachineLifecycle::Active,
            })
            .collect();
        let reality = DeployProjection {
            namespace,
            missing_automatic_routes: Vec::new(),
            roster,
        };
        let store = Arc::new(FakeStore {
            controller: Mutex::new(ControllerDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id,
                preferred_machine_name: machine_id.clone(),
                heartbeat_at: at,
            }),
            create_operation: AtomicBool::new(true),
            operation: Mutex::new(None),
            reality: Mutex::new(reality),
            commits: Mutex::new(Vec::new()),
            reject_commit: AtomicBool::new(false),
        });
        let hosts = Arc::new(FakeHosts {
            calls: Mutex::new(Vec::new()),
            inspections: Mutex::new(Vec::new()),
            stale_inspection_on: Mutex::new(None),
            prepare_requests: Mutex::new(Vec::new()),
            retire_requests: Mutex::new(Vec::new()),
            fail_prepare_service: None,
            stale_prepare: AtomicBool::new(false),
            fanout_barrier: None,
        });
        let request = DeployRequest {
            namespace_name: CorrosionNamespaceName::try_new("production").expect("namespace"),
            deploy_name: operation_id.clone(),
            services: [(
                CorrosionServiceName::try_new("api").expect("service"),
                DeployServiceRequest {
                    image: ImageReference::try_new("nginx:1.27-alpine").expect("image"),
                    credential: None,
                    runtime: ContainerRuntimeSpec::image_defaults(),
                    health_gate: HealthGatePolicy::Enforce,
                    placement: None,
                    machines: None,
                },
            )]
            .into_iter()
            .collect(),
        };
        let command = DeployCommand { request, initiator };
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
        let namespace_id = fixture.store.reality.lock().await.namespace.name.clone();
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
        assert_eq!(commit.namespace.name, namespace_id);
        assert_eq!(commit.namespace.services.len(), 1);
        let service = commit
            .namespace
            .services
            .get(&CorrosionServiceName::try_new("api").expect("service"))
            .expect("api projection");
        assert!(matches!(
            service.placement,
            ServicePlacement::Replicated { replicas } if replicas.get() == 1
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
    async fn complete_snapshot_spreads_services_over_planned_replicas() {
        let mut fixture = fixture(2);
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
        let started = fixture
            .executor
            .start(fixture.command.clone())
            .await
            .expect("start deploy");

        fixture.executor.run(started).await.expect("deploy");

        let prepared_on = fixture
            .hosts
            .calls
            .lock()
            .await
            .iter()
            .filter_map(|call| match call {
                HostCall::Prepare(machine) => Some(machine.clone()),
                HostCall::Inspect(_) | HostCall::Retire(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(prepared_on, fixture.machines);
    }

    #[tokio::test]
    async fn divergent_machine_view_does_not_block_deploy() {
        let fixture = fixture(2);
        let [_, unreachable] = fixture.machines.as_slice() else {
            panic!("fixture must have two machines")
        };
        let unreachable = unreachable.clone();
        *fixture.hosts.stale_inspection_on.lock().await = Some(unreachable.clone());
        let started = fixture
            .executor
            .start(fixture.command.clone())
            .await
            .expect("start deploy");

        let outcome = fixture.executor.run(started).await.expect("deploy");

        assert_eq!(
            outcome,
            CorrosionDeployOutcome::Completed {
                warnings: vec![CorrosionDeployWarning::CleanupIncomplete {
                    machines: vec![unreachable],
                }],
            }
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
        assert_eq!(commit.namespace.services.len(), 2);
        assert_eq!(
            commit
                .namespace
                .services
                .keys()
                .map(CorrosionServiceName::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["api", "worker"]),
        );
        assert!(
            commit
                .namespace
                .services
                .values()
                .all(|service| service.active_deploy == fixture.command.request.deploy_name)
        );
    }

    #[tokio::test]
    async fn complete_snapshot_does_not_inherit_placement_or_pins() {
        let fixture = fixture(1);
        let service_name = CorrosionServiceName::try_new("api").expect("service");
        let old_image = ImageReference::try_new("nginx:1.26-alpine").expect("image");
        fixture
            .store
            .reality
            .lock()
            .await
            .namespace
            .services
            .insert(
                service_name.clone(),
                PublishedService {
                    image: old_image.clone(),
                    env_fingerprints: BTreeMap::new(),
                    placement: ServicePlacement::Global {
                        host_ports: HostPortBindings::default(),
                    },
                    pinned_machines: BTreeSet::from([fixture
                        .machines
                        .first()
                        .cloned()
                        .expect("machine")]),
                    active_deploy: DeployName::try_new("release-0").expect("deploy"),
                    previous_image: None,
                    deployed_at: CorrosionTimestamp::try_new("2026-08-07T00:00:00Z").expect("time"),
                },
            );

        let started = fixture
            .executor
            .start(fixture.command.clone())
            .await
            .expect("start deploy");
        fixture.executor.run(started).await.expect("deploy");

        let commits = fixture.store.commits.lock().await;
        let service = commits
            .first()
            .and_then(|commit| commit.namespace.services.get(&service_name))
            .expect("published service");
        assert!(matches!(
            service.placement,
            ServicePlacement::Replicated { replicas } if replicas.get() == 1
        ));
        assert!(service.pinned_machines.is_empty());
        assert_eq!(service.previous_image, Some(old_image));
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
            identity: V2ManagedContainerIdentity {
                namespace_id: CorrosionNamespaceName::try_new("production").expect("namespace"),
                service_name: CorrosionServiceName::try_new("api").expect("service"),
                operation_id: DeployName::try_new("release-0").expect("deploy"),
                replica_slot: ReplicaSlot::Global,
            },
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
    async fn later_service_failure_rolls_back_known_prepared_containers() {
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
            identity: V2ManagedContainerIdentity {
                namespace_id: CorrosionNamespaceName::try_new("production").expect("namespace"),
                service_name: CorrosionServiceName::try_new("api").expect("service"),
                operation_id: DeployName::try_new("release-0").expect("deploy"),
                replica_slot: ReplicaSlot::Global,
            },
            host_ports,
        };
        fixture.hosts = Arc::new(FakeHosts {
            calls: Mutex::new(Vec::new()),
            inspections: Mutex::new(vec![incumbent.clone()]),
            stale_inspection_on: Mutex::new(None),
            prepare_requests: Mutex::new(Vec::new()),
            retire_requests: Mutex::new(Vec::new()),
            fail_prepare_service: Some(CorrosionServiceName::try_new("worker").expect("service")),
            stale_prepare: AtomicBool::new(false),
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
        let [candidate] = rollback.containers.as_slice() else {
            panic!("one prepared container expected")
        };
        assert_eq!(candidate.identity.service_name.as_str(), "api");
        assert_eq!(rollback.restart_after_retire, vec![incumbent]);
    }

    #[tokio::test]
    async fn unknown_commit_outcome_is_left_for_the_next_deploy_to_reconcile() {
        let rejected = fixture(1);
        rejected.store.reject_commit.store(true, Ordering::SeqCst);
        let started = rejected
            .executor
            .start(rejected.command.clone())
            .await
            .expect("start rejected commit");
        let outcome = rejected.executor.run(started).await.expect("deploy");
        assert!(matches!(outcome, CorrosionDeployOutcome::Interrupted));
        assert!(rejected.hosts.retire_requests.lock().await.is_empty());
    }

    #[tokio::test]
    async fn target_host_inspection_and_prepare_fan_out_concurrently() {
        let mut fixture = fixture(2);
        fixture.hosts = Arc::new(FakeHosts {
            calls: Mutex::new(Vec::new()),
            inspections: Mutex::new(Vec::new()),
            stale_inspection_on: Mutex::new(None),
            prepare_requests: Mutex::new(Vec::new()),
            retire_requests: Mutex::new(Vec::new()),
            fail_prepare_service: None,
            stale_prepare: AtomicBool::new(false),
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
            .placement = Some(RequestedPlacement::Replicated {
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
        let observed = |namespace_id: CorrosionNamespaceName,
                        service_name: CorrosionServiceName| {
            DeployObservedContainer {
                identity: V2ManagedContainerIdentity {
                    namespace_id,
                    service_name,
                    operation_id: release.clone(),
                    replica_slot: ReplicaSlot::Global,
                },
                host_ports: HostPortBindings::default(),
            }
        };
        let containers = vec![
            observed(production.clone(), api.clone()),
            observed(production.clone(), worker),
            observed(staging, api.clone()),
        ];

        assert_eq!(
            observed_service_containers(&containers, &production, &api),
            vec![ServiceContainerObservation { deploy: release }]
        );
    }

    #[tokio::test]
    async fn service_removal_retires_only_exact_runtime_matches() {
        let fixture = fixture(1);
        let namespace = CorrosionNamespaceName::try_new("production").expect("namespace");
        let service = CorrosionServiceName::try_new("api").expect("service");
        let deploy = DeployName::try_new("release-0").expect("deploy");
        let observed = |service_name: CorrosionServiceName| DeployObservedContainer {
            identity: V2ManagedContainerIdentity {
                namespace_id: namespace.clone(),
                service_name,
                operation_id: deploy.clone(),
                replica_slot: ReplicaSlot::Global,
            },
            host_ports: HostPortBindings::default(),
        };
        let target = observed(service.clone());
        let unrelated = observed(CorrosionServiceName::try_new("worker").expect("service"));
        *fixture.hosts.inspections.lock().await = vec![target.clone(), unrelated];

        assert!(
            fixture
                .executor
                .retire_service_containers(&namespace, &service, &fixture.machines)
                .await
                .is_empty()
        );
        let requests = fixture.hosts.retire_requests.lock().await;
        let [request] = requests.as_slice() else {
            panic!("one exact service retirement expected")
        };
        assert_eq!(request.operation_id.as_str(), "remove-3-api-release-0");
        assert_eq!(request.containers, vec![target]);
        assert!(request.restart_after_retire.is_empty());
    }

    #[tokio::test]
    async fn foreign_controller_interrupts_before_any_host_effect() {
        let fixture = fixture(1);
        fixture.store.controller.lock().await.preferred_machine_name =
            MachineName::try_new("another-machine").expect("machine");

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
