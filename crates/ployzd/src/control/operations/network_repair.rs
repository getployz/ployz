//! Operation-owned cluster dataplane repair.

use crate::control::intent::service::NatsIntentReader;
use crate::control::operation_evidence::{
    AcceptedNetworkRepairSubmission, OperationStatusStoreError, RecordOperationEventError,
};
use crate::control::role_client::dns::NatsDnsClient;
use crate::control::role_client::machine::{
    MAX_CONCURRENT_MACHINE_READS, MachineFactsRefreshError, NatsMachineFactsReader,
    unavailable_reason,
};
use crate::control::role_client::machine_convergence::gather_dataplane_statuses;
use crate::control::sequencer::OperationControllers;
use crate::roles::dns::protocol::DnsStatusRpcOk;
use crate::roles::machine::{MachineRequestFailure, MachineRuntimeUnavailableReason};
use crate::tasks::TaskSpawner;
use futures_util::{StreamExt, stream};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine::runtime::MachineFactsRefreshConfirmation;
use ployz_core::machine::validate_declared_machine;
use ployz_core::network::DataplaneProjection;
use ployz_core::network::internal_dns::{
    InternalDnsFactWatermark, InternalDnsResolverStatus, InternalDnsStatus,
};
use ployz_core::operation::{
    FailureMessage, NetworkRepairDnsRefreshProblem, NetworkRepairEvidence, NetworkRepairFailure,
    NetworkRepairMachineFactsRefreshOutcome, NetworkRepairProgressPhase,
    NetworkRepairRequestFailure, NetworkRepairRunningStage, NetworkRepairTransition,
};
use ployz_nats::service_runtime::NatsJsonServiceRequestError;
use std::time::Duration;

const DNS_REFRESH_POLL_INTERVAL: Duration = Duration::from_millis(100);
const DNS_STATUS_REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
const RECORD_ATTEMPTS: usize = 3;
const RECORD_RETRY_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
struct DnsRefreshBaseline {
    resolver_machine_id: MachineId,
    fact_watermarks: Vec<InternalDnsFactWatermark>,
}

#[derive(Debug, Clone)]
pub struct NetworkRepairOperation {
    controllers: OperationControllers,
    intent_reader: NatsIntentReader,
    facts_reader: NatsMachineFactsReader,
    client: async_nats::Client,
    dns_client: NatsDnsClient,
    operation_timeout: Duration,
    task_registry: TaskSpawner,
}

impl NetworkRepairOperation {
    #[must_use]
    pub fn new(
        controllers: OperationControllers,
        intent_reader: NatsIntentReader,
        facts_reader: NatsMachineFactsReader,
        client: async_nats::Client,
        operation_timeout: Duration,
        task_registry: TaskSpawner,
    ) -> Self {
        let dns_client = NatsDnsClient::new(client.clone());
        Self {
            controllers,
            intent_reader,
            facts_reader,
            client,
            dns_client,
            operation_timeout,
            task_registry,
        }
    }

    pub async fn start(&self, accepted: AcceptedNetworkRepairSubmission) {
        if !accepted.should_start_execution {
            return;
        }
        let operation_id = accepted.operation_id.clone();
        let runtime = self.clone();
        super::finish_rejected_task_admission(
            &self.controllers,
            &operation_id,
            self.task_registry.spawn(|| async move {
                runtime.run(accepted).await;
            }),
        )
        .await;
    }

