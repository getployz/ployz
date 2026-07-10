//! Operation-owned cluster dataplane repair.

use crate::intent::service::NatsIntentReader;
use crate::operation_api::admission::OperationControllers;
use crate::operations::log::{AcceptedNetworkRepairSubmission, RecordOperationEventError};
use crate::roles::dns::service::{DnsStatusRpcOk, DnsStatusRpcRequest};
use crate::roles::machine::client::{
    MachineFactsRefreshError, NatsMachineDataplanePreparer, NatsMachineFactsReader,
};
use crate::tasks::TaskRegistry;
use futures_util::future::{join_all, try_join_all};
use ployz_core::dataplane::{
    DataplaneMember, DataplanePrepareError, DataplanePrepareRequest, DataplaneProviderFailure,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::internal_dns::{InternalDnsFactWatermark, InternalDnsResolverStatus};
use ployz_core::ops::{
    FailureMessage, NetworkRepairEvidence, NetworkRepairFailure, NetworkRepairMachineFactWatermark,
    NetworkRepairRunningStage, NetworkRepairTransition,
};
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::service_runtime::request_json;
use std::time::Duration;

const DNS_REFRESH_POLL_INTERVAL: Duration = Duration::from_millis(100);
const DNS_STATUS_REQUEST_TIMEOUT: Duration = Duration::from_secs(1);

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
            .controllers
            .repository()
            .record_network_repair_transition(
                &operation_id,
                NetworkRepairTransition::Running {
                    stage: NetworkRepairRunningStage::PreparingDataplane,
                },
            )
            .await
        {
            record_warning(&operation_id, "record-running", &error);
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
                "record-dataplane-prepared",
            )
            .await
        {
            return;
        }
        if !self
            .record_stage(
                &operation_id,
                NetworkRepairRunningStage::RefreshingMachineFacts,
            )
            .await
        {
            return;
        }
        let watermarks = match self
            .refresh_machine_facts(&operation_id, &machine_ids)
            .await
        {
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
                "record-machine-facts-refreshed",
            )
            .await
        {
            return;
        }
        if !self
            .record_stage(
                &operation_id,
                NetworkRepairRunningStage::ConfirmingDnsRefresh,
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
                "record-dns-refresh-confirmed",
            )
            .await
        {
            return;
        }
        self.record_terminal(&operation_id, NetworkRepairTransition::Completed)
            .await;
    }

    async fn record_stage(
        &self,
        operation_id: &OperationId,
        stage: NetworkRepairRunningStage,
    ) -> bool {
        match self
            .controllers
            .repository()
            .record_network_repair_transition(
                operation_id,
                NetworkRepairTransition::Running { stage },
            )
            .await
        {
            Ok(_) => true,
            Err(error) => {
                record_warning(operation_id, "record-running", &error);
                false
            }
        }
    }

    async fn record_evidence(
        &self,
        operation_id: &OperationId,
        evidence: NetworkRepairEvidence,
        phase: &str,
    ) -> bool {
        match self
            .controllers
            .repository()
            .record_network_repair_evidence(operation_id, evidence)
            .await
        {
            Ok(_) => true,
            Err(error) => {
                record_warning(operation_id, phase, &error);
                false
            }
        }
    }

    async fn refresh_machine_facts(
        &self,
        operation_id: &OperationId,
        machine_ids: &[MachineId],
    ) -> Result<Vec<NetworkRepairMachineFactWatermark>, NetworkRepairFailure> {
        try_join_all(machine_ids.iter().map(|machine_id| async move {
            self.facts_reader
                .refresh_machine_facts(machine_id, operation_id.clone())
                .await
                .map(|observed_at_unix_ms| NetworkRepairMachineFactWatermark {
                    machine_id: machine_id.clone(),
                    observed_at_unix_ms,
                })
                .map_err(machine_facts_refresh_failure)
        }))
        .await
    }

    async fn confirm_dns_refresh(
        &self,
        machine_ids: &[MachineId],
        watermarks: &[NetworkRepairMachineFactWatermark],
    ) -> Result<Vec<MachineId>, NetworkRepairFailure> {
        let deadline = tokio::time::Instant::now() + self.operation_timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let results = join_all(machine_ids.iter().map(|machine_id| async move {
                let result = request_json::<_, DnsStatusRpcOk>(
                    &self.client,
                    machine_service(machine_id, MachineServiceEndpoint::DnsStatus),
                    &DnsStatusRpcRequest {},
                    DNS_STATUS_REQUEST_TIMEOUT.min(remaining),
                )
                .await;
                (machine_id, result)
            }))
            .await;
            let mut unavailable = None;
            let mut stale = None;
            for (machine_id, result) in results {
                if let Some(pending) = dns_refresh_pending(
                    machine_id,
                    result.map_err(|error| error.to_string()),
                    watermarks,
                ) {
                    match pending {
                        pending @ DnsRefreshPending::Unavailable { .. } => {
                            unavailable = Some(pending);
                        }
                        pending @ DnsRefreshPending::Stale { .. } => stale = Some(pending),
                    }
                }
            }
            let pending = unavailable.or(stale);
            let Some(pending) = pending else {
                return Ok(machine_ids.to_vec());
            };
            if tokio::time::Instant::now() >= deadline {
                return Err(pending.into_failure());
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
        if let Err(error) = self
            .controllers
            .repository()
            .record_network_repair_transition(operation_id, transition)
            .await
        {
            record_warning(operation_id, "record-terminal", &error);
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

fn machine_facts_refresh_failure(error: MachineFactsRefreshError) -> NetworkRepairFailure {
    match error {
        MachineFactsRefreshError::Unavailable { machine_id, reason } => {
            NetworkRepairFailure::MachineFactsRefreshUnavailable {
                machine_id,
                message: reason.failure_message(),
            }
        }
        MachineFactsRefreshError::RefreshFailed {
            machine_id,
            message,
        } => NetworkRepairFailure::MachineFactsRefreshFailed {
            machine_id,
            message,
        },
    }
}

fn stale_machine_ids(
    observed: &[InternalDnsFactWatermark],
    expected: &[NetworkRepairMachineFactWatermark],
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum DnsRefreshPending {
    Unavailable {
        machine_id: MachineId,
        message: FailureMessage,
    },
    Stale {
        machine_id: MachineId,
        stale_machine_ids: Vec<MachineId>,
    },
}

impl DnsRefreshPending {
    fn into_failure(self) -> NetworkRepairFailure {
        match self {
            Self::Unavailable {
                machine_id,
                message,
            } => NetworkRepairFailure::DnsRefreshUnavailable {
                machine_id,
                message,
            },
            Self::Stale {
                machine_id,
                stale_machine_ids,
            } => NetworkRepairFailure::DnsRefreshStale {
                machine_id,
                stale_machine_ids,
            },
        }
    }
}

fn dns_refresh_pending(
    machine_id: &MachineId,
    result: Result<DnsStatusRpcOk, String>,
    expected: &[NetworkRepairMachineFactWatermark],
) -> Option<DnsRefreshPending> {
    let status = match result {
        Ok(status) if status.machine_id == *machine_id => status.value,
        Ok(status) => {
            return Some(DnsRefreshPending::Unavailable {
                machine_id: machine_id.clone(),
                message: failure_message(format!(
                    "DNS status answered for {}",
                    status.machine_id.as_str()
                )),
            });
        }
        Err(message) => {
            return Some(DnsRefreshPending::Unavailable {
                machine_id: machine_id.clone(),
                message: failure_message(message),
            });
        }
    };
    if !matches!(status.resolver, InternalDnsResolverStatus::Serving { .. }) {
        return Some(DnsRefreshPending::Unavailable {
            machine_id: machine_id.clone(),
            message: failure_message("internal DNS resolver is not serving".to_owned()),
        });
    }
    let stale_machine_ids = stale_machine_ids(&status.fact_watermarks, expected);
    if stale_machine_ids.is_empty() {
        None
    } else {
        Some(DnsRefreshPending::Stale {
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
        let expected = vec![NetworkRepairMachineFactWatermark {
            machine_id: machine_id("machine_a"),
            observed_at_unix_ms: 42,
        }];
        let silence = dns_refresh_pending(
            &machine_id("machine_b"),
            Err("machine runtime has no responders".to_owned()),
            &expected,
        );
        let stale = dns_refresh_pending(
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
        let not_serving = dns_refresh_pending(
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
            Some(DnsRefreshPending::Unavailable { machine_id, .. })
                if machine_id == self::machine_id("machine_b")
        ));
        assert_eq!(
            stale,
            Some(DnsRefreshPending::Stale {
                machine_id: machine_id("machine_b"),
                stale_machine_ids: vec![machine_id("machine_a")],
            })
        );
        assert!(matches!(
            not_serving,
            Some(DnsRefreshPending::Unavailable { machine_id, message })
                if machine_id == self::machine_id("machine_b")
                    && message.as_str().contains("not serving")
        ));
    }
}
