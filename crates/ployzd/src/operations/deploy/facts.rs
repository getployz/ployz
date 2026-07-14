//! Load deploy execution facts from core intent and fresh machine facts RPCs.

use crate::certificate::gateway_certificate_targets;
use crate::intent::ingress_intent::{
    IngressIntentStore, PloyzDnsTargetAllocation, PloyzDnsTargetStore,
};
use crate::intent::namespace_intent::NamespaceIntentStore;
use crate::intent::service::NatsIntentReader;
use crate::roles::machine::client::{NatsMachineFactsReader, read_machine_placement_facts};
use crate::roles::machine::convergence::gather_dataplane_statuses;
use ployz_core::dataplane::{DataplaneMember, DataplaneProjection};
use ployz_core::deploy::{DeployRequest, DeployRouteTarget, validate_deploy_route_bindings};
use ployz_core::ids::MachineId;
use ployz_core::ingress::AutomaticHostnameConfiguration;
use ployz_core::ops::RouteHostname;
use ployz_core::state::{ActiveMachineState, IntentSnapshot, MachineLifecycle};
use std::time::Duration;

use super::DeployExecutionFacts;
use super::placement::classify_machine_usability;
use super::preparation::{namespace_cleanup_candidates, route_binding_id_for_target};

pub async fn load_deploy_execution_facts_from_nats(
    request: &DeployRequest,
    intent_reader: &NatsIntentReader,
    facts_reader: &NatsMachineFactsReader,
    target_store: &PloyzDnsTargetStore,
    step_timeout: Duration,
) -> Result<DeployExecutionFacts, DeployFactLoadError> {
    let intent = read_intent(intent_reader).await?;
    let (automatic_hostname_suffix, ployz_automatic_hostnames) =
        automatic_hostname_context(request, &intent, target_store).await?;
    let publishable_gateway_ids = if ployz_automatic_hostnames {
        target_store
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
        request,
        facts_reader,
        intent,
        projection,
        automatic_hostname_suffix,
        ployz_automatic_hostnames,
        publishable_gateway_ids,
        step_timeout,
    )
    .await
}

pub(super) fn auto_hostname_service(
    request: &DeployRequest,
) -> Option<&ployz_core::deploy::DeployServiceSpec> {
    request.services.iter().find(|service| {
        service
            .routes
            .iter()
            .any(|route| matches!(route.target, DeployRouteTarget::AutoHostname { .. }))
    })
}

async fn automatic_hostname_context(
    request: &DeployRequest,
    intent: &IntentSnapshot,
    target_store: &PloyzDnsTargetStore,
) -> Result<(Option<RouteHostname>, bool), DeployFactLoadError> {
    if auto_hostname_service(request).is_none() {
        return Ok((None, false));
    }
    match &intent.automatic_hostname_configuration {
        AutomaticHostnameConfiguration::Disabled => {
            Err(DeployFactLoadError::InvalidRouteBindings {
                message: "automatic hostnames are disabled".to_owned(),
            })
        }
        AutomaticHostnameConfiguration::Custom { suffix } => {
            Ok((Some(suffix.as_hostname().clone()), false))
        }
        AutomaticHostnameConfiguration::Ployz => {
            let allocation = target_store.load_allocation().await.map_err(|error| {
                DeployFactLoadError::IngressState {
                    message: error.to_string(),
                }
            })?;
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
            Ok((Some(suffix), true))
        }
    }
}