    #[tracing::instrument(name = "operation", skip_all, fields(kind = "network_repair", operation_id = accepted.operation_id.as_str()))]
    pub async fn run(self, accepted: AcceptedNetworkRepairSubmission) {
        let operation_id = accepted.operation_id;
        if let Err(error) = self
            .record_transition_with_retry(
                &operation_id,
                NetworkRepairTransition::Running {
                    stage: NetworkRepairRunningStage::AwaitingDataplane,
                },
            )
            .await
        {
            record_warning(&operation_id, "record-running", &error);
            self.record_progress_failure(
                &operation_id,
                NetworkRepairProgressPhase::Starting,
                &error,
            )
            .await;
            return;
        }
        let intent = match self.intent_reader.intent().await {
            Ok(intent) => intent,
            Err(error) => {
                self.record_terminal(
                    &operation_id,
                    NetworkRepairTransition::Failed {
                        failure: NetworkRepairFailure::IntentReadFailed {
                            message: failure_message(error.to_string()),
                        },
                    },
                )
                .await;
                return;
            }
        };
        let machine_ids = intent
            .active_machines
            .iter()
            .map(|machine| machine.machine_id.clone())
            .collect::<Vec<_>>();
        if machine_ids.is_empty() {
            self.record_terminal(
                &operation_id,
                NetworkRepairTransition::Failed {
                    failure: NetworkRepairFailure::NoActiveMachines,
                },
            )
            .await;
            return;
        }
        let targets = match accepted.target_machine_id {
            Some(machine_id) if machine_ids.contains(&machine_id) => vec![machine_id],
            Some(machine_id) => {
                self.record_terminal(
                    &operation_id,
                    NetworkRepairTransition::Failed {
                        failure: NetworkRepairFailure::TargetMachineNotFound { machine_id },
                    },
                )
                .await;
                return;
            }
            None => machine_ids.clone(),
        };
        let projection = intent.dataplane_projection;
        let invalidation = match tokio::time::timeout(self.operation_timeout, async {
            self.client
                .publish(ployz_nats::subjects::INTENT_CHANGED, Vec::new().into())
                .await
                .map_err(|error| error.to_string())?;
            self.client.flush().await.map_err(|error| error.to_string())
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(format!(
                "intent invalidation timed out after {}s",
                self.operation_timeout.as_secs()
            )),
        };
        if let Err(error) = invalidation {
            self.record_terminal(
                &operation_id,
                NetworkRepairTransition::Failed {
                    failure: NetworkRepairFailure::IntentReadFailed {
                        message: failure_message(error.to_string()),
                    },
                },
            )
            .await;
            return;
        }
        match self.await_declared_projection(&projection, &targets).await {
            Ok(()) => {}
            Err(failure) => {
                self.record_terminal(&operation_id, NetworkRepairTransition::Failed { failure })
                    .await;
                return;
            }
        }
        if !self
            .record_evidence(
                &operation_id,
                NetworkRepairEvidence::DataplaneConverged {
                    revision: projection.declared_revision().clone(),
                    machine_ids: targets,
                },
                NetworkRepairProgressPhase::RecordingDataplaneEvidence,
            )
            .await
        {
            return;
        }
        if !self
            .record_stage(
                &operation_id,
                NetworkRepairRunningStage::RefreshingMachineFacts,
                NetworkRepairProgressPhase::AdvancingMachineFacts,
            )
            .await
        {
            return;
        }
        let baselines = match self.gather_dns_refresh_baselines(&machine_ids).await {
            Ok(baselines) => baselines,
            Err(failure) => {
                self.record_terminal(&operation_id, NetworkRepairTransition::Failed { failure })
                    .await;
                return;
            }
        };
        let refreshes = match self.refresh_machine_facts(&machine_ids).await {
            Ok(refreshes) => refreshes,
            Err(failure) => {
                self.record_terminal(&operation_id, NetworkRepairTransition::Failed { failure })
                    .await;
                return;
            }
        };
        if !self
            .record_evidence(
                &operation_id,
                NetworkRepairEvidence::MachineFactsRefreshed { refreshes },
                NetworkRepairProgressPhase::RecordingMachineFactsEvidence,
            )
            .await
        {
            return;
        }
        if !self
            .record_stage(
                &operation_id,
                NetworkRepairRunningStage::ConfirmingDnsRefresh,
                NetworkRepairProgressPhase::AdvancingDnsRefresh,
            )
            .await
        {
            return;
        }
        let confirmed = match self.confirm_dns_refresh(&machine_ids, &baselines).await {
            Ok(confirmed) => confirmed,
            Err(failure) => {
                self.record_terminal(&operation_id, NetworkRepairTransition::Failed { failure })
                    .await;
                return;
            }
        };
        if !self
            .record_evidence(
                &operation_id,
                NetworkRepairEvidence::DnsRefreshConfirmed {
                    machine_ids: confirmed,
                },
                NetworkRepairProgressPhase::RecordingDnsRefreshEvidence,
            )
            .await
        {
            return;
        }
        self.record_terminal_for_phase(
            &operation_id,
            NetworkRepairTransition::Completed,
            NetworkRepairProgressPhase::Completing,
        )
        .await;
    }

    async fn await_declared_projection(
        &self,
        projection: &DataplaneProjection,
        targets: &[MachineId],
    ) -> Result<(), NetworkRepairFailure> {
        for machine_id in targets {
            declared_projection_member(projection, machine_id)?;
        }
        let deadline = tokio::time::Instant::now() + self.operation_timeout;
        loop {
            let mut latest = None;
            let statuses = gather_dataplane_statuses(&self.facts_reader, targets).await;
            for (machine_id, result) in statuses {
                let member = declared_projection_member(projection, &machine_id)?;
                match result {
                    Ok(status) => {
                        if let Some(reason) = validate_declared_machine(projection, member, &status)
                        {
                            latest = Some(NetworkRepairFailure::DataplaneUnavailable {
                                machine_id,
                                reason,
                            });
                            break;
                        }
                    }
                    Err(message) => {
                        latest = Some(NetworkRepairFailure::DataplaneUnavailable {
                            machine_id,
                            reason:
                                ployz_core::machine::DataplaneProjectionAdmissionFailure::NoAnswer {
                                    message,
                                },
                        });
                        break;
                    }
                }
            }
            let Some(failure) = latest else {
                return Ok(());
            };
            if tokio::time::Instant::now() >= deadline {
                return Err(failure);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn record_stage(
        &self,
        operation_id: &OperationId,
        stage: NetworkRepairRunningStage,
        phase: NetworkRepairProgressPhase,
    ) -> bool {
        match self
            .record_transition_with_retry(operation_id, NetworkRepairTransition::Running { stage })
            .await
        {
            Ok(_) => true,
            Err(error) => {
                record_warning(operation_id, "record-running", &error);
                self.record_progress_failure(operation_id, phase, &error)
                    .await;
                false
            }
        }
    }

    async fn record_evidence(
        &self,
        operation_id: &OperationId,
        evidence: NetworkRepairEvidence,
        phase: NetworkRepairProgressPhase,
    ) -> bool {
        match self
            .record_evidence_with_retry(operation_id, evidence)
            .await
        {
            Ok(_) => true,
            Err(error) => {
                record_warning(operation_id, phase.as_str(), &error);
                self.record_progress_failure(operation_id, phase, &error)
                    .await;
                false
            }
        }
    }

    async fn refresh_machine_facts(
        &self,
        machine_ids: &[MachineId],
    ) -> Result<Vec<MachineFactsRefreshConfirmation>, NetworkRepairFailure> {
        let deadline = tokio::time::Instant::now() + self.operation_timeout;
        let outcomes = stream::iter(machine_ids.iter().cloned())
            .map(|machine_id| async move {
                match tokio::time::timeout_at(
                    deadline,
                    self.facts_reader.refresh_machine_facts(&machine_id),
                )
                .await
                {
                    Ok(Ok(refresh)) => {
                        NetworkRepairMachineFactsRefreshOutcome::Refreshed { refresh }
                    }
                    Ok(Err(error)) => machine_facts_refresh_outcome(error),
                    Err(_) => NetworkRepairMachineFactsRefreshOutcome::Unavailable {
                        machine_id,
                        failure: NetworkRepairRequestFailure::TimedOut,
                    },
                }
            })
            .buffer_unordered(MAX_CONCURRENT_MACHINE_READS)
            .collect::<Vec<_>>()
            .await;
        if outcomes.iter().any(|outcome| {
            !matches!(
                outcome,
                NetworkRepairMachineFactsRefreshOutcome::Refreshed { .. }
            )
        }) {
            return Err(NetworkRepairFailure::MachineFactsRefreshFailed { outcomes });
        }
        Ok(outcomes
            .into_iter()
            .map(|outcome| match outcome {
                NetworkRepairMachineFactsRefreshOutcome::Refreshed { refresh } => refresh,
                NetworkRepairMachineFactsRefreshOutcome::Unavailable { .. }
                | NetworkRepairMachineFactsRefreshOutcome::Failed { .. } => {
                    unreachable!("failed outcomes returned above")
                }
            })
            .collect())
    }

    async fn gather_dns_refresh_baselines(
        &self,
        machine_ids: &[MachineId],
    ) -> Result<Vec<DnsRefreshBaseline>, NetworkRepairFailure> {
        let results = self.gather_dns_statuses(machine_ids).await;
        let mut baselines = Vec::new();
        let mut problems = Vec::new();
        for (machine_id, result) in results {
            match dns_status(&machine_id, result) {
                Ok(status) => baselines.push(DnsRefreshBaseline {
                    resolver_machine_id: machine_id,
                    fact_watermarks: status.fact_watermarks,
                }),
                Err(problem) => problems.push(problem),
            }
        }
        if problems.is_empty() {
            Ok(baselines)
        } else {
            Err(NetworkRepairFailure::DnsRefreshFailed {
                confirmed_machine_ids: baselines
                    .into_iter()
                    .map(|baseline| baseline.resolver_machine_id)
                    .collect(),
                problems,
            })
        }
    }

    async fn gather_dns_statuses(
        &self,
        machine_ids: &[MachineId],
    ) -> Vec<(
        MachineId,
        Result<DnsStatusRpcOk, NatsJsonServiceRequestError>,
    )> {
        let timeout = DNS_STATUS_REQUEST_TIMEOUT.min(self.operation_timeout);
        stream::iter(machine_ids.iter().cloned())
            .map(|machine_id| async move {
                let result = self.dns_client.status(&machine_id, timeout).await;
                (machine_id, result)
            })
            .buffer_unordered(MAX_CONCURRENT_MACHINE_READS)
            .collect()
            .await
    }

    async fn confirm_dns_refresh(
        &self,
        machine_ids: &[MachineId],
        baselines: &[DnsRefreshBaseline],
    ) -> Result<Vec<MachineId>, NetworkRepairFailure> {
        let deadline = tokio::time::Instant::now() + self.operation_timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let results = stream::iter(machine_ids.iter().cloned())
                .map(|machine_id| async move {
                    let result = self
                        .dns_client
                        .status(&machine_id, DNS_STATUS_REQUEST_TIMEOUT.min(remaining))
                        .await;
                    (machine_id, result)
                })
                .buffer_unordered(MAX_CONCURRENT_MACHINE_READS)
                .collect::<Vec<_>>()
                .await;
            let mut confirmed_machine_ids = Vec::new();
            let mut problems = Vec::new();
            for (machine_id, result) in results {
                let Some(baseline) = baselines
                    .iter()
                    .find(|baseline| baseline.resolver_machine_id == machine_id)
                else {
                    problems.push(NetworkRepairDnsRefreshProblem::Stale {
                        machine_id,
                        stale_machine_ids: machine_ids.to_vec(),
                    });
                    continue;
                };
                if let Some(problem) =
                    dns_refresh_problem(&machine_id, result, machine_ids, baseline)
                {
                    problems.push(problem);
                } else {
                    confirmed_machine_ids.push(machine_id);
                }
            }
            if problems.is_empty() {
                return Ok(confirmed_machine_ids);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(NetworkRepairFailure::DnsRefreshFailed {
                    confirmed_machine_ids,
                    problems,
                });
            }
            tokio::time::sleep(
                DNS_REFRESH_POLL_INTERVAL
                    .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
            )
            .await;
        }
    }

    async fn record_terminal(
        &self,
        operation_id: &OperationId,
        transition: NetworkRepairTransition,
    ) {
        self.record_terminal_for_phase(
            operation_id,
            transition,
            NetworkRepairProgressPhase::RecordingTerminal,
        )
        .await;
    }

    async fn record_terminal_for_phase(
        &self,
        operation_id: &OperationId,
        transition: NetworkRepairTransition,
        phase: NetworkRepairProgressPhase,
    ) {
        if let Err(error) = self
            .record_transition_with_retry(operation_id, transition)
            .await
        {
            record_warning(operation_id, "record-terminal", &error);
            self.record_progress_failure(operation_id, phase, &error)
                .await;
        }
    }

    async fn record_progress_failure(
        &self,
        operation_id: &OperationId,
        phase: NetworkRepairProgressPhase,
        error: &RecordOperationEventError,
    ) {
        let transition = NetworkRepairTransition::Failed {
            failure: NetworkRepairFailure::ProgressRecordFailed {
                phase,
                message: failure_message(error.to_string()),
            },
        };
        if let Err(error) = self
            .record_transition_with_retry(operation_id, transition)
            .await
        {
            record_warning(operation_id, "record-progress-failure", &error);
        }
    }

    async fn record_transition_with_retry(
        &self,
        operation_id: &OperationId,
        transition: NetworkRepairTransition,
    ) -> Result<(), RecordOperationEventError> {
        let mut attempts = 0;
        loop {
            attempts += 1;
            match self
                .controllers
                .repository()
                .record_network_repair_transition(operation_id, transition.clone())
                .await
            {
                Ok(_) => return Ok(()),
                Err(error) if attempts < RECORD_ATTEMPTS && retryable_record_failure(&error) => {
                    tokio::time::sleep(RECORD_RETRY_DELAY).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn record_evidence_with_retry(
        &self,
        operation_id: &OperationId,
        evidence: NetworkRepairEvidence,
    ) -> Result<(), RecordOperationEventError> {
        let mut attempts = 0;
        loop {
            attempts += 1;
            match self
                .controllers
                .repository()
                .record_network_repair_evidence(operation_id, evidence.clone())
                .await
            {
                Ok(_) => return Ok(()),
                Err(error) if attempts < RECORD_ATTEMPTS && retryable_record_failure(&error) => {
                    tokio::time::sleep(RECORD_RETRY_DELAY).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn declared_projection_member<'a>(
    projection: &'a DataplaneProjection,
    machine_id: &MachineId,
) -> Result<&'a ployz_core::network::DataplaneProjectionMember, NetworkRepairFailure> {
    projection
        .declared_members()
        .iter()
        .find(|member| &member.machine_id == machine_id)
        .ok_or_else(|| NetworkRepairFailure::ProjectionMemberMissing {
            machine_id: machine_id.clone(),
            revision: projection.declared_revision().clone(),
        })
}

fn retryable_record_failure(error: &RecordOperationEventError) -> bool {
    matches!(
        error,
        RecordOperationEventError::StoreStatus(OperationStatusStoreError::Index { .. })
    )
}

fn machine_facts_refresh_outcome(
    error: MachineFactsRefreshError,
) -> NetworkRepairMachineFactsRefreshOutcome {
    match error {
        MachineFactsRefreshError::Unavailable { machine_id, reason } => {
            NetworkRepairMachineFactsRefreshOutcome::Unavailable {
                machine_id,
                failure: machine_runtime_request_failure(reason),
            }
        }
        MachineFactsRefreshError::RefreshFailed {
            machine_id,
            message,
        } => NetworkRepairMachineFactsRefreshOutcome::Failed {
            machine_id,
            message,
        },
    }
}

fn machine_runtime_request_failure(
    reason: MachineRuntimeUnavailableReason,
) -> NetworkRepairRequestFailure {
    match reason.into_request_failure() {
        MachineRequestFailure::NoAnswer => NetworkRepairRequestFailure::NoAnswer,
        MachineRequestFailure::TimedOut => NetworkRepairRequestFailure::TimedOut,
        MachineRequestFailure::RequestFailed { message } => {
            NetworkRepairRequestFailure::RequestFailed { message }
        }
        MachineRequestFailure::ProtocolFailed { message } => {
            NetworkRepairRequestFailure::ProtocolFailed { message }
        }
        MachineRequestFailure::DecodeFailed { message } => {
            NetworkRepairRequestFailure::DecodeFailed { message }
        }
        MachineRequestFailure::WrongResponder { actual_machine_id } => {
            NetworkRepairRequestFailure::WrongResponder { actual_machine_id }
        }
    }
}

fn dns_request_failure(error: NatsJsonServiceRequestError) -> NetworkRepairRequestFailure {
    machine_runtime_request_failure(unavailable_reason(error))
}

fn stale_machine_ids(
    observed: &[InternalDnsFactWatermark],
    expected_machine_ids: &[MachineId],
    baseline: &[InternalDnsFactWatermark],
) -> Vec<MachineId> {
    expected_machine_ids
        .iter()
        .filter(|machine_id| {
            let previous = baseline
                .iter()
                .find(|watermark| watermark.machine_id == **machine_id);
            !observed.iter().any(|watermark| {
                watermark.machine_id == **machine_id
                    && previous.is_none_or(|previous| {
                        watermark.resolver_cache_incarnation != previous.resolver_cache_incarnation
                            || watermark.generation > previous.generation
                    })
            })
        })
        .cloned()
        .collect()
}

fn dns_status(
    machine_id: &MachineId,
    result: Result<DnsStatusRpcOk, NatsJsonServiceRequestError>,
) -> Result<InternalDnsStatus, NetworkRepairDnsRefreshProblem> {
    let status = match result {
        Ok(status) if status.machine_id == *machine_id => status.value,
        Ok(status) => {
            return Err(NetworkRepairDnsRefreshProblem::Unavailable {
                machine_id: machine_id.clone(),
                failure: NetworkRepairRequestFailure::WrongResponder {
                    actual_machine_id: status.machine_id,
                },
            });
        }
        Err(error) => {
            return Err(NetworkRepairDnsRefreshProblem::Unavailable {
                machine_id: machine_id.clone(),
                failure: dns_request_failure(error),
            });
        }
    };
    Ok(status)
}

fn serving_dns_status(
    machine_id: &MachineId,
    result: Result<DnsStatusRpcOk, NatsJsonServiceRequestError>,
) -> Result<InternalDnsStatus, NetworkRepairDnsRefreshProblem> {
    let status = dns_status(machine_id, result)?;
    if !matches!(status.resolver, InternalDnsResolverStatus::Serving { .. }) {
        return Err(NetworkRepairDnsRefreshProblem::ResolverNotServing {
            machine_id: machine_id.clone(),
        });
    }
    Ok(status)
}

fn dns_refresh_problem(
    machine_id: &MachineId,
    result: Result<DnsStatusRpcOk, NatsJsonServiceRequestError>,
    expected_machine_ids: &[MachineId],
    baseline: &DnsRefreshBaseline,
) -> Option<NetworkRepairDnsRefreshProblem> {
    let status = match serving_dns_status(machine_id, result) {
        Ok(status) => status,
        Err(problem) => return Some(problem),
    };
    let stale_machine_ids = stale_machine_ids(
        &status.fact_watermarks,
        expected_machine_ids,
        &baseline.fact_watermarks,
    );
    if stale_machine_ids.is_empty() {
        None
    } else {
        Some(NetworkRepairDnsRefreshProblem::Stale {
            machine_id: machine_id.clone(),
            stale_machine_ids,
        })
    }
}

fn failure_message(message: String) -> FailureMessage {
    FailureMessage::try_new(message).expect("rendered operation failure is non-empty")
}

fn record_warning(operation_id: &OperationId, phase: &str, error: &RecordOperationEventError) {
    tracing::warn!(
        phase,
        operation_id = operation_id.as_str(),
        error = %error,
        "network repair evidence record failed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::network::internal_dns::{
        InternalDnsFactGeneration, InternalDnsIntentHealth, InternalDnsResolverCacheIncarnation,
    };
    use ployz_nats::service_runtime::NatsServiceRequestFailure;

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    #[test]
    fn record_retries_only_storage_failures() {
        let storage = RecordOperationEventError::StoreStatus(OperationStatusStoreError::Index {
            message: "database unavailable".to_owned(),
        });
        let invariant = RecordOperationEventError::MissingOperation {
            operation_id: OperationId::try_new("op_network_repair").expect("operation id"),
        };

        assert!(retryable_record_failure(&storage));
        assert!(!retryable_record_failure(&invariant));
    }

    #[test]
    fn requested_target_missing_from_declared_projection_cannot_converge() {
        let projection =
            DataplaneProjection::try_new(Vec::new(), None).expect("empty projection is valid");
        let machine_id = machine_id("machine_a");

        assert_eq!(
            declared_projection_member(&projection, &machine_id),
            Err(NetworkRepairFailure::ProjectionMemberMissing {
                machine_id,
                revision: projection.declared_revision().clone(),
            })
        );
    }

    #[test]
    fn dns_refresh_check_distinguishes_silence_health_and_stale_cache() {
        let expected_machine_ids = vec![machine_id("machine_a")];
        let baseline = DnsRefreshBaseline {
            resolver_machine_id: machine_id("machine_b"),
            fact_watermarks: vec![fact_watermark("machine_a", 42, 1)],
        };
        let silence = dns_refresh_problem(
            &machine_id("machine_b"),
            Err(NatsJsonServiceRequestError::Request {
                failure: NatsServiceRequestFailure::NoResponders,
            }),
            &expected_machine_ids,
            &baseline,
        );
        let stale = dns_refresh_problem(
            &machine_id("machine_b"),
            Ok(DnsStatusRpcOk {
                machine_id: machine_id("machine_b"),
                value: InternalDnsStatus {
                    resolver: InternalDnsResolverStatus::Serving {
                        bound: "10.198.2.1:53".parse().expect("resolver address"),
                    },
                    fact_watermarks: vec![fact_watermark("machine_a", 42, 1)],
                    intent_health: InternalDnsIntentHealth::pending(),
                },
            }),
            &expected_machine_ids,
            &baseline,
        );
        let not_serving = dns_refresh_problem(
            &machine_id("machine_b"),
            Ok(DnsStatusRpcOk {
                machine_id: machine_id("machine_b"),
                value: InternalDnsStatus {
                    resolver: InternalDnsResolverStatus::AwaitingBind { attempts: 2 },
                    fact_watermarks: vec![fact_watermark("machine_a", 42, 2)],
                    intent_health: InternalDnsIntentHealth::pending(),
                },
            }),
            &expected_machine_ids,
            &baseline,
        );

        assert!(matches!(
            silence,
            Some(NetworkRepairDnsRefreshProblem::Unavailable {
                machine_id,
                failure: NetworkRepairRequestFailure::NoAnswer,
            })
                if machine_id == self::machine_id("machine_b")
        ));
        assert_eq!(
            stale,
            Some(NetworkRepairDnsRefreshProblem::Stale {
                machine_id: machine_id("machine_b"),
                stale_machine_ids: vec![machine_id("machine_a")],
            })
        );
        assert!(matches!(
            not_serving,
            Some(NetworkRepairDnsRefreshProblem::ResolverNotServing { machine_id })
                if machine_id == self::machine_id("machine_b")
        ));
    }

    #[test]
    fn dns_baseline_preserves_watermarks_while_resolver_is_starting() {
        let status = dns_status(
            &machine_id("machine_b"),
            Ok(DnsStatusRpcOk {
                machine_id: machine_id("machine_b"),
                value: InternalDnsStatus {
                    resolver: InternalDnsResolverStatus::AwaitingBind { attempts: 1 },
                    fact_watermarks: vec![fact_watermark("machine_a", 42, 1)],
                    intent_health: InternalDnsIntentHealth::pending(),
                },
            }),
        )
        .expect("a non-serving resolver still provides a refresh baseline");

        assert_eq!(
            status.fact_watermarks,
            vec![fact_watermark("machine_a", 42, 1)]
        );
    }

    #[test]
    fn dns_refresh_accepts_a_later_periodic_full_snapshot() {
        let baseline = DnsRefreshBaseline {
            resolver_machine_id: machine_id("machine_b"),
            fact_watermarks: vec![fact_watermark("machine_a", 42, 1)],
        };
        let problem = dns_refresh_problem(
            &machine_id("machine_b"),
            Ok(DnsStatusRpcOk {
                machine_id: machine_id("machine_b"),
                value: InternalDnsStatus {
                    resolver: InternalDnsResolverStatus::Serving {
                        bound: "10.198.2.1:53".parse().expect("resolver address"),
                    },
                    fact_watermarks: vec![fact_watermark("machine_a", 42, 3)],
                    intent_health: InternalDnsIntentHealth::pending(),
                },
            }),
            &[machine_id("machine_a")],
            &baseline,
        );

        assert_eq!(problem, None);
    }

    #[test]
    fn dns_refresh_accepts_first_snapshot_from_new_resolver_cache_incarnation() {
        let baseline = DnsRefreshBaseline {
            resolver_machine_id: machine_id("machine_b"),
            fact_watermarks: vec![fact_watermark_in_incarnation(
                "machine_a",
                42,
                "cache_before_restart",
                3,
            )],
        };
        let problem = dns_refresh_problem(
            &machine_id("machine_b"),
            Ok(DnsStatusRpcOk {
                machine_id: machine_id("machine_b"),
                value: InternalDnsStatus {
                    resolver: InternalDnsResolverStatus::Serving {
                        bound: "10.198.2.1:53".parse().expect("resolver address"),
                    },
                    fact_watermarks: vec![fact_watermark_in_incarnation(
                        "machine_a",
                        1,
                        "cache_after_restart",
                        1,
                    )],
                    intent_health: InternalDnsIntentHealth::pending(),
                },
            }),
            &[machine_id("machine_a")],
            &baseline,
        );

        assert_eq!(problem, None);
    }

    fn fact_watermark(
        machine_id_value: &str,
        observed_at_unix_ms: u64,
        generation: u64,
    ) -> InternalDnsFactWatermark {
        fact_watermark_in_incarnation(
            machine_id_value,
            observed_at_unix_ms,
            "cache_test",
            generation,
        )
    }

    fn fact_watermark_in_incarnation(
        machine_id_value: &str,
        observed_at_unix_ms: u64,
        incarnation: &str,
        generation: u64,
    ) -> InternalDnsFactWatermark {
        InternalDnsFactWatermark {
            machine_id: machine_id(machine_id_value),
            observed_at_unix_ms,
            resolver_cache_incarnation: InternalDnsResolverCacheIncarnation::try_new(incarnation)
                .expect("incarnation"),
            generation: InternalDnsFactGeneration::try_new(generation).expect("generation"),
        }
    }
}
