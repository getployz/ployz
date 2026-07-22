//! Operation-owned machine-local ZFS preparation.

use std::time::{Duration, Instant};

use crate::control::operation_evidence::{
    AcceptedMachineStoragePrepareSubmission, RecordOperationEventError,
};
use crate::control::role_client::machine::{
    MachineStoragePrepareError, NatsMachineSubstrateUpdater,
};
use crate::control::sequencer::OperationControllers;
use crate::roles::machine::MachineRuntimeUnavailableReason;
use crate::roles::machine::protocol::{
    MachineStoragePrepareCancelRpcRequest, MachineStoragePrepareReport,
    MachineStoragePrepareRpcRequest,
};
use crate::tasks::TaskSpawner;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::{
    FailureMessage, MachineStoragePrepareFailure, MachineStoragePrepareTransition,
};

const REPORT_POLL_INTERVAL: Duration = Duration::from_secs(1);
const REPORT_NO_TESTIMONY_BUDGET: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct MachineStoragePrepareOperation {
    controllers: OperationControllers,
    updater: NatsMachineSubstrateUpdater,
    task_registry: TaskSpawner,
}

impl MachineStoragePrepareOperation {
    #[must_use]
    pub const fn new(
        controllers: OperationControllers,
        updater: NatsMachineSubstrateUpdater,
        task_registry: TaskSpawner,
    ) -> Self {
        Self {
            controllers,
            updater,
            task_registry,
        }
    }

    pub async fn start(&self, accepted: AcceptedMachineStoragePrepareSubmission) {
        if !accepted.should_start_execution {
            return;
        }
        let operation_id = accepted.operation_id.clone();
        let lease = self
            .controllers
            .machine_substrate_lease(accepted.machine_id.clone(), accepted.operation_id.clone());
        let runtime = self.clone();
        let admission = self.task_registry.spawn(move || async move {
            let _lease = lease;
            runtime.run(accepted).await;
        });
        super::finish_rejected_task_admission(&self.controllers, &operation_id, admission).await;
    }

    pub async fn cancel_machine_effect(
        &self,
        machine_id: &MachineId,
        operation_id: &OperationId,
        reason: ployz_core::operation::CancellationReason,
    ) -> Result<(), MachineStoragePrepareError> {
        self.updater
            .cancel_storage_prepare(
                machine_id,
                MachineStoragePrepareCancelRpcRequest {
                    operation_id: operation_id.clone(),
                    reason,
                },
            )
            .await
    }

    async fn run(self, accepted: AcceptedMachineStoragePrepareSubmission) {
        self.clone().run_inner(accepted).await;
    }

    async fn run_inner(self, accepted: AcceptedMachineStoragePrepareSubmission) {
        let operation_id = accepted.operation_id;
        let machine_id = accepted.machine_id;
        if let Err(error) = self
            .controllers
            .repository()
            .record_machine_storage_prepare_transition(
                &operation_id,
                &machine_id,
                MachineStoragePrepareTransition::Preparing,
            )
            .await
        {
            self.record_failed(
                &operation_id,
                &machine_id,
                MachineStoragePrepareFailure::StateCommitFailed {
                    machine_id: machine_id.clone(),
                    message: event_failure(error),
                },
            )
            .await;
            return;
        }
        match self
            .updater
            .prepare_storage(
                &machine_id,
                MachineStoragePrepareRpcRequest {
                    operation_id: operation_id.clone(),
                    pool: accepted.requested_pool,
                },
            )
            .await
        {
            Ok(()) => {}
            Err(MachineStoragePrepareError::PreparationFailed {
                machine_id,
                failure,
            }) => {
                self.record_failed(
                    &operation_id,
                    &machine_id,
                    MachineStoragePrepareFailure::PreparationRejected {
                        machine_id: machine_id.clone(),
                        failure,
                    },
                )
                .await;
                return;
            }
            Err(MachineStoragePrepareError::Busy {
                machine_id,
                owner_operation_id,
            }) => {
                self.record_failed(
                    &operation_id,
                    &machine_id,
                    MachineStoragePrepareFailure::MachineSubstrateBusy {
                        machine_id: machine_id.clone(),
                        owner_operation_id,
                    },
                )
                .await;
                return;
            }
            Err(MachineStoragePrepareError::Unavailable { reason, .. }) => {
                if !storage_report_retryable(&reason) {
                    self.record_failed(
                        &operation_id,
                        &machine_id,
                        MachineStoragePrepareFailure::MachineUnavailable {
                            machine_id: machine_id.clone(),
                            message: reason.failure_message(),
                        },
                    )
                    .await;
                    return;
                }
            }
        }
        let mut poll = StoragePrepareReportPoll::new(Instant::now());
        loop {
            let report = self
                .updater
                .report_storage_prepare(&machine_id, &operation_id)
                .await;
            match poll.observe(report, Instant::now(), &machine_id) {
                ReportPollDecision::Continue => {
                    tokio::time::sleep(REPORT_POLL_INTERVAL).await;
                }
                ReportPollDecision::Terminal(transition) => {
                    self.record_terminal(&operation_id, &machine_id, transition)
                        .await;
                    return;
                }
                ReportPollDecision::Failed(failure) => {
                    self.record_failed(&operation_id, &machine_id, failure)
                        .await;
                    return;
                }
            }
        }
    }

