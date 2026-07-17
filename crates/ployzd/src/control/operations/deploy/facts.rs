//! Load deploy execution facts from core intent and fresh machine facts RPCs.

use crate::certificate::gateway_certificate_targets;
use crate::control::intent::ingress_intent::{
    IngressIntentStore, IngressProjectionStore, PloyzDnsTargetAllocation, PloyzDnsTargetStore,
};
use crate::control::intent::namespace_intent::NamespaceIntentStore;
use crate::control::intent::service::NatsIntentReader;
use crate::control::role_client::machine::{
    NatsMachineFactsReader, read_machine_placement_facts,
    read_machine_placement_facts_with_gather_timeout,
};
use crate::control::role_client::machine_convergence::gather_dataplane_statuses;
use ployz_core::deploy::{
    AutoHostnameRouteBindingError, DeployPlanningTarget, DeployRequest,
    DeployRouteBindingValidationError, DeployRouteTarget,
};
use ployz_core::ids::MachineId;
use ployz_core::ingress::{AutomaticHostnameConfiguration, IngressConfiguration};
use ployz_core::intent::ActiveMachineState;
use ployz_core::intent::IntentSnapshot;
use ployz_core::machine::MachineLifecycle;
use ployz_core::network::{DataplaneMember, DataplaneProjection};
use ployz_core::operation::RouteHostname;

use std::time::Duration;

