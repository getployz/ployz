//! Deploy preparation and route admission.

use super::*;

pub fn prepare_deploy(
    input: DeployPreparationInput<'_>,
    mut new_route_binding_id: impl FnMut(&RouteTarget) -> RouteBindingId,
) -> Result<PreparedDeploy, DeployPreparationError> {
    let Some(service) = input.request.service(&input.service_id) else {
        return Err(DeployPreparationError::UnknownService {
            service_id: input.service_id,
        });
    };
    let prepared = prepare_planning_service(
        PlanningPreparationInput {
            target: DeployPlanningTarget::Deploy(input.request),
            service: DeployPlanningService::from_deploy(service),
            occupied_route_bindings: input.occupied_route_bindings,
            eligible_machines: input.eligible_machines,
            machine_platforms: input.machine_platforms,
            draining_machines: input.draining_machines,
            observed_machines: input.observed_machines,
            existing_replica_policy: input.existing_replica_policy,
        },
        &mut new_route_binding_id,
    )?;

    Ok(PreparedDeploy {
        service: service.clone(),
        route_commits: prepared.route_commits,
        eligible_machines: prepared.eligible_machines,
        unusable_machines: prepared.unusable_machines,
        existing_replicas: prepared.existing_replicas,
        cleanup_candidates: prepared.cleanup_candidates,
    })
}

pub fn prepare_deploy_preview(
    input: DeployPreviewPreparationInput<'_>,
    mut new_route_binding_id: impl FnMut(&RouteTarget) -> RouteBindingId,
) -> Result<PreparedDeployPreview, DeployPreparationError> {
    let Some(service) = input.target.service(&input.service_id) else {
        return Err(DeployPreparationError::UnknownService {
            service_id: input.service_id,
        });
    };
    let prepared = prepare_planning_service(
        PlanningPreparationInput {
            target: DeployPlanningTarget::Preview(input.target),
            service: DeployPlanningService::from_preview(service),
            occupied_route_bindings: input.occupied_route_bindings,
            eligible_machines: input.eligible_machines,
            machine_platforms: input.machine_platforms,
            draining_machines: input.draining_machines,
            observed_machines: input.observed_machines,
            existing_replica_policy: input.existing_replica_policy,
        },
        &mut new_route_binding_id,
    )?;
    Ok(PreparedDeployPreview {
        service: service.clone(),
        route_commits: prepared.route_commits,
        eligible_machines: prepared.eligible_machines,
        unusable_machines: prepared.unusable_machines,
        existing_replicas: prepared.existing_replicas,
        cleanup_candidates: prepared.cleanup_candidates,
    })
}

struct PreparedPlanningService {
    route_commits: Vec<RouteBindingState>,
    eligible_machines: Vec<MachineId>,
    unusable_machines: Vec<crate::operation::UnusableMachine>,
    existing_replicas: Vec<ExistingServiceReplica>,
    cleanup_candidates: Vec<ObservedCleanupCandidate>,
}

struct PlanningPreparationInput<'a> {
    target: DeployPlanningTarget<'a>,
    service: DeployPlanningService<'a>,
    occupied_route_bindings: Vec<RouteBindingState>,
    eligible_machines: Vec<MachineId>,
    machine_platforms: BTreeMap<MachineId, OciPlatform>,
    draining_machines: Vec<MachineId>,
    observed_machines: Vec<MachineContainerObservationSnapshot>,
    existing_replica_policy: ExistingReplicaPolicy,
}

