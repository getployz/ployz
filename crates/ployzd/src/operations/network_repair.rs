//! Operation-owned cluster dataplane repair.

use crate::intent::service::NatsIntentReader;
use crate::operation_api::admission::OperationControllers;
use crate::operations::deploy::MachineRuntimeUnavailableReason;
use crate::operations::log::{AcceptedNetworkRepairSubmission, RecordOperationEventError};
use crate::roles::dns::service::{DnsStatusRpcOk, DnsStatusRpcRequest};
use crate::roles::machine::client::{
    MAX_CONCURRENT_MACHINE_READS, MachineFactsRefreshError, NatsMachineDataplanePreparer,
    NatsMachineFactsReader,
};
use crate::tasks::TaskRegistry;
use futures_util::{StreamExt, stream};
use ployz_core::dataplane::{
    DataplaneMember, DataplanePrepareError, DataplanePrepareRequest, DataplaneProviderFailure,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::internal_dns::{InternalDnsFactWatermark, InternalDnsResolverStatus};
use ployz_core::ops::{
    FailureMessage, NetworkRepairDnsRefreshProblem, NetworkRepairEvidence, NetworkRepairFailure,
    NetworkRepairMachineFactsRefreshOutcome, NetworkRepairProgressPhase,
    NetworkRepairRequestFailure, NetworkRepairRunningStage, NetworkRepairTransition,
};
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::service_protocol::NatsServiceErrorCode;
use ployz_nats::service_runtime::{
    NatsJsonServiceRequestError, NatsServiceRequestFailure, request_json,
};
use std::time::Duration;

const DNS_REFRESH_POLL_INTERVAL: Duration = Duration::from_millis(100);
const DNS_STATUS_REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
const RECORD_ATTEMPTS: usize = 3;
const RECORD_RETRY_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct NetworkRepairOperation {
    controllers: OperationControllers,
    intent_reader: NatsIntentReader,
    dataplane: NatsMachineDataplanePreparer,
    facts_reader: NatsMachineFactsReader,
    client: async_nats::Client,
    operation_timeout: Duration,
    task_registry: TaskRegistry,
}

impl NetworkRepairOperation {
    #[must_use]
    pub fn new(
        controllers: OperationControllers,
        intent_reader: NatsIntentReader,
        dataplane: NatsMachineDataplanePreparer,
        facts_reader: NatsMachineFactsReader,
        client: async_nats::Client,
        operation_timeout: Duration,
        task_registry: TaskRegistry,
    ) -> Self {
        Self {
            controllers,
            intent_reader,
            dataplane,
            facts_reader,
            client,
            operation_timeout,
            task_registry,
        }
    }

    pub fn start(&self, accepted: AcceptedNetworkRepairSubmission) {
        if !accepted.should_start_execution {
            return;
        }
        let runtime = self.clone();
        self.task_registry.spawn(async move {
            runtime.run(accepted).await;
        });
    }

    pub async fn run(self, accepted: AcceptedNetworkRepairSubmission) {
        let operation_id = accepted.operation_id;
        if let Err(error) = self
            .record_transition_with_retry(
                &operation_id,
                NetworkRepairTransition::Running {
                    stage: NetworkRepairRunningStage::PreparingDataplane,
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
        let membership = intent
            .active_machines
            .iter()
            .map(|machine| DataplaneMember {
                machine_id: machine.machine_id.clone(),
                endpoint_subnet: machine.endpoint_subnet.clone(),
            })
            .collect::<Vec<_>>();
        if membership.is_empty() {
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
        let request = DataplanePrepareRequest {
            operation_id: operation_id.clone(),
            membership,
        };
        let report = match self
            .dataplane
            .prepare_dataplane_for_targets(request, &targets)
            .await
        {
            Ok(report) => report,
            Err(error) => {
                self.record_terminal(
                    &operation_id,
                    NetworkRepairTransition::Failed {
                        failure: network_repair_failure(error),
                    },
                )
                .await;
                return;
            }
        };
        if !self
            .record_evidence(
                &operation_id,
                NetworkRepairEvidence::DataplanePrepared { report },
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
        let watermarks = match self.refresh_machine_facts(&machine_ids).await {
            Ok(watermarks) => watermarks,
            Err(failure) => {
                self.record_terminal(&operation_id, NetworkRepairTransition::Failed { failure })
                    .await;
                return;
            }
        };
        if !self
            .record_evidence(
                &operation_id,
                NetworkRepairEvidence::MachineFactsRefreshed {
                    watermarks: watermarks.clone(),
                },
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
        let confirmed = match self.confirm_dns_refresh(&machine_ids, &watermarks).await {
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
    ) -> Result<Vec<InternalDnsFactWatermark>, NetworkRepairFailure> {
        let outcomes = stream::iter(machine_ids.iter().cloned())
            .map(|machine_id| async move {
                match self.facts_reader.refresh_machine_facts(&machine_id).await {
                    Ok(observed_at_unix_ms) => NetworkRepairMachineFactsRefreshOutcome::Refreshed {
                        machine_id: machine_id.clone(),
                        observed_at_unix_ms,
                    },
                    Err(error) => machine_facts_refresh_outcome(error),
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
                NetworkRepairMachineFactsRefreshOutcome::Refreshed {
                    machine_id,
                    observed_at_unix_ms,
                } => InternalDnsFactWatermark {
                    machine_id,
                    observed_at_unix_ms,
                },
                NetworkRepairMachineFactsRefreshOutcome::Unavailable { .. }
                | NetworkRepairMachineFactsRefreshOutcome::Failed { .. } => {
                    unreachable!("failed outcomes returned above")
                }
            })
            .collect())
    }

    async fn confirm_dns_refresh(
        &self,
        machine_ids: &[MachineId],
        watermarks: &[InternalDnsFactWatermark],
    ) -> Result<Vec<MachineId>, NetworkRepairFailure> {
        let deadline = tokio::time::Instant::now() + self.operation_timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let results = stream::iter(machine_ids.iter().cloned())
                .map(|machine_id| async move {
                    let result = request_json::<_, DnsStatusRpcOk>(
                        &self.client,
                        machine_service(&machine_id, MachineServiceEndpoint::DnsStatus),
                        &DnsStatusRpcRequest {},
                        DNS_STATUS_REQUEST_TIMEOUT.min(remaining),
                    )
                    .await;
                    (machine_id, result)
                })
                .buffer_unordered(MAX_CONCURRENT_MACHINE_READS)
                .collect::<Vec<_>>()
                .await;
            let mut confirmed_machine_ids = Vec::new();
            let mut problems = Vec::new();
            for (machine_id, result) in results {
                if let Some(problem) = dns_refresh_problem(&machine_id, result, watermarks) {
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
                Err(_) if attempts < RECORD_ATTEMPTS => {
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
                Err(_) if attempts < RECORD_ATTEMPTS => {
                    tokio::time::sleep(RECORD_RETRY_DELAY).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn network_repair_failure(error: DataplanePrepareError) -> NetworkRepairFailure {
    match error {
        DataplanePrepareError::Unavailable {
            machine_id,
            provider: DataplaneProviderFailure::PloyzNativeMesh { component },
            message,
        } => NetworkRepairFailure::DataplaneConvergenceFailed {
            machine_id,
            component,
            message,
        },
        DataplanePrepareError::InvalidReport { message } => {
            NetworkRepairFailure::DataplaneReportInvalid { message }
        }
    }
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
    let message = reason.failure_message();
    match reason {
        MachineRuntimeUnavailableReason::NoResponders => NetworkRepairRequestFailure::NoAnswer,
        MachineRuntimeUnavailableReason::RequestTimedOut
        | MachineRuntimeUnavailableReason::ServiceTimedOut { .. } => {
            NetworkRepairRequestFailure::TimedOut
        }
        MachineRuntimeUnavailableReason::MalformedServiceError { .. } => {
            NetworkRepairRequestFailure::ProtocolFailed { message }
        }
        MachineRuntimeUnavailableReason::DecodeResponse { .. } => {
            NetworkRepairRequestFailure::DecodeFailed { message }
        }
        MachineRuntimeUnavailableReason::WrongResponder { actual_machine_id } => {
            NetworkRepairRequestFailure::WrongResponder { actual_machine_id }
        }
        MachineRuntimeUnavailableReason::EncodeRequest { .. }
        | MachineRuntimeUnavailableReason::InvalidSubject
        | MachineRuntimeUnavailableReason::MaxPayloadExceeded
        | MachineRuntimeUnavailableReason::RequestFailed { .. }
        | MachineRuntimeUnavailableReason::ServiceBadRequest { .. }
        | MachineRuntimeUnavailableReason::ServiceConflict { .. }
        | MachineRuntimeUnavailableReason::ServiceUnavailable { .. }
        | MachineRuntimeUnavailableReason::ServiceInternal { .. } => {
            NetworkRepairRequestFailure::RequestFailed { message }
        }
    }
}

fn dns_request_failure(error: NatsJsonServiceRequestError) -> NetworkRepairRequestFailure {
    match error {
        NatsJsonServiceRequestError::Request {
            failure: NatsServiceRequestFailure::NoResponders,
        } => NetworkRepairRequestFailure::NoAnswer,
        NatsJsonServiceRequestError::Request {
            failure: NatsServiceRequestFailure::TimedOut,
        }
        | NatsJsonServiceRequestError::Service {
            failure:
                ployz_nats::service_protocol::NatsServiceError {
                    code: NatsServiceErrorCode::Timeout,
                    ..
                },
        } => NetworkRepairRequestFailure::TimedOut,
        NatsJsonServiceRequestError::ServiceProtocol { error } => {
            NetworkRepairRequestFailure::ProtocolFailed {
                message: failure_message(error.to_string()),
            }
        }
        NatsJsonServiceRequestError::DecodeResponse { message } => {
            NetworkRepairRequestFailure::DecodeFailed {
                message: failure_message(message),
            }
        }
        error @ NatsJsonServiceRequestError::EncodeRequest { .. }
        | error @ NatsJsonServiceRequestError::Request {
            failure:
                NatsServiceRequestFailure::InvalidSubject
                | NatsServiceRequestFailure::MaxPayloadExceeded
                | NatsServiceRequestFailure::Other { .. },
        }
        | error @ NatsJsonServiceRequestError::Service {
            failure:
                ployz_nats::service_protocol::NatsServiceError {
                    code:
                        NatsServiceErrorCode::BadRequest
                        | NatsServiceErrorCode::Conflict
                        | NatsServiceErrorCode::Unavailable
                        | NatsServiceErrorCode::Internal,
                    ..
                },
        } => NetworkRepairRequestFailure::RequestFailed {
            message: failure_message(error.to_string()),
        },
    }
}

fn stale_machine_ids(
    observed: &[InternalDnsFactWatermark],
    expected: &[InternalDnsFactWatermark],
) -> Vec<MachineId> {
    expected
        .iter()
        .filter(|expected| {
            !observed.iter().any(|observed| {
                observed.machine_id == expected.machine_id
                    && observed.observed_at_unix_ms >= expected.observed_at_unix_ms
            })
        })
        .map(|expected| expected.machine_id.clone())
        .collect()
}

fn dns_refresh_problem(
    machine_id: &MachineId,
    result: Result<DnsStatusRpcOk, NatsJsonServiceRequestError>,
    expected: &[InternalDnsFactWatermark],
) -> Option<NetworkRepairDnsRefreshProblem> {
    let status = match result {
        Ok(status) if status.machine_id == *machine_id => status.value,
        Ok(status) => {
            return Some(NetworkRepairDnsRefreshProblem::Unavailable {
                machine_id: machine_id.clone(),
                failure: NetworkRepairRequestFailure::WrongResponder {
                    actual_machine_id: status.machine_id,
                },
            });
        }
        Err(error) => {
            return Some(NetworkRepairDnsRefreshProblem::Unavailable {
                machine_id: machine_id.clone(),
                failure: dns_request_failure(error),
            });
        }
    };
    if !matches!(status.resolver, InternalDnsResolverStatus::Serving { .. }) {
        return Some(NetworkRepairDnsRefreshProblem::ResolverNotServing {
            machine_id: machine_id.clone(),
        });
    }
    let stale_machine_ids = stale_machine_ids(&status.fact_watermarks, expected);
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
    eprintln!(
        "ployzd network repair warning: phase={phase} operation_id={} error={error}",
        operation_id.as_str()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::internal_dns::InternalDnsStatus;

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    #[test]
    fn dns_refresh_check_distinguishes_silence_health_and_stale_cache() {
        let expected = vec![InternalDnsFactWatermark {
            machine_id: machine_id("machine_a"),
            observed_at_unix_ms: 42,
        }];
        let silence = dns_refresh_problem(
            &machine_id("machine_b"),
            Err(NatsJsonServiceRequestError::Request {
                failure: NatsServiceRequestFailure::NoResponders,
            }),
            &expected,
        );
        let stale = dns_refresh_problem(
            &machine_id("machine_b"),
            Ok(DnsStatusRpcOk {
                machine_id: machine_id("machine_b"),
                value: InternalDnsStatus {
                    resolver: InternalDnsResolverStatus::Serving {
                        bound: "10.198.2.1:53".parse().expect("resolver address"),
                    },
                    fact_watermarks: vec![InternalDnsFactWatermark {
                        machine_id: machine_id("machine_a"),
                        observed_at_unix_ms: 41,
                    }],
                },
            }),
            &expected,
        );
        let not_serving = dns_refresh_problem(
            &machine_id("machine_b"),
            Ok(DnsStatusRpcOk {
                machine_id: machine_id("machine_b"),
                value: InternalDnsStatus {
                    resolver: InternalDnsResolverStatus::AwaitingBind { attempts: 2 },
                    fact_watermarks: vec![InternalDnsFactWatermark {
                        machine_id: machine_id("machine_a"),
                        observed_at_unix_ms: 42,
                    }],
                },
            }),
            &expected,
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
}
