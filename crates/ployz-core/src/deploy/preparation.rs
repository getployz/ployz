//! Deploy preparation and route admission.

use super::*;

pub fn prepare_deploy(
    input: DeployPreparationInput<'_>,
) -> Result<PreparedDeploy, DeployPreparationError> {
    let Some(service) = input.target.service(&input.service_id) else {
        return Err(DeployPreparationError::UnknownService {
            service_id: input.service_id,
        });
    };
    let DeployPreparationInput {
        target,
        service_id: _,
        occupied_route_bindings,
        eligible_machines,
        machine_platforms,
        draining_machines,
        observed_machines,
        existing_replica_policy,
    } = input;
    let route_additions = declared_route_binding_additions(
        target.namespace_id(),
        service,
        &occupied_route_bindings,
        &[],
    )?;
    let mut existing_replicas = existing_replicas(
        target,
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

    Ok(PreparedDeploy {
        route_additions,
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
    target: &DeployPlanningTarget,
    service: &DeployPlanningService,
    observed_machines: &[MachineContainerObservationSnapshot],
    draining_machines: &[MachineId],
    policy: &ExistingReplicaPolicy,
) -> Vec<ExistingServiceReplica> {
    let Some(namespace_revision_entry_id) = service.namespace_revision_entry_id_for_target(target)
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
                    target.namespace_id(),
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
    service: &DeployPlanningService,
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
            named_volume_names: container.named_volume_names.clone(),
            created_at_unix_seconds: container.created_at_unix_seconds,
            observed_image_identity: container.resolved_image_identity.clone(),
        })
        .collect()
}

/// Validate every prospective route addition against current cluster intent.
pub fn validate_deploy_route_bindings(
    target: &DeployPlanningTarget,
    automatic_hostname_suffix: Option<&RouteHostname>,
    existing: &[RouteBindingState],
) -> Result<Vec<DeployRouteBindingAddition>, DeployRouteBindingValidationError> {
    let mut additions = Vec::new();
    let mut services = target.services().iter().collect::<Vec<_>>();
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
        let automatic = auto_hostname_route_binding_additions(
            target.namespace_id(),
            service,
            automatic_hostname_suffix,
            existing,
            &additions,
        )?;
        additions.extend(automatic);

        let declared =
            declared_route_binding_additions(target.namespace_id(), service, existing, &additions)?;
        additions.extend(declared);
    }
    Ok(additions)
}

/// Authoritative-only conversion from prospective additions to durable state.
#[must_use]
pub fn commit_deploy_route_bindings(
    additions: Vec<DeployRouteBindingAddition>,
    existing: &[RouteBindingState],
    mut new_route_binding_id: impl FnMut(&RouteTarget) -> RouteBindingId,
) -> Vec<RouteBindingState> {
    additions
        .into_iter()
        .map(|addition| {
            let id = existing
                .iter()
                .find(|binding| binding.target == addition.target)
                .map_or_else(
                    || new_route_binding_id(&addition.target),
                    |binding| binding.id.clone(),
                );
            RouteBindingState {
                id,
                namespace_id: addition.namespace_id,
                target: addition.target,
                endpoint_port: addition.endpoint_port,
                service_id: addition.service_id,
                origin: addition.origin,
            }
        })
        .collect()
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
    #[error("declared hostname {} is declared more than once", .hostname.as_str())]
    DuplicateHostname { hostname: RouteHostname },
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

fn declared_route_binding_additions(
    namespace_id: &NamespaceId,
    service: &DeployPlanningService,
    existing: &[RouteBindingState],
    planned: &[DeployRouteBindingAddition],
) -> Result<Vec<DeployRouteBindingAddition>, RouteBindingCommitError> {
    let mut additions = Vec::new();
    for route in service.routes() {
        let Some(target) = route.target.concrete_target() else {
            continue;
        };
        if planned
            .iter()
            .chain(&additions)
            .any(|addition| addition.target == target)
        {
            return Err(RouteBindingCommitError::DuplicateHostname {
                hostname: target.hostname,
            });
        }
        if let Some(binding) = existing.iter().find(|binding| binding.target == target)
            && (binding.origin != RouteBindingOrigin::Declared
                || binding.namespace_id != *namespace_id
                || binding.service_id != *service.service_id())
        {
            return Err(RouteBindingCommitError::HostnameCollision {
                hostname: target.hostname,
                route_binding_id: binding.id.clone(),
            });
        }
        additions.push(DeployRouteBindingAddition {
            namespace_id: namespace_id.clone(),
            target,
            endpoint_port: route.endpoint_port,
            service_id: service.service_id().clone(),
            origin: RouteBindingOrigin::Declared,
        });
    }
    Ok(additions)
}

fn auto_hostname_route_binding_additions(
    namespace_id: &NamespaceId,
    service: &DeployPlanningService,
    configured_suffix: Option<&RouteHostname>,
    existing: &[RouteBindingState],
    planned: &[DeployRouteBindingAddition],
) -> Result<Vec<DeployRouteBindingAddition>, AutoHostnameRouteBindingError> {
    let mut additions = Vec::new();

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
        if planned
            .iter()
            .chain(&additions)
            .any(|addition| addition.target == target)
        {
            return Err(AutoHostnameRouteBindingError::DuplicateHostname {
                hostname: target.hostname,
            });
        }
        if let Some(binding) = existing.iter().find(|binding| binding.target == target)
            && (binding.origin != RouteBindingOrigin::Automatic
                || binding.namespace_id != *namespace_id
                || binding.service_id != *service.service_id())
        {
            return Err(AutoHostnameRouteBindingError::HostnameCollision {
                hostname: target.hostname,
                route_binding_id: binding.id.clone(),
            });
        }
        additions.push(DeployRouteBindingAddition {
            namespace_id: namespace_id.clone(),
            target,
            endpoint_port: route.endpoint_port,
            service_id: service.service_id().clone(),
            origin: RouteBindingOrigin::Automatic,
        });
    }
    Ok(additions)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AutoHostnameRouteBindingError {
    #[error("automatic hostnames are disabled")]
    AutomaticHostnamesDisabled,
    #[error("derived automatic hostname is invalid: {message}")]
    InvalidHostname { message: String },
    #[error("automatic hostname {} is declared more than once", .hostname.as_str())]
    DuplicateHostname { hostname: RouteHostname },
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