    async fn record_terminal(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        transition: MachineStoragePrepareTransition,
    ) {
        if let Err(error) = self
            .controllers
            .repository()
            .record_machine_storage_prepare_transition(operation_id, machine_id, transition)
            .await
        {
            self.record_failed(
                operation_id,
                machine_id,
                MachineStoragePrepareFailure::StateCommitFailed {
                    machine_id: machine_id.clone(),
                    message: event_failure(error),
                },
            )
            .await;
        }
    }

    async fn record_failed(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        failure: MachineStoragePrepareFailure,
    ) {
        let _ = self
            .controllers
            .repository()
            .record_machine_storage_prepare_transition(
                operation_id,
                machine_id,
                MachineStoragePrepareTransition::Failed { failure },
            )
            .await;
    }
}

struct StoragePrepareReportPoll {
    no_testimony_deadline: Instant,
}

impl StoragePrepareReportPoll {
    fn new(now: Instant) -> Self {
        Self {
            no_testimony_deadline: now + REPORT_NO_TESTIMONY_BUDGET,
        }
    }

    fn observe(
        &mut self,
        report: Result<MachineStoragePrepareReport, MachineStoragePrepareError>,
        now: Instant,
        machine_id: &MachineId,
    ) -> ReportPollDecision {
        match report {
            Ok(MachineStoragePrepareReport::Running) => {
                self.no_testimony_deadline = now + REPORT_NO_TESTIMONY_BUDGET;
                ReportPollDecision::Continue
            }
            Ok(MachineStoragePrepareReport::Completed { pool }) => {
                ReportPollDecision::Terminal(MachineStoragePrepareTransition::Completed { pool })
            }
            Ok(MachineStoragePrepareReport::Failed { failure }) => {
                ReportPollDecision::Failed(MachineStoragePrepareFailure::PreparationRejected {
                    machine_id: machine_id.clone(),
                    failure,
                })
            }
            Ok(MachineStoragePrepareReport::Cancelled { reason }) => {
                ReportPollDecision::Terminal(MachineStoragePrepareTransition::Cancelled { reason })
            }
            Ok(MachineStoragePrepareReport::NotFound) => {
                ReportPollDecision::Failed(MachineStoragePrepareFailure::EvidenceUnavailable {
                    machine_id: machine_id.clone(),
                    message: storage_failure_message(
                        "accepted storage preparation has no machine evidence",
                    ),
                })
            }
            Err(MachineStoragePrepareError::Unavailable { reason, .. })
                if storage_report_retryable(&reason) && now < self.no_testimony_deadline =>
            {
                ReportPollDecision::Continue
            }
            Err(MachineStoragePrepareError::Unavailable { reason, .. })
                if storage_report_retryable(&reason) =>
            {
                ReportPollDecision::Failed(MachineStoragePrepareFailure::EvidenceUnavailable {
                    machine_id: machine_id.clone(),
                    message: storage_failure_message(
                        "storage preparation testimony was unavailable before the bounded deadline",
                    ),
                })
            }
            Err(error) => ReportPollDecision::Failed(operation_failure(error)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReportPollDecision {
    Continue,
    Terminal(MachineStoragePrepareTransition),
    Failed(MachineStoragePrepareFailure),
}

fn operation_failure(error: MachineStoragePrepareError) -> MachineStoragePrepareFailure {
    match error {
        MachineStoragePrepareError::Unavailable { machine_id, reason } => {
            MachineStoragePrepareFailure::MachineUnavailable {
                machine_id,
                message: reason.failure_message(),
            }
        }
        MachineStoragePrepareError::PreparationFailed {
            machine_id,
            failure,
        } => MachineStoragePrepareFailure::PreparationRejected {
            machine_id,
            failure,
        },
        MachineStoragePrepareError::Busy {
            machine_id,
            owner_operation_id,
        } => MachineStoragePrepareFailure::MachineSubstrateBusy {
            machine_id,
            owner_operation_id,
        },
    }
}

fn event_failure(error: RecordOperationEventError) -> FailureMessage {
    FailureMessage::try_new(format!(
        "failed to record storage preparation event: {error}"
    ))
    .expect("event failure is non-empty")
}

fn storage_failure_message(message: &str) -> FailureMessage {
    FailureMessage::try_new(message).expect("storage failure message is non-empty")
}

fn storage_report_retryable(reason: &MachineRuntimeUnavailableReason) -> bool {
    match reason {
        MachineRuntimeUnavailableReason::RequestTimedOut
        | MachineRuntimeUnavailableReason::RequestFailed { .. }
        | MachineRuntimeUnavailableReason::ServiceUnavailable { .. }
        | MachineRuntimeUnavailableReason::ServiceTimedOut { .. }
        | MachineRuntimeUnavailableReason::ServiceInternal { .. } => true,
        MachineRuntimeUnavailableReason::EncodeRequest { .. }
        | MachineRuntimeUnavailableReason::NoResponders
        | MachineRuntimeUnavailableReason::InvalidSubject
        | MachineRuntimeUnavailableReason::MaxPayloadExceeded
        | MachineRuntimeUnavailableReason::ServiceBadRequest { .. }
        | MachineRuntimeUnavailableReason::ServiceConflict { .. }
        | MachineRuntimeUnavailableReason::ServiceResponseTooLarge
        | MachineRuntimeUnavailableReason::MalformedServiceError { .. }
        | MachineRuntimeUnavailableReason::DecodeResponse { .. }
        | MachineRuntimeUnavailableReason::WrongResponder { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::ZfsPoolName;
    use ployz_core::operation::CancellationReason;
    use ployz_core::storage::StorageEffectFailure;

    fn machine_id() -> MachineId {
        MachineId::try_new("machine_a").expect("machine id")
    }

    #[test]
    fn accepted_running_then_completed_is_the_only_happy_path() {
        let now = Instant::now();
        let mut poll = StoragePrepareReportPoll::new(now);
        assert_eq!(
            poll.observe(Ok(MachineStoragePrepareReport::Running), now, &machine_id()),
            ReportPollDecision::Continue
        );
        let pool = ZfsPoolName::try_new("tank").expect("pool");
        assert_eq!(
            poll.observe(
                Ok(MachineStoragePrepareReport::Completed { pool: pool.clone() }),
                now + Duration::from_secs(1),
                &machine_id(),
            ),
            ReportPollDecision::Terminal(MachineStoragePrepareTransition::Completed { pool })
        );
    }

    #[test]
    fn transient_report_failure_is_bounded_and_running_refreshes_testimony_only() {
        let now = Instant::now();
        let mut poll = StoragePrepareReportPoll::new(now);
        let unavailable = || MachineStoragePrepareError::Unavailable {
            machine_id: machine_id(),
            reason: MachineRuntimeUnavailableReason::RequestTimedOut,
        };
        assert_eq!(
            poll.observe(Err(unavailable()), now, &machine_id()),
            ReportPollDecision::Continue
        );
        let running_at = now + REPORT_NO_TESTIMONY_BUDGET - Duration::from_secs(1);
        assert_eq!(
            poll.observe(
                Ok(MachineStoragePrepareReport::Running),
                running_at,
                &machine_id()
            ),
            ReportPollDecision::Continue
        );
        assert_eq!(
            poll.no_testimony_deadline,
            running_at + REPORT_NO_TESTIMONY_BUDGET
        );
        assert!(matches!(
            poll.observe(
                Err(unavailable()),
                poll.no_testimony_deadline,
                &machine_id()
            ),
            ReportPollDecision::Failed(MachineStoragePrepareFailure::EvidenceUnavailable { .. })
        ));
    }

    #[test]
    fn not_found_failed_and_cancelled_are_distinct_terminal_outcomes() {
        let now = Instant::now();
        let machine_id = machine_id();
        assert!(matches!(
            StoragePrepareReportPoll::new(now).observe(
                Ok(MachineStoragePrepareReport::NotFound),
                now,
                &machine_id
            ),
            ReportPollDecision::Failed(MachineStoragePrepareFailure::EvidenceUnavailable { .. })
        ));
        let failure = StorageEffectFailure::OperationTimedOut;
        assert_eq!(
            StoragePrepareReportPoll::new(now).observe(
                Ok(MachineStoragePrepareReport::Failed {
                    failure: failure.clone()
                }),
                now,
                &machine_id,
            ),
            ReportPollDecision::Failed(MachineStoragePrepareFailure::PreparationRejected {
                machine_id: machine_id.clone(),
                failure,
            })
        );
        let reason = CancellationReason::try_new("operator cancelled").expect("reason");
        assert_eq!(
            StoragePrepareReportPoll::new(now).observe(
                Ok(MachineStoragePrepareReport::Cancelled {
                    reason: reason.clone()
                }),
                now,
                &machine_id,
            ),
            ReportPollDecision::Terminal(MachineStoragePrepareTransition::Cancelled { reason })
        );
    }

    #[test]
    fn report_recovery_fails_fast_for_permanent_unavailability() {
        assert!(!storage_report_retryable(
            &MachineRuntimeUnavailableReason::NoResponders
        ));
        assert!(!storage_report_retryable(
            &MachineRuntimeUnavailableReason::InvalidSubject
        ));
        assert!(storage_report_retryable(
            &MachineRuntimeUnavailableReason::RequestTimedOut
        ));
    }
}