use super::placement::classify_machine_usability;
use super::preparation::namespace_cleanup_candidates;
use super::{AutomaticHostnameMode, DeployExecutionFacts};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlacementFactsGatherPolicy {
    CompleteAll,
    OverallDeadline(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeployFactGatherPolicy {
    step_timeout: Duration,
    placement_facts: PlacementFactsGatherPolicy,
}

pub async fn load_deploy_execution_facts_from_nats(
    request: &DeployRequest,
    intent_reader: &NatsIntentReader,
    facts_reader: &NatsMachineFactsReader,
    target_store: &PloyzDnsTargetStore,
    projection_store: &IngressProjectionStore,
    step_timeout: Duration,
) -> Result<DeployExecutionFacts, DeployFactLoadError> {
    let target = DeployPlanningTarget::try_from_deploy(request).map_err(|error| {
        DeployFactLoadError::InvalidStoredTarget {
            message: error.to_string(),
        }
    })?;
    load_planning_facts_from_nats(
        &target,
        intent_reader,
        facts_reader,
        target_store,
        projection_store,
        step_timeout,
        PlacementFactsGatherPolicy::CompleteAll,
    )
    .await
}

pub(super) async fn load_planning_facts_from_nats(
    target: &DeployPlanningTarget,
    intent_reader: &NatsIntentReader,
    facts_reader: &NatsMachineFactsReader,
    target_store: &PloyzDnsTargetStore,
    projection_store: &IngressProjectionStore,
    step_timeout: Duration,
    placement_facts_gather: PlacementFactsGatherPolicy,
) -> Result<DeployExecutionFacts, DeployFactLoadError> {
    let intent = read_intent(intent_reader).await?;
    let allocation = if has_auto_hostname_service(target)
        && matches!(
            &intent.automatic_hostname_configuration,
            AutomaticHostnameConfiguration::Ployz
        ) {
        target_store
            .load_allocation()
            .await
            .map_err(|error| DeployFactLoadError::IngressState {
                message: error.to_string(),
            })?
    } else {
        None
    };
    let automatic_hostname_mode = resolve_automatic_hostname_mode(
        target,
        Some(&intent.automatic_hostname_configuration),
        allocation.as_ref(),
    )?;
    let publishable_gateway_ids = if automatic_hostname_mode.is_ployz() {
        projection_store
            .load_publishable_gateway_ids()
            .await
            .map_err(|error| DeployFactLoadError::IngressState {
                message: error.to_string(),
            })?
    } else {
        Vec::new()
    };
    let projection = intent.dataplane_projection.clone();
    deploy_execution_facts(
        target,
        facts_reader,
        intent,
        projection,
        automatic_hostname_mode,
        publishable_gateway_ids,
        DeployFactGatherPolicy {
            step_timeout,
            placement_facts: placement_facts_gather,
        },
    )
    .await
}

fn resolve_automatic_hostname_mode(
    target: &DeployPlanningTarget,
    configuration: Option<&AutomaticHostnameConfiguration>,
    allocation: Option<&PloyzDnsTargetAllocation>,
) -> Result<AutomaticHostnameMode, DeployFactLoadError> {
    if !has_auto_hostname_service(target) {
        return Ok(AutomaticHostnameMode::Disabled);
    }
    let Some(configuration) = configuration else {
        return Err(DeployFactLoadError::IngressUnavailable {
            message: "ingress is not configured".to_owned(),
        });
    };
    match configuration {
        AutomaticHostnameConfiguration::Disabled => {
            Err(DeployFactLoadError::InvalidRouteBindings {
                failure: DeployRouteBindingValidationError::Automatic(
                    AutoHostnameRouteBindingError::AutomaticHostnamesDisabled,
                ),
            })
        }
        AutomaticHostnameConfiguration::Custom { suffix } => Ok(AutomaticHostnameMode::Custom {
            suffix: suffix.as_hostname().clone(),
        }),
        AutomaticHostnameConfiguration::Ployz => {
            let Some(PloyzDnsTargetAllocation::Allocated { lease }) = allocation else {
                return Err(DeployFactLoadError::IngressUnavailable {
                    message: "Ployz DNS target is not allocated".to_owned(),
                });
            };
            let suffix = RouteHostname::try_new(lease.name.hostname_suffix()).map_err(|error| {
                DeployFactLoadError::IngressState {
                    message: error.to_string(),
                }
            })?;
            Ok(AutomaticHostnameMode::Ployz { suffix })
        }
    }
}

fn has_auto_hostname_service(target: &DeployPlanningTarget) -> bool {
    target.services().iter().any(|service| {
        service
            .routes()
            .iter()
            .any(|route| matches!(route.target, DeployRouteTarget::AutoHostname { .. }))
    })
}

pub async fn validate_deploy_route_admission(
    request: &DeployRequest,
    ingress: &IngressIntentStore,
    target_store: &PloyzDnsTargetStore,
    namespace: &NamespaceIntentStore,
) -> Result<(), DeployFactLoadError> {
    let target = DeployPlanningTarget::try_from_deploy(request).map_err(|error| {
        DeployFactLoadError::InvalidStoredTarget {
            message: error.to_string(),
        }
    })?;
    let configuration =
        ingress
            .load()
            .await
            .map_err(|error| DeployFactLoadError::IngressState {
                message: error.to_string(),
            })?;
    let allocation = if has_auto_hostname_service(&target)
        && configuration.as_ref().is_some_and(|configuration| {
            matches!(
                configuration.automatic_hostnames(),
                AutomaticHostnameConfiguration::Ployz
            )
        }) {
        target_store
            .load_allocation()
            .await
            .map_err(|error| DeployFactLoadError::IngressState {
                message: error.to_string(),
            })?
    } else {
        None
    };
    let automatic_hostname_mode = resolve_automatic_hostname_mode(
        &target,
        configuration
            .as_ref()
            .map(IngressConfiguration::automatic_hostnames),
        allocation.as_ref(),
    )?;
    let existing = namespace
        .load()
        .await
        .map_err(|error| DeployFactLoadError::IntentRead {
            message: error.to_string(),
        })?
        .route_bindings;
    validate_route_bindings(&target, automatic_hostname_mode.suffix(), &existing)
}

async fn read_intent(
    intent_reader: &NatsIntentReader,
) -> Result<IntentSnapshot, DeployFactLoadError> {
    intent_reader
        .intent()
        .await
        .map_err(|source| DeployFactLoadError::IntentRead {
            message: source.to_string(),
        })
}

async fn deploy_execution_facts(
    target: &DeployPlanningTarget,
    facts_reader: &NatsMachineFactsReader,
    intent: IntentSnapshot,
    projection: DataplaneProjection,
    automatic_hostname_mode: AutomaticHostnameMode,
    publishable_gateway_ids: Vec<MachineId>,
    gather_policy: DeployFactGatherPolicy,
) -> Result<DeployExecutionFacts, DeployFactLoadError> {
    let active_machines = intent.active_machines.clone();
    let machine_lifecycles = load_machine_lifecycles(&intent);
    // Hostnames share one managed DNS lease across the cluster, so minting
    // must see bindings in every namespace. Namespace-scoped removal still
    // filters inside the planner.
    let namespace_route_bindings = intent.route_bindings;
    let namespace_serving_entries = intent
        .serving_target_entries
        .into_iter()
        .filter(|entry| entry.namespace_id == *target.namespace_id())
        .collect::<Vec<_>>();
    let namespace_volume_pins = intent
        .volume_pins
        .into_iter()
        .filter(|pin| pin.namespace_id() == target.namespace_id())
        .collect::<Vec<_>>();
    let placement_facts = match gather_policy.placement_facts {
        PlacementFactsGatherPolicy::CompleteAll => {
            read_machine_placement_facts(facts_reader, machine_lifecycles).await
        }
        PlacementFactsGatherPolicy::OverallDeadline(timeout) => {
            read_machine_placement_facts_with_gather_timeout(
                facts_reader,
                machine_lifecycles,
                timeout,
            )
            .await
        }
    };
    let dataplane_statuses = gather_dataplane_statuses(
        facts_reader,
        projection
            .declared_members()
            .iter()
            .map(|member| &member.machine_id),
    )
    .await;
    let observed_machines = placement_facts
        .iter()
        .filter_map(|facts| {
            facts
                .answer
                .as_ref()
                .map(|answer| answer.containers.clone())
        })
        .collect::<Vec<_>>();
    let (eligible_machines, unusable_machines) =
        classify_machine_usability(&placement_facts, &projection, &dataplane_statuses);
    let machine_platforms = placement_facts
        .iter()
        .filter_map(|facts| {
            facts
                .answer
                .as_ref()
                .map(|answer| (facts.machine_id.clone(), answer.platform.clone()))
        })
        .collect();
    let seed_clock_testimony = placement_facts
        .iter()
        .filter_map(|facts| {
            facts
                .answer
                .as_ref()
                .map(|answer| (facts.machine_id.clone(), answer.clock))
        })
        .collect();
    let machine_storage_testimony = placement_facts
        .iter()
        .filter_map(|facts| {
            facts
                .answer
                .as_ref()
                .map(|answer| (facts.machine_id.clone(), answer.storage.clone()))
        })
        .collect();
    let dataplane_members = operation_dataplane_members(target, &active_machines);
    let gateway_certificate_targets =
        gateway_certificate_targets(&active_machines, &placement_facts);
    let ployz_gateway_certificate_targets = gateway_certificate_targets
        .iter()
        .filter(|target| publishable_gateway_ids.contains(&target.machine_id))
        .cloned()
        .collect();
    let namespace_cleanup_candidates =
        namespace_cleanup_candidates(target.namespace_id(), &observed_machines);
    validate_route_bindings(
        target,
        automatic_hostname_mode.suffix(),
        &namespace_route_bindings,
    )?;
    Ok(DeployExecutionFacts {
        namespace_route_bindings,
        namespace_serving_entries,
        namespace_volume_pins,
        eligible_machines,
        unusable_machines,
        dataplane_members,
        observed_machines,
        machine_platforms,
        seed_clock_testimony,
        machine_storage_testimony,
        namespace_cleanup_candidates,
        automatic_hostname_mode,
        gateway_certificate_targets,
        ployz_gateway_certificate_targets,
        step_timeout: gather_policy.step_timeout,
    })
}

fn validate_route_bindings(
    target: &DeployPlanningTarget,
    automatic_hostname_suffix: Option<&RouteHostname>,
    existing: &[ployz_core::intent::RouteBindingState],
) -> Result<(), DeployFactLoadError> {
    ployz_core::deploy::validate_deploy_route_bindings(target, automatic_hostname_suffix, existing)
        .map(|_| ())
        .map_err(|failure| DeployFactLoadError::InvalidRouteBindings { failure })
}

fn operation_dataplane_members(
    target: &DeployPlanningTarget,
    active_machines: &[ActiveMachineState],
) -> Vec<DataplaneMember> {
    let needs_membership = target.services().iter().any(|service| {
        !service.routes().is_empty()
            || service.concrete_image().is_some_and(|(_, image_source)| {
                matches!(
                    image_source,
                    ployz_core::deploy::ImageSource::PushedToSeed(_)
                )
            })
    });
    if !needs_membership {
        return Vec::new();
    }

    active_machines
        .iter()
        .map(|machine| DataplaneMember {
            machine_id: machine.machine_id.clone(),
            endpoint_subnet: machine.endpoint_subnet.clone(),
        })
        .collect()
}

fn load_machine_lifecycles(intent: &IntentSnapshot) -> Vec<(MachineId, MachineLifecycle)> {
    intent
        .active_machines
        .iter()
        .map(|machine| (machine.machine_id.clone(), machine.lifecycle))
        .collect()
}

/// An intent read failed before deploy execution started. The rendered
/// message is failure evidence.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployFactLoadError {
    #[error("stored deploy target is invalid: {message}")]
    InvalidStoredTarget { message: String },
    #[error("intent could not be read: {message}")]
    IntentRead { message: String },
    #[error("invalid route bindings: {failure}")]
    InvalidRouteBindings {
        failure: DeployRouteBindingValidationError,
    },
    #[error("ingress state could not be read: {message}")]
    IngressState { message: String },
    #[error("ingress is unavailable: {message}")]
    IngressUnavailable { message: String },
}
