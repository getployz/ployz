//! Load deploy execution facts from core intent and fresh machine facts RPCs.

use crate::certificate::gateway_certificate_targets;
use crate::intent::lease_intent::LeaseIntentStore;
use crate::intent::service::NatsIntentReader;
use crate::operations::log::OperationRepository;
use crate::roles::machine::client::{NatsMachineFactsReader, read_machine_placement_facts};
use crate::roles::machine::convergence::gather_dataplane_statuses;
use ployz_core::cert::{AutoLeaseState, ManagedLeaseIntent, PublicUrlMode};
use ployz_core::dataplane::{DataplaneMember, DataplaneProjection};
use ployz_core::deploy::{DeployRequest, DeployRouteTarget};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::ops::{DeployEvidence, ManagedPublicUrlPending};
use ployz_core::state::{ActiveMachineState, IntentSnapshot, MachineLifecycle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::DeployExecutionFacts;
use super::placement::classify_machine_usability;
use super::preparation::namespace_cleanup_candidates;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedPublicUrlWaitPolicy {
    overall_timeout: Duration,
    poll_interval: Duration,
}

impl ManagedPublicUrlWaitPolicy {
    #[must_use]
    pub const fn production() -> Self {
        Self::new(Duration::from_secs(90), Duration::from_secs(5))
    }

    #[must_use]
    pub const fn new(overall_timeout: Duration, poll_interval: Duration) -> Self {
        Self {
            overall_timeout,
            poll_interval,
        }
    }
}

pub(super) struct ManagedPublicUrlWaitContext<'a> {
    pub(super) lease_intent: &'a LeaseIntentStore,
    pub(super) repository: &'a OperationRepository,
    pub(super) policy: ManagedPublicUrlWaitPolicy,
}

pub async fn load_deploy_execution_facts_from_nats(
    request: &DeployRequest,
    intent_reader: &NatsIntentReader,
    facts_reader: &NatsMachineFactsReader,
    step_timeout: Duration,
) -> Result<DeployExecutionFacts, DeployFactLoadError> {
    let intent = read_intent(intent_reader).await?;
    let managed_lease = match &intent.public_url {
        ployz_core::state::IntentPublicUrl::Auto(managed_lease) => match managed_lease.as_ref() {
            ployz_core::state::ManagedLeaseProjection::Ready { lease, .. } => {
                Some(lease.name.clone())
            }
            ployz_core::state::ManagedLeaseProjection::Unacquired
            | ployz_core::state::ManagedLeaseProjection::RecordOnly { .. } => None,
        },
        ployz_core::state::IntentPublicUrl::Unconfigured
        | ployz_core::state::IntentPublicUrl::BringYourOwn
        | ployz_core::state::IntentPublicUrl::None => None,
    };
    let projection = intent.dataplane_projection.clone();
    deploy_execution_facts(
        request,
        facts_reader,
        intent,
        projection,
        managed_lease,
        step_timeout,
    )
    .await
}

pub(super) async fn ensure_managed_public_url_for_deploy(
    request: &DeployRequest,
    operation_id: &OperationId,
    context: ManagedPublicUrlWaitContext<'_>,
) -> Result<(), DeployFactLoadError> {
    if auto_hostname_service(request).is_none() {
        return Ok(());
    }
    wait_for_managed_public_url(operation_id, context).await
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
    managed_lease: Option<ployz_core::cert::ManagedLeaseName>,
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
    let namespace_cleanup_candidates =
        namespace_cleanup_candidates(&request.namespace_id, &observed_machines);
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
        managed_lease,
        gateway_certificate_targets,
        step_timeout,
    })
}

async fn wait_for_managed_public_url(
    operation_id: &OperationId,
    context: ManagedPublicUrlWaitContext<'_>,
) -> Result<(), DeployFactLoadError> {
    let deadline = tokio::time::Instant::now() + context.policy.overall_timeout;
    let mut recorded_stage = None;
    loop {
        let intent = context
            .lease_intent
            .load_if_configured()
            .await
            .map_err(|source| DeployFactLoadError::ManagedPublicUrlStore {
                message: source.to_string(),
            })?;
        let pending =
            match intent {
                Some(ManagedLeaseIntent::BringYourOwn) => {
                    return Err(DeployFactLoadError::ManagedPublicUrlDisabled {
                        mode: PublicUrlMode::BringYourOwn,
                    });
                }
                Some(ManagedLeaseIntent::None) => {
                    return Err(DeployFactLoadError::ManagedPublicUrlDisabled {
                        mode: PublicUrlMode::None,
                    });
                }
                None => ManagedPublicUrlPending::Lease,
                Some(ManagedLeaseIntent::Auto { state }) => match *state {
                    AutoLeaseState::Unacquired { .. } => ManagedPublicUrlPending::Lease,
                    AutoLeaseState::RecordOnly { lease } => {
                        if lease.is_valid_at(now_seconds()?) {
                            ManagedPublicUrlPending::Certificate { last_error: None }
                        } else {
                            ManagedPublicUrlPending::Lease
                        }
                    }
                    AutoLeaseState::Ready { lease, bundle } => {
                        let now_seconds = now_seconds()?;
                        if !lease.is_valid_at(now_seconds) {
                            ManagedPublicUrlPending::Lease
                        } else if !bundle.is_valid_at(now_seconds) {
                            ManagedPublicUrlPending::Certificate { last_error: None }
                        } else {
                            let applied =
                                context.lease_intent.applied_address_set().await.map_err(
                                    |source| DeployFactLoadError::ManagedPublicUrlStore {
                                        message: source.to_string(),
                                    },
                                )?;
                            if applied.is_some() {
                                return Ok(());
                            }
                            ManagedPublicUrlPending::GatewayAddresses
                        }
                    }
                },
            };

        let stage = pending.stage();
        if recorded_stage != Some(stage) {
            context
                .repository
                .record_deploy_evidence(
                    operation_id,
                    DeployEvidence::WaitingForManagedPublicUrl { stage },
                )
                .await
                .map_err(|source| DeployFactLoadError::ManagedPublicUrlProgress {
                    message: source.to_string(),
                })?;
            recorded_stage = Some(stage);
        }

        let next_poll = (tokio::time::Instant::now() + context.policy.poll_interval).min(deadline);
        tokio::time::sleep_until(next_poll).await;
        if tokio::time::Instant::now() >= deadline {
            return Err(DeployFactLoadError::ManagedPublicUrlPending { pending });
        }
    }
}

fn now_seconds() -> Result<u64, DeployFactLoadError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|source| DeployFactLoadError::ManagedPublicUrlClock {
            message: source.to_string(),
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
    #[error("managed public URL is still pending at {pending:?}")]
    ManagedPublicUrlPending { pending: ManagedPublicUrlPending },
    #[error("automatic public URLs are disabled by mode {mode:?}")]
    ManagedPublicUrlDisabled { mode: PublicUrlMode },
    #[error("managed public URL state could not be read: {message}")]
    ManagedPublicUrlStore { message: String },
    #[error("managed public URL clock could not be read: {message}")]
    ManagedPublicUrlClock { message: String },
    #[error("managed public URL wait progress could not be recorded: {message}")]
    ManagedPublicUrlProgress { message: String },
}