pub async fn validate_deploy_route_admission(
    request: &DeployRequest,
    ingress: &IngressIntentStore,
    target_store: &PloyzDnsTargetStore,
    namespace: &NamespaceIntentStore,
) -> Result<(), DeployFactLoadError> {
    let configuration =
        ingress
            .load()
            .await
            .map_err(|error| DeployFactLoadError::IngressState {
                message: error.to_string(),
            })?;
    let automatic_hostname_suffix = if auto_hostname_service(request).is_some() {
        let Some(configuration) = configuration else {
            return Err(DeployFactLoadError::IngressUnavailable {
                message: "ingress is not configured".to_owned(),
            });
        };
        match configuration.automatic_hostnames {
            AutomaticHostnameConfiguration::Disabled => {
                return Err(DeployFactLoadError::InvalidRouteBindings {
                    message: "automatic hostnames are disabled".to_owned(),
                });
            }
            AutomaticHostnameConfiguration::Custom { suffix } => Some(suffix.as_hostname().clone()),
            AutomaticHostnameConfiguration::Ployz => {
                let allocation = target_store.load_allocation().await.map_err(|error| {
                    DeployFactLoadError::IngressState {
                        message: error.to_string(),
                    }
                })?;
                let Some(PloyzDnsTargetAllocation::Allocated { lease }) = allocation else {
                    return Err(DeployFactLoadError::IngressUnavailable {
                        message: "Ployz DNS target is not allocated".to_owned(),
                    });
                };
                Some(
                    RouteHostname::try_new(lease.name.hostname_suffix()).map_err(|error| {
                        DeployFactLoadError::IngressState {
                            message: error.to_string(),
                        }
                    })?,
                )
            }
        }
    } else {
        None
    };
    let existing = namespace
        .load()
        .await
        .map_err(|error| DeployFactLoadError::IntentRead {
            message: error.to_string(),
        })?
        .route_bindings;
    validate_route_bindings(request, automatic_hostname_suffix.as_ref(), &existing)
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
    request: &DeployRequest,
    facts_reader: &NatsMachineFactsReader,
    intent: IntentSnapshot,
    projection: DataplaneProjection,
    automatic_hostname_suffix: Option<RouteHostname>,
    ployz_automatic_hostnames: bool,
    publishable_gateway_ids: Vec<MachineId>,
    step_timeout: Duration,
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
        .filter(|entry| entry.namespace_id == request.namespace_id)
        .collect::<Vec<_>>();
    let namespace_volume_pins = intent
        .volume_pins
        .into_iter()
        .filter(|pin| pin.namespace_id == request.namespace_id)
        .collect::<Vec<_>>();
    let placement_facts = read_machine_placement_facts(facts_reader, machine_lifecycles).await;
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
        .filter_map(|facts| facts.containers.clone())
        .collect::<Vec<_>>();
    let (eligible_machines, unusable_machines) =
        classify_machine_usability(&placement_facts, &projection, &dataplane_statuses);
    let machine_platforms = placement_facts
        .iter()
        .filter_map(|facts| {
            facts
                .platform
                .clone()
                .map(|platform| (facts.machine_id.clone(), platform))
        })
        .collect();
    let dataplane_members = operation_dataplane_members(request, &active_machines);
    let gateway_certificate_targets =
        gateway_certificate_targets(&active_machines, &placement_facts);
    let ployz_gateway_certificate_targets = gateway_certificate_targets
        .iter()
        .filter(|target| publishable_gateway_ids.contains(&target.machine_id))
        .cloned()
        .collect();
    let namespace_cleanup_candidates =
        namespace_cleanup_candidates(&request.namespace_id, &observed_machines);
    validate_route_bindings(
        request,
        automatic_hostname_suffix.as_ref(),
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
        namespace_cleanup_candidates,
        automatic_hostname_suffix,
        ployz_automatic_hostnames,
        gateway_certificate_targets,
        ployz_gateway_certificate_targets,
        step_timeout,
    })
}

fn validate_route_bindings(
    request: &DeployRequest,
    automatic_hostname_suffix: Option<&RouteHostname>,
    existing: &[ployz_core::state::RouteBindingState],
) -> Result<(), DeployFactLoadError> {
    validate_deploy_route_bindings(
        request,
        automatic_hostname_suffix,
        existing,
        route_binding_id_for_target,
    )
    .map(|_| ())
    .map_err(|error| DeployFactLoadError::InvalidRouteBindings {
        message: error.to_string(),
    })
}

fn operation_dataplane_members(
    request: &DeployRequest,
    active_machines: &[ActiveMachineState],
) -> Vec<DataplaneMember> {
    let needs_membership = request.services.iter().any(|service| {
        !service.routes.is_empty()
            || matches!(
                &service.image_source,
                ployz_core::deploy::ImageSource::PushedToSeed { .. }
            )
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
    #[error("intent could not be read: {message}")]
    IntentRead { message: String },
    #[error("invalid route bindings: {message}")]
    InvalidRouteBindings { message: String },
    #[error("ingress state could not be read: {message}")]
    IngressState { message: String },
    #[error("ingress is unavailable: {message}")]
    IngressUnavailable { message: String },
}
