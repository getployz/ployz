//! Pure deploy preparation and placement planning.

use super::*;
use crate::ids::OperationId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPlanningInput {
    pub service_id: ServiceId,
    pub eligible_machines: Vec<MachineId>,
    pub existing_replicas: Vec<ExistingServiceReplica>,
    pub cleanup_candidates: Vec<ObservedCleanupCandidate>,
    pub volume_pins: Vec<VolumePinState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingServiceReplica {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub creation_gate: ExistingReplicaCreationGate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingReplicaCreationGate {
    /// Current serving intent proves a prior deploy completed the creation gate.
    AlreadyPassed,
    /// Durable interruption provenance permits adoption, but cannot prove the
    /// container passed its first-creation gate.
    RequiredAfterInterruption,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExistingReplicaPolicy {
    /// Matching observed replicas belong to the current serving target.
    Promoted {
        /// Containers created by an interrupted attempt against this same
        /// entry still require their own creation gate.
        interrupted_operation_ids: std::collections::BTreeSet<OperationId>,
    },
    /// Only replicas created by one of these durably interrupted deploys may
    /// be adopted, and each adopted replica must pass its creation gate.
    RecoverInterrupted {
        operation_ids: std::collections::BTreeSet<OperationId>,
    },
    /// Matching observations have neither serving intent nor recoverable
    /// interruption provenance.
    ExcludeUnpromoted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPreparationInput<'a> {
    pub request: &'a VolumeDeclaredDeployRequest,
    pub service_id: ServiceId,
    pub occupied_route_bindings: Vec<RouteBindingState>,
    pub eligible_machines: Vec<MachineId>,
    /// Machines under drain intent: their running replicas do not count as
    /// existing capacity, so the plan replaces them elsewhere and the
    /// unselected containers fall through to cleanup. This is how drain
    /// evacuates on the next deploy (ADR 0026). A separate list rather than
    /// filtering by `eligible_machines`: replica reuse is reality-driven and
    /// deliberately independent of placement scope, so only explicit drain
    /// intent may disqualify observed capacity.
    pub draining_machines: Vec<MachineId>,
    pub observed_machines: Vec<MachineContainerObservationSnapshot>,
    pub existing_replica_policy: ExistingReplicaPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedDeploy {
    pub service: DeployServiceSpec,
    pub route_commits: Vec<RouteBindingState>,
    pub eligible_machines: Vec<MachineId>,
    pub existing_replicas: Vec<ExistingServiceReplica>,
    pub cleanup_candidates: Vec<ObservedCleanupCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployPlan {
    pub namespace_id: NamespaceId,
    pub namespace_revision_id: NamespaceRevisionId,
    pub phases: Vec<DeployPhasePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_pin_commits: Vec<VolumePinState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup_containers: Vec<DeployCleanupContainer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_reclamations: Vec<DeployImageReclamation>,
}

impl DeployPlan {
    #[must_use]
    pub fn target_machines(&self) -> Vec<MachineId> {
        let mut machines = self
            .phases
            .iter()
            .flat_map(|phase| &phase.services)
            .flat_map(|service| &service.steps)
            .map(|step| match step {
                DeployPlanStep::UseExistingContainer { machine_id, .. }
                | DeployPlanStep::RunContainer { machine_id, .. } => machine_id.clone(),
            })
            .collect::<Vec<_>>();
        machines.sort();
        machines.dedup();
        machines
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployPhasePlan {
    pub services: Vec<DeployServicePlan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployServicePlan {
    pub service_id: ServiceId,
    pub steps: Vec<DeployPlanStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_start: Option<PreStartHookStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PreStartHookStep {
    pub machine_id: MachineId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployPlanStep {
    UseExistingContainer {
        machine_id: MachineId,
        container_id: ContainerId,
        slot: ReplicaSlot,
    },
    RunContainer {
        machine_id: MachineId,
        slot: ReplicaSlot,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "SafeInteger<\"ReplicaSlot\">"))]
#[serde(try_from = "u16", into = "u16")]
pub struct ReplicaSlot(u16);

impl ReplicaSlot {
    pub fn try_new(value: u16) -> Result<Self, ReplicaSlotError> {
        if value == 0 {
            return Err(ReplicaSlotError::Zero);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for ReplicaSlot {
    type Error = ReplicaSlotError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ReplicaSlot> for u16 {
    fn from(value: ReplicaSlot) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplicaSlotError {
    #[error("replica slot must be greater than zero")]
    Zero,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployPlanError {
    UnknownService {
        service_id: ServiceId,
    },
    NoEligibleMachines,
    UnknownServiceDependency {
        service_id: ServiceId,
        dependency: ServiceId,
    },
    ServiceDependencyCycle {
        service_ids: Vec<ServiceId>,
    },
    HealthyDependencyWithoutHealthcheck {
        service_id: ServiceId,
        dependency: ServiceId,
    },
    ConflictingVolumePins {
        service_id: ServiceId,
        machines: Vec<MachineId>,
    },
    ProvisionedVolumeRequiresProvisioning {
        service_id: ServiceId,
        volume_name: VolumeName,
    },
    VolumePinIncompatible {
        service_id: ServiceId,
        volume_name: VolumeName,
        declaration: VolumeSpec,
        pin_kind: crate::intent::VolumeKind,
    },
    ProvisionedVolumeShrink {
        service_id: ServiceId,
        volume_name: VolumeName,
        declared_max_size_bytes: VolumeMaxSizeBytes,
        pinned_max_size_bytes: VolumeMaxSizeBytes,
    },
}

pub fn prepare_deploy(
    input: DeployPreparationInput<'_>,
    mut new_route_binding_id: impl FnMut(&RouteTarget) -> RouteBindingId,
) -> Result<PreparedDeploy, DeployPreparationError> {
    let Some(service) = input.request.service(&input.service_id) else {
        return Err(DeployPreparationError::UnknownService {
            service_id: input.service_id,
        });
    };
    let route_commits = route_binding_commits(
        input.request.namespace_id(),
        service,
        &input.occupied_route_bindings,
        &mut new_route_binding_id,
    )?;
    let existing_replicas = existing_replicas(
        input.request.namespace_id(),
        service,
        &input.observed_machines,
        &input.draining_machines,
        &input.existing_replica_policy,
    );
    let cleanup_candidates = cleanup_candidates(
        input.request.namespace_id(),
        service,
        &input.observed_machines,
        &input.draining_machines,
    );

    Ok(PreparedDeploy {
        service: service.clone(),
        route_commits,
        eligible_machines: input.eligible_machines,
        existing_replicas,
        cleanup_candidates,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployPreparationError {
    #[error("validated deploy request does not contain service {}", .service_id.as_str())]
    UnknownService { service_id: ServiceId },
    #[error(transparent)]
    Route(#[from] RouteBindingCommitError),
}

/// Route binding removals are a namespace-level decision: the manifest is
/// the full desired state, so any stored binding whose target no service in
/// the manifest declares is detached - including bindings owned by services
/// the manifest omits entirely.
#[must_use]
pub fn namespace_route_binding_removals(
    namespace_id: &NamespaceId,
    declared_targets: &[RouteTarget],
    stored_bindings: &[RouteBindingState],
) -> Vec<RouteTarget> {
    stored_bindings
        .iter()
        .filter(|binding| {
            binding.namespace_id == *namespace_id && !declared_targets.contains(&binding.target)
        })
        .map(|binding| binding.target.clone())
        .collect()
}

/// Serving target entries of services the manifest omits are unpublished:
/// manifest omission removes a service from the namespace, so its entry
/// must not stay serveable in stored state.
#[must_use]
pub fn namespace_serving_target_removals(
    namespace_id: &NamespaceId,
    declared_services: &[ServiceId],
    stored_entries: &[ServingTargetEntry],
) -> Vec<ServingTargetEntry> {
    stored_entries
        .iter()
        .filter(|entry| {
            entry.namespace_id == *namespace_id && !declared_services.contains(&entry.service_id)
        })
        .cloned()
        .collect()
}

/// Replicas on draining machines are excluded from reuse: they keep serving
/// until convergence, but not counting them here means the plan places their
/// replacement on an eligible machine and cleanup removes the original.
fn existing_replicas(
    namespace_id: &NamespaceId,
    service: &DeployServiceSpec,
    observed_machines: &[MachineContainerObservationSnapshot],
    draining_machines: &[MachineId],
    policy: &ExistingReplicaPolicy,
) -> Vec<ExistingServiceReplica> {
    observed_machines
        .iter()
        .flat_map(MachineContainerObservationSnapshot::containers)
        .filter(|container| {
            !draining_machines.contains(&container.machine_id)
                && container.state.is_running()
                && container.identity.is_service_entry(
                    namespace_id,
                    &service.service_id,
                    &service.namespace_revision_entry_id(namespace_id),
                )
                && match policy {
                    ExistingReplicaPolicy::Promoted { .. } => true,
                    ExistingReplicaPolicy::RecoverInterrupted { operation_ids } => {
                        operation_ids.contains(&container.identity.operation_id)
                    }
                    ExistingReplicaPolicy::ExcludeUnpromoted => false,
                }
        })
        .map(|container| ExistingServiceReplica {
            machine_id: container.machine_id.clone(),
            container_id: container.container_id.clone(),
            creation_gate: match policy {
                ExistingReplicaPolicy::Promoted {
                    interrupted_operation_ids,
                } if interrupted_operation_ids.contains(&container.identity.operation_id) => {
                    ExistingReplicaCreationGate::RequiredAfterInterruption
                }
                ExistingReplicaPolicy::Promoted { .. } => {
                    ExistingReplicaCreationGate::AlreadyPassed
                }
                ExistingReplicaPolicy::RecoverInterrupted { .. } => {
                    ExistingReplicaCreationGate::RequiredAfterInterruption
                }
                ExistingReplicaPolicy::ExcludeUnpromoted => {
                    unreachable!("excluded replicas were filtered before mapping")
                }
            },
        })
        .collect()
}

fn cleanup_candidates(
    namespace_id: &NamespaceId,
    service: &DeployServiceSpec,
    observed_machines: &[MachineContainerObservationSnapshot],
    draining_machines: &[MachineId],
) -> Vec<ObservedCleanupCandidate> {
    observed_machines
        .iter()
        .flat_map(MachineContainerObservationSnapshot::containers)
        .filter(|container| {
            container.is_service()
                && container.identity.namespace_id == *namespace_id
                && container.identity.service_id == service.service_id
        })
        .map(|container| ObservedCleanupCandidate {
            target: DeployCleanupContainer {
                machine_id: container.machine_id.clone(),
                container_id: container.container_id.clone(),
                identity: container.identity.clone(),
            },
            state: container.state.clone(),
            created_at_unix_seconds: container.created_at_unix_seconds,
            resolved_image_identity: container
                .resolved_image_identity
                .as_ref()
                .and_then(|identity| crate::image::OciDigest::try_new(identity.clone()).ok()),
            image_reclamation_eligible: !container.state.is_running()
                || (!draining_machines.contains(&container.machine_id)
                    && container.identity.namespace_revision_entry_id
                        != service.namespace_revision_entry_id(namespace_id)),
        })
        .collect()
}

fn route_binding_commits(
    namespace_id: &NamespaceId,
    service: &DeployServiceSpec,
    occupied: &[RouteBindingState],
    new_route_binding_id: &mut impl FnMut(&RouteTarget) -> RouteBindingId,
) -> Result<Vec<RouteBindingState>, RouteBindingCommitError> {
    let mut bindings = Vec::<RouteBindingState>::new();
    for route in &service.routes {
        let Some(target) = route.target.concrete_target() else {
            continue;
        };
        if let Some(existing) = bindings.iter().find(|binding| binding.target == target) {
            return Err(RouteBindingCommitError::HostnameCollision {
                hostname: target.hostname,
                route_binding_id: existing.id.clone(),
            });
        }
        if let Some(existing) = occupied.iter().find(|binding| binding.target == target) {
            if existing.origin == RouteBindingOrigin::Declared
                && existing.namespace_id == *namespace_id
                && existing.service_id == service.service_id
            {
                bindings.push(RouteBindingState {
                    endpoint_port: route.endpoint_port,
                    ..existing.clone()
                });
                continue;
            }
            return Err(RouteBindingCommitError::HostnameCollision {
                hostname: target.hostname,
                route_binding_id: existing.id.clone(),
            });
        }
        bindings.push(RouteBindingState {
            id: new_route_binding_id(&target),
            namespace_id: namespace_id.clone(),
            target,
            endpoint_port: route.endpoint_port,
            service_id: service.service_id.clone(),
            origin: RouteBindingOrigin::Declared,
        });
    }
    Ok(bindings)
}

/// Validate every route mutation in one deploy against current cluster intent.
///
/// Automatic bindings are derived first because declared and automatic routes
/// share one hostname namespace. The returned bindings include identical
/// existing bindings, making this the same policy used by execution planning.
pub fn validate_deploy_route_bindings(
    request: &VolumeDeclaredDeployRequest,
    automatic_hostname_suffix: Option<&RouteHostname>,
    existing: &[RouteBindingState],
    mut new_route_binding_id: impl FnMut(&RouteTarget) -> RouteBindingId,
) -> Result<Vec<RouteBindingState>, DeployRouteBindingValidationError> {
    let mut occupied = existing.to_vec();
    let mut commits = Vec::new();
    let mut services = request.services().iter().collect::<Vec<_>>();
    services.sort_by(|left, right| left.service_id.cmp(&right.service_id));
    let duplicate_service_id = services.windows(2).find_map(|pair| {
        let [first, second] = pair else {
            return None;
        };
        (first.service_id == second.service_id).then(|| first.service_id.clone())
    });
    if let Some(service_id) = duplicate_service_id {
        return Err(DeployRouteBindingValidationError::DuplicateServiceId { service_id });
    }
    for service in services {
        let automatic = auto_hostname_route_binding_commits(
            request.namespace_id(),
            service,
            automatic_hostname_suffix,
            &occupied,
            &mut new_route_binding_id,
        )?;
        occupied.extend(automatic.iter().cloned());
        commits.extend(automatic);

        let declared = route_binding_commits(
            request.namespace_id(),
            service,
            &occupied,
            &mut new_route_binding_id,
        )?;
        occupied.extend(declared.iter().cloned());
        commits.extend(declared);
    }
    Ok(commits)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployRouteBindingValidationError {
    #[error("service {} is declared more than once", .service_id.as_str())]
    DuplicateServiceId { service_id: ServiceId },
    #[error(transparent)]
    Automatic(#[from] AutoHostnameRouteBindingError),
    #[error(transparent)]
    Declared(#[from] RouteBindingCommitError),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RouteBindingCommitError {
    #[error(
        "declared hostname {} collides with route binding {}",
        .hostname.as_str(),
        .route_binding_id.as_str()
    )]
    HostnameCollision {
        hostname: RouteHostname,
        route_binding_id: RouteBindingId,
    },
}

/// Derive exact automatic hostnames and reuse only identical requests.
pub fn auto_hostname_route_binding_commits(
    namespace_id: &NamespaceId,
    service: &DeployServiceSpec,
    configured_suffix: Option<&RouteHostname>,
    occupied: &[RouteBindingState],
    mut new_route_binding_id: impl FnMut(&RouteTarget) -> RouteBindingId,
) -> Result<Vec<RouteBindingState>, AutoHostnameRouteBindingError> {
    let mut bindings = Vec::<RouteBindingState>::new();

    for route in &service.routes {
        let DeployRouteTarget::AutoHostname { label } = &route.target else {
            continue;
        };
        let Some(suffix) = configured_suffix else {
            return Err(AutoHostnameRouteBindingError::AutomaticHostnamesDisabled);
        };
        let hostname = RouteHostname::try_new(format!("{}.{}", label.as_str(), suffix.as_str()))
            .map_err(|error| AutoHostnameRouteBindingError::InvalidHostname {
                message: error.to_string(),
            })?;
        let target = RouteTarget::new(hostname);
        if let Some(existing) = bindings.iter().find(|binding| binding.target == target) {
            return Err(AutoHostnameRouteBindingError::HostnameCollision {
                hostname: target.hostname,
                route_binding_id: existing.id.clone(),
            });
        }
        if let Some(existing) = occupied.iter().find(|binding| binding.target == target) {
            if existing.origin == RouteBindingOrigin::Automatic
                && existing.namespace_id == *namespace_id
                && existing.service_id == service.service_id
            {
                bindings.push(RouteBindingState {
                    endpoint_port: route.endpoint_port,
                    ..existing.clone()
                });
                continue;
            }
            return Err(AutoHostnameRouteBindingError::HostnameCollision {
                hostname: target.hostname,
                route_binding_id: existing.id.clone(),
            });
        }
        bindings.push(RouteBindingState {
            id: new_route_binding_id(&target),
            namespace_id: namespace_id.clone(),
            target,
            endpoint_port: route.endpoint_port,
            service_id: service.service_id.clone(),
            origin: RouteBindingOrigin::Automatic,
        });
    }
    Ok(bindings)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AutoHostnameRouteBindingError {
    #[error("automatic hostnames are disabled")]
    AutomaticHostnamesDisabled,
    #[error("derived automatic hostname is invalid: {message}")]
    InvalidHostname { message: String },
    #[error(
        "automatic hostname {} collides with route binding {}",
        .hostname.as_str(),
        .route_binding_id.as_str()
    )]
    HostnameCollision {
        hostname: RouteHostname,
        route_binding_id: RouteBindingId,
    },
}

pub fn plan_namespace_deploy(
    request: &VolumeDeclaredDeployRequest,
    services: Vec<DeployPlanningInput>,
    cleanup_containers: Vec<DeployCleanupContainer>,
) -> Result<DeployPlan, DeployPlanError> {
    let phases = dependency_ordered_phases(request, services)?;
    let mut phase_plans = Vec::new();
    let mut cleanup_containers = cleanup_containers;
    let mut volume_pin_commits = Vec::new();
    let mut image_reclamations = Vec::new();
    let mut working_volume_pins = phases
        .iter()
        .flatten()
        .flat_map(|input| &input.volume_pins)
        .cloned()
        .collect::<Vec<_>>();
    for phase in phases {
        let mut services = Vec::new();
        for mut input in phase {
            input.volume_pins = working_volume_pins.clone();
            let plan = plan_deploy_service(request, input)?;
            working_volume_pins.extend(plan.volume_pin_commits.clone());
            volume_pin_commits.extend(plan.volume_pin_commits);
            image_reclamations.extend(plan.image_reclamations);
            services.push(DeployServicePlan {
                service_id: plan.service_id,
                steps: plan.steps,
                pre_start: plan.pre_start,
            });
            cleanup_containers.extend(plan.cleanup_containers);
        }
        phase_plans.push(DeployPhasePlan { services });
    }

    Ok(DeployPlan {
        namespace_id: request.namespace_id().clone(),
        namespace_revision_id: request.namespace_revision_id(),
        phases: phase_plans,
        volume_pin_commits,
        cleanup_containers,
        image_reclamations,
    })
}

fn dependency_ordered_phases(
    request: &VolumeDeclaredDeployRequest,
    services: Vec<DeployPlanningInput>,
) -> Result<Vec<Vec<DeployPlanningInput>>, DeployPlanError> {
    let service_healthchecks = services
        .iter()
        .map(|input| {
            let service = deploy_service(request, &input.service_id)?;
            Ok((
                input.service_id.clone(),
                service
                    .runtime
                    .healthcheck
                    .as_ref()
                    .is_some_and(ContainerHealthcheck::reports_docker_health),
            ))
        })
        .collect::<Result<BTreeMap<_, _>, DeployPlanError>>()?;
    let mut dependents = BTreeMap::<ServiceId, Vec<ServiceId>>::new();
    let mut dependency_counts = BTreeMap::<ServiceId, usize>::new();

    for input in &services {
        let service = deploy_service(request, &input.service_id)?;
        let mut dependencies = BTreeSet::new();
        for dependency in &service.depends_on {
            let Some(has_healthcheck) = service_healthchecks.get(&dependency.service_id) else {
                return Err(DeployPlanError::UnknownServiceDependency {
                    service_id: input.service_id.clone(),
                    dependency: dependency.service_id.clone(),
                });
            };
            if dependency.condition == DependencyCondition::Healthy && !has_healthcheck {
                return Err(DeployPlanError::HealthyDependencyWithoutHealthcheck {
                    service_id: input.service_id.clone(),
                    dependency: dependency.service_id.clone(),
                });
            }
            dependencies.insert(dependency.service_id.clone());
        }
        dependency_counts.insert(input.service_id.clone(), dependencies.len());
        for dependency in dependencies {
            dependents
                .entry(dependency)
                .or_default()
                .push(input.service_id.clone());
        }
    }

    let mut ready = dependency_counts
        .iter()
        .filter_map(|(service_id, count)| (*count == 0).then_some(service_id.clone()))
        .collect::<BTreeSet<_>>();
    let mut phase_ids = Vec::new();
    let mut placed = 0;
    while !ready.is_empty() {
        let current = std::mem::take(&mut ready);
        placed += current.len();
        for service_id in &current {
            if let Some(service_dependents) = dependents.get(service_id) {
                for dependent in service_dependents {
                    let Some(dependency_count) = dependency_counts.get_mut(dependent) else {
                        continue;
                    };
                    *dependency_count -= 1;
                    if *dependency_count == 0 {
                        ready.insert(dependent.clone());
                    }
                }
            }
        }
        phase_ids.push(current);
    }

    if placed != services.len() {
        let mut service_ids = dependency_counts
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(service_id, _)| service_id.clone())
            .collect::<Vec<_>>();
        service_ids.sort();
        return Err(DeployPlanError::ServiceDependencyCycle { service_ids });
    }

    let mut services = services
        .into_iter()
        .map(|input| (input.service_id.clone(), input))
        .collect::<BTreeMap<_, _>>();
    Ok(phase_ids
        .into_iter()
        .map(|ids| {
            ids.into_iter()
                .filter_map(|service_id| services.remove(&service_id))
                .collect()
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploySingleServicePlan {
    pub service_id: ServiceId,
    pub namespace_revision_id: NamespaceRevisionId,
    pub steps: Vec<DeployPlanStep>,
    pub pre_start: Option<PreStartHookStep>,
    pub volume_pin_commits: Vec<VolumePinState>,
    pub cleanup_containers: Vec<DeployCleanupContainer>,
    pub image_reclamations: Vec<DeployImageReclamation>,
}

fn plan_deploy_service(
    request: &VolumeDeclaredDeployRequest,
    input: DeployPlanningInput,
) -> Result<DeploySingleServicePlan, DeployPlanError> {
    let service = deploy_service(request, &input.service_id)?;
    let volume_placement = volume_placement(
        request,
        service,
        &input.eligible_machines,
        &input.volume_pins,
    )?;
    let target_replicas = usize::from(service.replicas.get());
    let mut existing_replicas = input.existing_replicas;
    if let Some(machine_id) = &volume_placement.machine_id {
        existing_replicas.retain(|replica| &replica.machine_id == machine_id);
    }
    existing_replicas.sort_by(|left, right| {
        left.machine_id
            .cmp(&right.machine_id)
            .then_with(|| left.container_id.cmp(&right.container_id))
    });
    existing_replicas.dedup_by(|left, right| {
        left.machine_id == right.machine_id && left.container_id == right.container_id
    });
    let mut steps = existing_replicas
        .into_iter()
        .take(target_replicas)
        .enumerate()
        .map(|(index, replica)| DeployPlanStep::UseExistingContainer {
            machine_id: replica.machine_id,
            container_id: replica.container_id,
            slot: ReplicaSlot((index + 1) as u16),
        })
        .collect::<Vec<_>>();
    let missing_replicas = target_replicas.saturating_sub(steps.len());
    if missing_replicas > 0 && input.eligible_machines.is_empty() {
        return Err(DeployPlanError::NoEligibleMachines);
    }

    let existing_replicas = steps.len();
    let run_machines = volume_placement
        .machine_id
        .as_ref()
        .map(|machine_id| vec![machine_id.clone()])
        .unwrap_or(input.eligible_machines);
    steps.extend(
        run_machines
            .iter()
            .cycle()
            .take(missing_replicas)
            .enumerate()
            .map(|(index, machine_id)| {
                let slot = ReplicaSlot((existing_replicas + index + 1) as u16);
                DeployPlanStep::RunContainer {
                    machine_id: machine_id.clone(),
                    slot,
                }
            }),
    );
    let selected_containers = steps
        .iter()
        .filter_map(|step| match step {
            DeployPlanStep::UseExistingContainer { container_id, .. } => Some(container_id),
            DeployPlanStep::RunContainer { .. } => None,
        })
        .collect::<Vec<_>>();
    let (mut cleanup_containers, image_reclamations) = super::retention::plan_cleanup(
        input.cleanup_candidates,
        &selected_containers,
        service.keep,
    );
    cleanup_containers.sort_by(|left, right| {
        left.machine_id
            .cmp(&right.machine_id)
            .then_with(|| left.container_id.cmp(&right.container_id))
    });
    cleanup_containers.dedup_by(|left, right| {
        left.machine_id == right.machine_id && left.container_id == right.container_id
    });
    let pre_start = service.pre_start.as_ref().and_then(|_| {
        steps.iter().find_map(|step| match step {
            DeployPlanStep::RunContainer { machine_id, .. } => Some(PreStartHookStep {
                machine_id: machine_id.clone(),
            }),
            DeployPlanStep::UseExistingContainer { .. } => None,
        })
    });

    Ok(DeploySingleServicePlan {
        service_id: input.service_id,
        namespace_revision_id: request.namespace_revision_id(),
        steps,
        pre_start,
        volume_pin_commits: volume_placement.commits,
        cleanup_containers,
        image_reclamations,
    })
}

fn deploy_service<'a>(
    request: &'a VolumeDeclaredDeployRequest,
    service_id: &ServiceId,
) -> Result<&'a DeployServiceSpec, DeployPlanError> {
    request
        .service(service_id)
        .ok_or_else(|| DeployPlanError::UnknownService {
            service_id: service_id.clone(),
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VolumePlacement {
    machine_id: Option<MachineId>,
    commits: Vec<VolumePinState>,
}

fn volume_placement(
    request: &VolumeDeclaredDeployRequest,
    service: &DeployServiceSpec,
    eligible_machines: &[MachineId],
    volume_pins: &[VolumePinState],
) -> Result<VolumePlacement, DeployPlanError> {
    if service.runtime.volume_mounts.is_empty() {
        return Ok(VolumePlacement {
            machine_id: None,
            commits: Vec::new(),
        });
    }

    let mut pinned_machines = Vec::new();
    let mut new_plain_volumes = Vec::new();
    for mount in &service.runtime.volume_mounts {
        let volume_name = &mount.volume_name;
        let spec = request
            .request()
            .volumes
            .get(volume_name)
            .expect("volume-declared deploy validates every mounted volume declaration");
        match volume_pins.iter().find(|pin| {
            pin.namespace_id() == request.namespace_id() && pin.volume_name() == volume_name
        }) {
            Some(pin) => match (spec, pin.kind()) {
                (VolumeSpec::Plain, crate::intent::VolumeKind::Plain) => {
                    pinned_machines.push(pin.machine_id().clone());
                }
                (
                    VolumeSpec::Provisioned { max_size_bytes },
                    crate::intent::VolumeKind::Provisioned {
                        max_size_bytes: pinned_size,
                        ..
                    },
                ) if max_size_bytes.get() >= pinned_size.get() => {
                    pinned_machines.push(pin.machine_id().clone());
                }
                (
                    VolumeSpec::Provisioned { max_size_bytes },
                    crate::intent::VolumeKind::Provisioned {
                        max_size_bytes: pinned_size,
                        ..
                    },
                ) => {
                    return Err(DeployPlanError::ProvisionedVolumeShrink {
                        service_id: service.service_id.clone(),
                        volume_name: volume_name.clone(),
                        declared_max_size_bytes: *max_size_bytes,
                        pinned_max_size_bytes: *pinned_size,
                    });
                }
                (VolumeSpec::Plain, crate::intent::VolumeKind::Provisioned { .. })
                | (VolumeSpec::Provisioned { .. }, crate::intent::VolumeKind::Plain) => {
                    return Err(DeployPlanError::VolumePinIncompatible {
                        service_id: service.service_id.clone(),
                        volume_name: volume_name.clone(),
                        declaration: spec.clone(),
                        pin_kind: pin.kind().clone(),
                    });
                }
            },
            None if matches!(spec, VolumeSpec::Plain) => {
                new_plain_volumes.push(volume_name.clone());
            }
            None => {
                return Err(DeployPlanError::ProvisionedVolumeRequiresProvisioning {
                    service_id: service.service_id.clone(),
                    volume_name: volume_name.clone(),
                });
            }
        }
    }
    new_plain_volumes.sort();
    new_plain_volumes.dedup();
    pinned_machines.sort();
    pinned_machines.dedup();
    match pinned_machines.as_slice() {
        [machine_id] => {
            if !eligible_machines.contains(machine_id) {
                return Err(DeployPlanError::NoEligibleMachines);
            }
            Ok(VolumePlacement {
                machine_id: Some(machine_id.clone()),
                commits: plain_volume_pin_commits(
                    request.namespace_id(),
                    machine_id,
                    &new_plain_volumes,
                ),
            })
        }
        [] => {
            let Some(machine_id) = eligible_machines.first() else {
                return Err(DeployPlanError::NoEligibleMachines);
            };
            Ok(VolumePlacement {
                machine_id: Some(machine_id.clone()),
                commits: plain_volume_pin_commits(
                    request.namespace_id(),
                    machine_id,
                    &new_plain_volumes,
                ),
            })
        }
        _ => Err(DeployPlanError::ConflictingVolumePins {
            service_id: service.service_id.clone(),
            machines: pinned_machines,
        }),
    }
}

fn plain_volume_pin_commits(
    namespace_id: &NamespaceId,
    machine_id: &MachineId,
    volume_names: &[VolumeName],
) -> Vec<VolumePinState> {
    volume_names
        .iter()
        .map(|volume_name| {
            VolumePinState::plain(
                namespace_id.clone(),
                volume_name.clone(),
                machine_id.clone(),
            )
        })
        .collect()
}