fn prepare_planning_service(
    input: PlanningPreparationInput<'_>,
    new_route_binding_id: &mut impl FnMut(&RouteTarget) -> RouteBindingId,
) -> Result<PreparedPlanningService, DeployPreparationError> {
    let PlanningPreparationInput {
        target,
        service,
        occupied_route_bindings,
        eligible_machines,
        machine_platforms,
        draining_machines,
        observed_machines,
        existing_replica_policy,
    } = input;
    let route_commits = planning_route_binding_commits(
        target.namespace_id(),
        service,
        &occupied_route_bindings,
        new_route_binding_id,
    )?;
    let mut existing_replicas = existing_replicas(
        target.namespace_id(),
        service,
        &observed_machines,
        &draining_machines,
        &existing_replica_policy,
    );
    let cleanup_candidates = cleanup_candidates(target.namespace_id(), service, &observed_machines);
    let mut platform_candidates = eligible_machines.clone();
    platform_candidates.extend(
        existing_replicas
            .iter()
            .map(|replica| replica.machine_id.clone()),
    );
    platform_candidates.sort();
    platform_candidates.dedup();
    let (platform_usable_machines, unusable_machines) = if let Some((_, image_source)) =
        service.concrete_image()
    {
        classify_image_platform_usability(&platform_candidates, &machine_platforms, image_source)
    } else {
        (platform_candidates, Vec::new())
    };
    let eligible_machines = eligible_machines
        .into_iter()
        .filter(|machine_id| platform_usable_machines.contains(machine_id))
        .collect();
    existing_replicas.retain(|replica| platform_usable_machines.contains(&replica.machine_id));

    Ok(PreparedPlanningService {
        route_commits,
        eligible_machines,
        unusable_machines,
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
    service: DeployPlanningService<'_>,
    observed_machines: &[MachineContainerObservationSnapshot],
    draining_machines: &[MachineId],
    policy: &ExistingReplicaPolicy,
) -> Vec<ExistingServiceReplica> {
    let Some(namespace_revision_entry_id) = service.namespace_revision_entry_id(namespace_id)
    else {
        return Vec::new();
    };
    observed_machines
        .iter()
        .flat_map(MachineContainerObservationSnapshot::containers)
        .filter(|container| {
            !draining_machines.contains(&container.machine_id)
                && container.state.is_running()
                && container.identity.is_service_entry(
                    namespace_id,
                    service.service_id(),
                    &namespace_revision_entry_id,
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
    service: DeployPlanningService<'_>,
    observed_machines: &[MachineContainerObservationSnapshot],
) -> Vec<ObservedCleanupCandidate> {
    observed_machines
        .iter()
        .flat_map(MachineContainerObservationSnapshot::containers)
        .filter(|container| {
            container.is_service()
                && container.identity.namespace_id == *namespace_id
                && container.identity.service_id == *service.service_id()
        })
        .map(|container| ObservedCleanupCandidate {
            target: DeployCleanupContainer {
                machine_id: container.machine_id.clone(),
                container_id: container.container_id.clone(),
                identity: container.identity.clone(),
            },
            state: container.state.clone(),
            created_at_unix_seconds: container.created_at_unix_seconds,
            observed_image_identity: container.resolved_image_identity.clone(),
        })
        .collect()
}

fn planning_route_binding_commits(
    namespace_id: &NamespaceId,
    service: DeployPlanningService<'_>,
    occupied: &[RouteBindingState],
    new_route_binding_id: &mut impl FnMut(&RouteTarget) -> RouteBindingId,
) -> Result<Vec<RouteBindingState>, RouteBindingCommitError> {
    let mut bindings = Vec::<RouteBindingState>::new();
    for route in service.routes() {
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
                && existing.service_id == *service.service_id()
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
            service_id: service.service_id().clone(),
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
    new_route_binding_id: impl FnMut(&RouteTarget) -> RouteBindingId,
) -> Result<Vec<RouteBindingState>, DeployRouteBindingValidationError> {
    validate_planning_route_bindings(
        DeployPlanningTarget::Deploy(request),
        automatic_hostname_suffix,
        existing,
        new_route_binding_id,
    )
}

pub fn validate_deploy_preview_route_bindings(
    target: &VolumeDeclaredDeployPreviewTarget,
    automatic_hostname_suffix: Option<&RouteHostname>,
    existing: &[RouteBindingState],
    new_route_binding_id: impl FnMut(&RouteTarget) -> RouteBindingId,
) -> Result<Vec<RouteBindingState>, DeployRouteBindingValidationError> {
    validate_planning_route_bindings(
        DeployPlanningTarget::Preview(target),
        automatic_hostname_suffix,
        existing,
        new_route_binding_id,
    )
}

fn validate_planning_route_bindings(
    target: DeployPlanningTarget<'_>,
    automatic_hostname_suffix: Option<&RouteHostname>,
    existing: &[RouteBindingState],
    mut new_route_binding_id: impl FnMut(&RouteTarget) -> RouteBindingId,
) -> Result<Vec<RouteBindingState>, DeployRouteBindingValidationError> {
    let mut occupied = existing.to_vec();
    let mut commits = Vec::new();
    let mut services = target.services();
    services.sort_by(|left, right| left.service_id().cmp(right.service_id()));
    let duplicate_service_id = services.windows(2).find_map(|pair| {
        let [first, second] = pair else {
            return None;
        };
        (first.service_id() == second.service_id()).then(|| first.service_id().clone())
    });
    if let Some(service_id) = duplicate_service_id {
        return Err(DeployRouteBindingValidationError::DuplicateServiceId { service_id });
    }
    for service in services {
        let automatic = planning_auto_hostname_route_binding_commits(
            target.namespace_id(),
            service,
            automatic_hostname_suffix,
            &occupied,
            &mut new_route_binding_id,
        )?;
        occupied.extend(automatic.iter().cloned());
        commits.extend(automatic);

        let declared = planning_route_binding_commits(
            target.namespace_id(),
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
    new_route_binding_id: impl FnMut(&RouteTarget) -> RouteBindingId,
) -> Result<Vec<RouteBindingState>, AutoHostnameRouteBindingError> {
    planning_auto_hostname_route_binding_commits(
        namespace_id,
        DeployPlanningService::from_deploy(service),
        configured_suffix,
        occupied,
        new_route_binding_id,
    )
}

pub fn auto_hostname_preview_route_binding_commits(
    namespace_id: &NamespaceId,
    service: &DeployPreviewService,
    configured_suffix: Option<&RouteHostname>,
    occupied: &[RouteBindingState],
    new_route_binding_id: impl FnMut(&RouteTarget) -> RouteBindingId,
) -> Result<Vec<RouteBindingState>, AutoHostnameRouteBindingError> {
    planning_auto_hostname_route_binding_commits(
        namespace_id,
        DeployPlanningService::from_preview(service),
        configured_suffix,
        occupied,
        new_route_binding_id,
    )
}

fn planning_auto_hostname_route_binding_commits(
    namespace_id: &NamespaceId,
    service: DeployPlanningService<'_>,
    configured_suffix: Option<&RouteHostname>,
    occupied: &[RouteBindingState],
    mut new_route_binding_id: impl FnMut(&RouteTarget) -> RouteBindingId,
) -> Result<Vec<RouteBindingState>, AutoHostnameRouteBindingError> {
    let mut bindings = Vec::<RouteBindingState>::new();

    for route in service.routes() {
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
                && existing.service_id == *service.service_id()
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
            service_id: service.service_id().clone(),
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
