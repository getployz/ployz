//! Load deploy execution facts from core intent and fresh machine facts RPCs.

use crate::intent::lease_intent::{LeaseIntentStore, StoreLeaseOutcome};
use crate::intent::service::NatsIntentReader;
use crate::lease::{BundleDownloadOutcome, LeaseClient};
use crate::operations::log::OperationRepository;
use crate::roles::machine::client::{NatsMachineFactsReader, read_machine_placement_facts};
use ployz_core::cert::ManagedCertificateIssuanceFailureKind;
use ployz_core::dataplane::DataplaneMember;
use ployz_core::deploy::{DeployRequest, DeployRouteTarget};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::ops::{DeployEvidence, UnusableMachine};
use ployz_core::state::{
    ActiveMachineState, IntentSnapshot, MachineLifecycle, MachineUsabilityReason,
    placement_rejection,
};
use std::collections::BTreeMap;
use std::time::Duration;

use super::DeployExecutionFacts;
use super::preparation::namespace_cleanup_candidates;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedCertificateWaitPolicy {
    overall_timeout: Duration,
    poll_interval: Duration,
}

impl ManagedCertificateWaitPolicy {
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

pub(super) struct ManagedCertificateWaitContext<'a> {
    pub(super) lease_intent: &'a LeaseIntentStore,
    pub(super) lease_client: &'a LeaseClient,
    pub(super) repository: &'a OperationRepository,
    pub(super) policy: ManagedCertificateWaitPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployMachineCandidates {
    machine_ids: Vec<MachineId>,
}

impl DeployMachineCandidates {
    #[must_use]
    pub fn same_machines(machines: Vec<MachineId>) -> Self {
        Self {
            machine_ids: sorted_unique_machines(machines.iter()),
        }
    }
}

pub async fn load_deploy_execution_facts_from_nats(
    request: &DeployRequest,
    fallback_candidates: DeployMachineCandidates,
    intent_reader: &NatsIntentReader,
    facts_reader: &NatsMachineFactsReader,
    step_timeout: Duration,
) -> Result<DeployExecutionFacts, DeployFactLoadError> {
    let intent = read_intent(intent_reader).await?;
    let managed_lease = match &intent.managed_lease {
        ployz_core::state::ManagedLeaseProjection::Ready { lease, .. } => Some(lease.name.clone()),
        ployz_core::state::ManagedLeaseProjection::Unacquired
        | ployz_core::state::ManagedLeaseProjection::RecordOnly { .. } => None,
    };
    deploy_execution_facts(
        request,
        fallback_candidates,
        facts_reader,
        intent,
        managed_lease,
        step_timeout,
    )
    .await
}

pub(super) async fn ensure_managed_certificate_for_deploy(
    request: &DeployRequest,
    operation_id: &OperationId,
    intent_reader: &NatsIntentReader,
    context: ManagedCertificateWaitContext<'_>,
) -> Result<(), DeployFactLoadError> {
    if !requests_auto_hostname(request) {
        return Ok(());
    }
    let intent = read_intent(intent_reader).await?;
    ensure_managed_certificate(operation_id, &intent.managed_lease, context).await
}

fn requests_auto_hostname(request: &DeployRequest) -> bool {
    request.services.iter().any(|service| {
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
    fallback_candidates: DeployMachineCandidates,
    facts_reader: &NatsMachineFactsReader,
    intent: IntentSnapshot,
    managed_lease: Option<ployz_core::cert::ManagedLeaseName>,
    step_timeout: Duration,
) -> Result<DeployExecutionFacts, DeployFactLoadError> {
    let active_machines = intent.active_machines.clone();
    let machine_lifecycles = load_machine_lifecycles(&intent, fallback_candidates.clone());
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
    let observed_machines = placement_facts
        .iter()
        .filter_map(|facts| facts.containers.clone())
        .collect::<Vec<_>>();
    let answering_machines = sorted_unique_machines(
        observed_machines
            .iter()
            .map(MachineContainerObservationSnapshot::machine_id),
    );
    let (eligible_machines, unusable_machines) = classify_machine_usability(&placement_facts);
    let machine_platforms = placement_facts
        .iter()
        .filter_map(|facts| {
            facts
                .platform
                .clone()
                .map(|platform| (facts.machine_id.clone(), platform))
        })
        .collect();
    let dataplane_members =
        operation_dataplane_members(request, &active_machines, answering_machines);
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
        step_timeout,
    })
}

async fn ensure_managed_certificate(
    operation_id: &OperationId,
    projection: &ployz_core::state::ManagedLeaseProjection,
    context: ManagedCertificateWaitContext<'_>,
) -> Result<(), DeployFactLoadError> {
    match projection {
        ployz_core::state::ManagedLeaseProjection::Ready { .. }
        | ployz_core::state::ManagedLeaseProjection::Unacquired => Ok(()),
        ployz_core::state::ManagedLeaseProjection::RecordOnly { lease } => {
            wait_for_managed_certificate(operation_id, lease, context).await
        }
    }
}

async fn wait_for_managed_certificate(
    operation_id: &OperationId,
    lease: &ployz_core::cert::ManagedLeaseRecord,
    context: ManagedCertificateWaitContext<'_>,
) -> Result<(), DeployFactLoadError> {
    let deadline = tokio::time::Instant::now() + context.policy.overall_timeout;
    let mut latest_last_error = None;
    let mut waiting_recorded = false;

    loop {
        let download = tokio::time::timeout_at(
            deadline,
            context
                .lease_client
                .download_bundle(lease.name.clone(), lease.token.clone()),
        )
        .await;
        match download {
            Err(_) => {
                return Err(certificate_pending(latest_last_error));
            }
            Ok(Err(source)) => {
                return Err(DeployFactLoadError::ManagedCertificateWorker {
                    message: source.to_string(),
                });
            }
            Ok(Ok(BundleDownloadOutcome::Ready(bundle))) => {
                return match context
                    .lease_intent
                    .store_lease(lease.clone(), Some(bundle))
                    .await
                    .map_err(|source| DeployFactLoadError::ManagedCertificateStore {
                        message: source.to_string(),
                    })? {
                    StoreLeaseOutcome::Stored => Ok(()),
                    StoreLeaseOutcome::Superseded => {
                        Err(DeployFactLoadError::ManagedCertificateSuperseded)
                    }
                };
            }
            Ok(Ok(BundleDownloadOutcome::Pending { last_error })) => {
                latest_last_error = last_error;
                if !waiting_recorded {
                    context
                        .repository
                        .record_deploy_evidence(
                            operation_id,
                            DeployEvidence::WaitingForManagedCertificate,
                        )
                        .await
                        .map_err(|source| DeployFactLoadError::ManagedCertificateProgress {
                            message: source.to_string(),
                        })?;
                    waiting_recorded = true;
                }
            }
        }

        let next_poll = (tokio::time::Instant::now() + context.policy.poll_interval).min(deadline);
        tokio::time::sleep_until(next_poll).await;
        if tokio::time::Instant::now() >= deadline {
            return Err(certificate_pending(latest_last_error));
        }
    }
}

fn certificate_pending(
    last_error: Option<ManagedCertificateIssuanceFailureKind>,
) -> DeployFactLoadError {
    DeployFactLoadError::CertificatePending { last_error }
}

fn operation_dataplane_members(
    request: &DeployRequest,
    active_machines: &[ActiveMachineState],
    fallback_machines: Vec<MachineId>,
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

    if !active_machines.is_empty() {
        return active_machines
            .iter()
            .map(|machine| DataplaneMember {
                machine_id: machine.machine_id.clone(),
                endpoint_subnet: machine.endpoint_subnet.clone(),
            })
            .collect();
    }

    sorted_unique_machines(fallback_machines.iter())
        .into_iter()
        .map(DataplaneMember::default_for_machine)
        .collect()
}

fn load_machine_lifecycles(
    intent: &IntentSnapshot,
    fallback: DeployMachineCandidates,
) -> Vec<(MachineId, MachineLifecycle)> {
    if intent.active_machines.is_empty() {
        return fallback
            .machine_ids
            .into_iter()
            .map(|machine_id| (machine_id, MachineLifecycle::Active))
            .collect();
    }

    intent
        .active_machines
        .iter()
        .map(|machine| (machine.machine_id.clone(), machine.lifecycle))
        .collect()
}

fn sorted_unique_machines<'a>(machines: impl IntoIterator<Item = &'a MachineId>) -> Vec<MachineId> {
    machines
        .into_iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn classify_machine_usability(
    placement_facts: &[crate::roles::machine::client::MachinePlacementFacts],
) -> (Vec<MachineId>, Vec<UnusableMachine>) {
    let mut eligible = Vec::new();
    let mut unusable = BTreeMap::new();

    for facts in placement_facts {
        if let Some(reason) = placement_rejection(facts.lifecycle) {
            unusable.insert(facts.machine_id.clone(), reason);
            continue;
        }

        // Eligibility is reachability plus operator intent: a machine that
        // answered with its facts and is not draining can take work. Placement
        // does not ask a machine to bid — a dead machine is silent here and
        // fails again at the point of use (ADR 0027).
        if facts.containers.is_some() {
            eligible.push(facts.machine_id.clone());
            continue;
        }

        unusable.insert(
            facts.machine_id.clone(),
            MachineUsabilityReason::FactsUnavailable,
        );
    }

    (
        eligible,
        unusable
            .into_iter()
            .map(|(machine_id, reason)| UnusableMachine { machine_id, reason })
            .collect(),
    )
}

/// An intent read failed before deploy execution started. The rendered
/// message is failure evidence.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployFactLoadError {
    #[error("intent could not be read: {message}")]
    IntentRead { message: String },
    #[error("managed certificate is still pending")]
    CertificatePending {
        last_error: Option<ManagedCertificateIssuanceFailureKind>,
    },
    #[error("managed certificate worker failed: {message}")]
    ManagedCertificateWorker { message: String },
    #[error("managed certificate could not be stored: {message}")]
    ManagedCertificateStore { message: String },
    #[error("managed certificate result was superseded by a public URL mode change")]
    ManagedCertificateSuperseded,
    #[error("managed certificate wait progress could not be recorded: {message}")]
    ManagedCertificateProgress { message: String },
}
