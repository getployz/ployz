use crate::control::operation_evidence::{
    AcceptedMachineBuildCachePruneSubmission, RecordOperationEventError,
};
use crate::control::role_client::machine::{
    MachineBuildCachePruneStartError, MachineBuildCachePruneStatusError,
    NatsMachineSubstrateUpdater,
};
use crate::control::sequencer::OperationControllers;
use crate::tasks::TaskSpawner;
use ployz_core::build::{
    BUILD_CACHE_PRUNE_FINAL_RECONCILE_TIMEOUT, BUILD_CACHE_PRUNE_POLL_INTERVAL,
    BUILD_CACHE_PRUNE_QUEUED_STALL_TIMEOUT, BUILD_CACHE_PRUNE_RUNNING_STALL_TIMEOUT,
    BuildCachePruneStatus,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::{
    BuildCachePruneEvidence, CancellationReason, FailureMessage, MachineBuildCachePruneFailure,
    MachineBuildCachePruneTransition,
};
use std::time::Duration;
use tokio::time::Instant;

#[derive(Debug, Clone)]
pub struct MachineBuildCachePruneOperation {
    controllers: OperationControllers,
    updater: NatsMachineSubstrateUpdater,
    task_registry: TaskSpawner,
}

impl MachineBuildCachePruneOperation {
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

    pub async fn start(&self, accepted: AcceptedMachineBuildCachePruneSubmission) {
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

    #[tracing::instrument(name = "operation", skip_all, fields(kind = "machine_build_cache_prune", operation_id = accepted.operation_id.as_str()))]
    async fn run(self, accepted: AcceptedMachineBuildCachePruneSubmission) {
        let operation_id = accepted.operation_id;
        let machine_id = accepted.machine_id;
        if let Err(error) = self
            .controllers
            .repository()
            .record_machine_build_cache_prune_transition(
                &operation_id,
                &machine_id,
                MachineBuildCachePruneTransition::Pruning,
            )
            .await
        {
            self.record_failed(
                &operation_id,
                &machine_id,
                commit_failure(&machine_id, error),
            )
            .await;
            return;
        }
        let completion = match self
            .updater
            .start_build_cache_prune(&machine_id, &operation_id)
            .await
        {
            Ok(_) => self.poll(&machine_id, &operation_id).await,
            Err(error @ MachineBuildCachePruneStartError::Unavailable { .. }) => {
                let fallback = start_failure(error);
                self.cancel_and_reconcile(&machine_id, &operation_id, fallback)
                    .await
            }
            Err(error) => PruneCompletion::Failed(start_failure(error)),
        };
        let transition = match completion {
            PruneCompletion::Completed(evidence) => {
                MachineBuildCachePruneTransition::Completed { evidence }
            }
            PruneCompletion::Failed(failure) => {
                MachineBuildCachePruneTransition::Failed { failure }
            }
            PruneCompletion::Cancelled(reason) => {
                MachineBuildCachePruneTransition::Cancelled { reason }
            }
        };
        if let Err(error) = self
            .controllers
            .repository()
            .record_machine_build_cache_prune_transition(&operation_id, &machine_id, transition)
            .await
        {
            self.record_failed(
                &operation_id,
                &machine_id,
                commit_failure(&machine_id, error),
            )
            .await;
        }
    }

    async fn poll(&self, machine_id: &MachineId, operation_id: &OperationId) -> PruneCompletion {
        let mut progress = PruneProgressDeadline::new(Instant::now());
        loop {
            if progress.is_stalled(Instant::now()) {
                return self
                    .cancel_and_reconcile(machine_id, operation_id, stalled_failure(machine_id))
                    .await;
            }
            match tokio::time::timeout_at(
                progress.deadline,
                self.updater
                    .build_cache_prune_status(machine_id, operation_id),
            )
            .await
            {
                Err(_) => {
                    return self
                        .cancel_and_reconcile(machine_id, operation_id, stalled_failure(machine_id))
                        .await;
                }
                Ok(result) => match result {
                    Ok(status) => match classify_status(machine_id, status) {
                        StatusDisposition::Waiting {
                            progress: next,
                            budget,
                        } => {
                            let now = Instant::now();
                            if !progress.observe(next, budget, now) && progress.is_stalled(now) {
                                return self
                                    .cancel_and_reconcile(
                                        machine_id,
                                        operation_id,
                                        stalled_failure(machine_id),
                                    )
                                    .await;
                            }
                        }
                        StatusDisposition::Terminal(completion) => return completion,
                        StatusDisposition::NotFound => {
                            return self
                                .cancel_and_reconcile(
                                    machine_id,
                                    operation_id,
                                    not_found_failure(machine_id),
                                )
                                .await;
                        }
                    },
                    Err(error) => {
                        let silence = status_error_is_silence(&error);
                        let failure = status_failure(error);
                        if !silence || progress.is_stalled(Instant::now()) {
                            return self
                                .cancel_and_reconcile(machine_id, operation_id, failure)
                                .await;
                        }
                    }
                },
            }
            let now = Instant::now();
            tokio::time::sleep_until(
                (now + BUILD_CACHE_PRUNE_POLL_INTERVAL).min(progress.deadline),
            )
            .await;
        }
    }

    async fn cancel_and_reconcile(
        &self,
        machine_id: &MachineId,
        operation_id: &OperationId,
        fallback: MachineBuildCachePruneFailure,
    ) -> PruneCompletion {
        if let Err(error) = self
            .updater
            .cancel_build_cache_prune(machine_id, operation_id)
            .await
        {
            tracing::warn!(
                operation_id = operation_id.as_str(),
                machine_id = machine_id.as_str(),
                error = ?error,
                "build cache prune cancel request failed; reconciling from status"
            );
        }
        let deadline = Instant::now() + BUILD_CACHE_PRUNE_FINAL_RECONCILE_TIMEOUT;
        loop {
            let disposition = match tokio::time::timeout_at(
                deadline,
                self.updater
                    .build_cache_prune_status(machine_id, operation_id),
            )
            .await
            {
                Err(_) => return PruneCompletion::Failed(fallback),
                Ok(Ok(status)) => Some(classify_status(machine_id, status)),
                Ok(Err(error)) if status_error_is_silence(&error) => None,
                Ok(Err(_)) => return PruneCompletion::Failed(fallback),
            };
            let now = Instant::now();
            match final_reconcile_decision(disposition, now, deadline) {
                FinalReconcileDecision::Complete(completion) => return completion,
                FinalReconcileDecision::Fallback => return PruneCompletion::Failed(fallback),
                FinalReconcileDecision::Wait => {}
            }
            tokio::time::sleep_until((now + BUILD_CACHE_PRUNE_POLL_INTERVAL).min(deadline)).await;
        }
    }

    async fn record_failed(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        failure: MachineBuildCachePruneFailure,
    ) {
        if let Err(error) = self
            .controllers
            .repository()
            .record_machine_build_cache_prune_transition(
                operation_id,
                machine_id,
                MachineBuildCachePruneTransition::Failed { failure },
            )
            .await
        {
            tracing::error!(
                operation_id = operation_id.as_str(),
                machine_id = machine_id.as_str(),
                error = %error,
                "build cache prune failed and its terminal transition could not be recorded"
            );
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PruneCompletion {
    Completed(BuildCachePruneEvidence),
    Failed(MachineBuildCachePruneFailure),
    Cancelled(CancellationReason),
}

#[derive(Debug, PartialEq, Eq)]
enum StatusDisposition {
    NotFound,
    Waiting { progress: u64, budget: Duration },
    Terminal(PruneCompletion),
}

#[derive(Debug, PartialEq, Eq)]
enum FinalReconcileDecision {
    Complete(PruneCompletion),
    Wait,
    Fallback,
}

fn final_reconcile_decision(
    disposition: Option<StatusDisposition>,
    now: Instant,
    deadline: Instant,
) -> FinalReconcileDecision {
    if let Some(StatusDisposition::Terminal(completion)) = disposition {
        return FinalReconcileDecision::Complete(completion);
    }
    if now >= deadline {
        FinalReconcileDecision::Fallback
    } else {
        FinalReconcileDecision::Wait
    }
}

fn classify_status(machine_id: &MachineId, status: BuildCachePruneStatus) -> StatusDisposition {
    match status {
        BuildCachePruneStatus::NotFound { .. } => StatusDisposition::NotFound,
        BuildCachePruneStatus::Queued { progress, .. } => StatusDisposition::Waiting {
            progress,
            budget: BUILD_CACHE_PRUNE_QUEUED_STALL_TIMEOUT,
        },
        BuildCachePruneStatus::Running { progress, .. } => StatusDisposition::Waiting {
            progress,
            budget: BUILD_CACHE_PRUNE_RUNNING_STALL_TIMEOUT,
        },
        BuildCachePruneStatus::Completed { evidence, .. } => {
            StatusDisposition::Terminal(PruneCompletion::Completed(evidence))
        }
        BuildCachePruneStatus::Failed { message, .. } => StatusDisposition::Terminal(
            PruneCompletion::Failed(MachineBuildCachePruneFailure::PruneRejected {
                machine_id: machine_id.clone(),
                message,
            }),
        ),
        BuildCachePruneStatus::Cancelled { .. } => {
            StatusDisposition::Terminal(PruneCompletion::Cancelled(machine_cancel_reason()))
        }
    }
}

struct PruneProgressDeadline {
    watermark: Option<u64>,
    deadline: Instant,
}

impl PruneProgressDeadline {
    fn new(now: Instant) -> Self {
        Self {
            watermark: None,
            deadline: now + BUILD_CACHE_PRUNE_QUEUED_STALL_TIMEOUT,
        }
    }

    fn observe(&mut self, progress: u64, budget: Duration, now: Instant) -> bool {
        if self
            .watermark
            .is_some_and(|watermark| progress <= watermark)
        {
            return false;
        }
        self.watermark = Some(progress);
        self.deadline = now + budget;
        true
    }

    fn is_stalled(&self, now: Instant) -> bool {
        now >= self.deadline
    }
}

fn machine_cancel_reason() -> CancellationReason {
    CancellationReason::try_new("machine cancelled build cache prune")
        .expect("machine cancellation reason is non-empty")
}

fn start_failure(error: MachineBuildCachePruneStartError) -> MachineBuildCachePruneFailure {
    match error {
        MachineBuildCachePruneStartError::Unavailable { machine_id, reason } => {
            MachineBuildCachePruneFailure::MachineUnavailable {
                machine_id,
                message: reason.failure_message(),
            }
        }
        MachineBuildCachePruneStartError::Rejected { machine_id, error } => {
            use crate::roles::machine::protocol::MachineBuildCachePruneStartDomainError;
            let message = match error {
                MachineBuildCachePruneStartDomainError::RuntimeUnavailable => {
                    failure_message("machine build runtime is unavailable")
                }
                MachineBuildCachePruneStartDomainError::RuntimeStopped => {
                    failure_message("machine build runtime is stopped")
                }
                MachineBuildCachePruneStartDomainError::AlreadyRunning { operation_id } => {
                    failure_message(format!(
                        "build cache prune {} is already running",
                        operation_id.as_str()
                    ))
                }
                MachineBuildCachePruneStartDomainError::StartFailed { message } => message,
            };
            MachineBuildCachePruneFailure::PruneRejected {
                machine_id,
                message,
            }
        }
        MachineBuildCachePruneStartError::UnexpectedOperation {
            machine_id,
            expected,
            actual,
        } => MachineBuildCachePruneFailure::MachineUnavailable {
            machine_id,
            message: failure_message(format!(
                "machine acknowledged build cache prune {} for request {}",
                actual.as_str(),
                expected.as_str()
            )),
        },
    }
}

fn status_failure(error: MachineBuildCachePruneStatusError) -> MachineBuildCachePruneFailure {
    match error {
        MachineBuildCachePruneStatusError::Unavailable { machine_id, reason } => {
            MachineBuildCachePruneFailure::MachineUnavailable {
                machine_id,
                message: reason.failure_message(),
            }
        }
        MachineBuildCachePruneStatusError::Rejected { machine_id, error } => {
            let crate::roles::machine::protocol::MachineBuildCachePruneStatusDomainError::ReadFailed {
                message,
            } = error;
            MachineBuildCachePruneFailure::PruneRejected {
                machine_id,
                message,
            }
        }
        MachineBuildCachePruneStatusError::UnexpectedOperation {
            machine_id,
            expected,
            actual,
        } => MachineBuildCachePruneFailure::MachineUnavailable {
            machine_id,
            message: failure_message(format!(
                "machine reported build cache prune {} for request {}",
                actual.as_str(),
                expected.as_str()
            )),
        },
    }
}

fn status_error_is_silence(error: &MachineBuildCachePruneStatusError) -> bool {
    matches!(error, MachineBuildCachePruneStatusError::Unavailable { .. })
}

fn not_found_failure(machine_id: &MachineId) -> MachineBuildCachePruneFailure {
    MachineBuildCachePruneFailure::PruneRejected {
        machine_id: machine_id.clone(),
        message: failure_message("machine did not retain the accepted build cache prune"),
    }
}

fn stalled_failure(machine_id: &MachineId) -> MachineBuildCachePruneFailure {
    MachineBuildCachePruneFailure::PruneRejected {
        machine_id: machine_id.clone(),
        message: failure_message("machine build cache prune stopped making progress"),
    }
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("machine prune failure message is non-empty")
}

fn commit_failure(
    machine_id: &MachineId,
    error: RecordOperationEventError,
) -> MachineBuildCachePruneFailure {
    let message =
        FailureMessage::try_new(format!("failed to record build cache prune event: {error}"))
            .expect("event failure is non-empty");
    MachineBuildCachePruneFailure::StateCommitFailed {
        machine_id: machine_id.clone(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::build::BuildCachePruneRunningPhase;

    fn machine_id() -> MachineId {
        MachineId::try_new("machine-a").expect("machine")
    }

    fn operation_id() -> OperationId {
        OperationId::try_new("prune-1").expect("operation")
    }

    #[test]
    fn only_strict_progress_renews_the_phase_deadline() {
        let now = Instant::now();
        let mut deadline = PruneProgressDeadline::new(now);
        assert!(deadline.observe(1, Duration::from_secs(10), now));
        let first_deadline = deadline.deadline;
        assert!(!deadline.observe(1, Duration::from_secs(20), now + Duration::from_secs(1)));
        assert_eq!(deadline.deadline, first_deadline);
        assert!(!deadline.observe(0, Duration::from_secs(20), now + Duration::from_secs(2)));
        assert_eq!(deadline.deadline, first_deadline);
        assert!(deadline.observe(2, Duration::from_secs(20), now + Duration::from_secs(3)));
        assert_eq!(deadline.deadline, now + Duration::from_secs(23));
    }

    #[test]
    fn advancing_progress_has_no_total_elapsed_deadline() {
        let started = Instant::now();
        let mut deadline = PruneProgressDeadline::new(started);
        let first = started + BUILD_CACHE_PRUNE_QUEUED_STALL_TIMEOUT - Duration::from_secs(1);
        assert!(deadline.observe(1, BUILD_CACHE_PRUNE_RUNNING_STALL_TIMEOUT, first));
        let second = first + BUILD_CACHE_PRUNE_RUNNING_STALL_TIMEOUT - Duration::from_secs(1);
        assert!(deadline.observe(2, BUILD_CACHE_PRUNE_RUNNING_STALL_TIMEOUT, second));
        assert!(second > started + BUILD_CACHE_PRUNE_QUEUED_STALL_TIMEOUT);
        assert!(!deadline.is_stalled(second));
    }

    #[test]
    fn queued_and_every_running_phase_select_their_bounded_silence_budget() {
        let machine = machine_id();
        let operation = operation_id();
        assert_eq!(
            classify_status(
                &machine,
                BuildCachePruneStatus::Queued {
                    operation_id: operation.clone(),
                    progress: 0,
                }
            ),
            StatusDisposition::Waiting {
                progress: 0,
                budget: BUILD_CACHE_PRUNE_QUEUED_STALL_TIMEOUT,
            }
        );
        for phase in [
            BuildCachePruneRunningPhase::MeasuringBefore,
            BuildCachePruneRunningPhase::InspectingCache,
            BuildCachePruneRunningPhase::RemovingCache,
            BuildCachePruneRunningPhase::MeasuringAfter,
        ] {
            assert_eq!(
                classify_status(
                    &machine,
                    BuildCachePruneStatus::Running {
                        operation_id: operation.clone(),
                        phase,
                        progress: 4,
                    }
                ),
                StatusDisposition::Waiting {
                    progress: 4,
                    budget: BUILD_CACHE_PRUNE_RUNNING_STALL_TIMEOUT,
                }
            );
        }
    }

    #[test]
    fn terminal_machine_statuses_map_to_existing_operation_transitions() {
        let machine = machine_id();
        let operation = operation_id();
        let evidence = BuildCachePruneEvidence {
            before_available_bytes: 10,
            reclaimed_bytes: 4,
            after_available_bytes: 14,
        };
        assert_eq!(
            classify_status(
                &machine,
                BuildCachePruneStatus::Completed {
                    operation_id: operation.clone(),
                    evidence: evidence.clone(),
                }
            ),
            StatusDisposition::Terminal(PruneCompletion::Completed(evidence))
        );
        assert!(matches!(
            classify_status(
                &machine,
                BuildCachePruneStatus::Failed {
                    operation_id: operation.clone(),
                    message: failure_message("prune failed"),
                }
            ),
            StatusDisposition::Terminal(PruneCompletion::Failed(
                MachineBuildCachePruneFailure::PruneRejected { .. }
            ))
        ));
        assert_eq!(
            classify_status(
                &machine,
                BuildCachePruneStatus::Cancelled {
                    operation_id: operation.clone(),
                }
            ),
            StatusDisposition::Terminal(PruneCompletion::Cancelled(machine_cancel_reason()))
        );
        assert_eq!(
            classify_status(
                &machine,
                BuildCachePruneStatus::NotFound {
                    operation_id: operation,
                }
            ),
            StatusDisposition::NotFound
        );
    }

    #[test]
    fn transient_status_unavailability_is_silence_but_typed_rejection_is_not() {
        let machine_id = machine_id();
        assert!(status_error_is_silence(
            &MachineBuildCachePruneStatusError::Unavailable {
                machine_id: machine_id.clone(),
                reason: crate::roles::machine::MachineRuntimeUnavailableReason::RequestTimedOut,
            }
        ));
        assert!(!status_error_is_silence(
            &MachineBuildCachePruneStatusError::Rejected {
                machine_id,
                error: crate::roles::machine::protocol::MachineBuildCachePruneStatusDomainError::ReadFailed {
                    message: failure_message("read failed"),
                },
            }
        ));
    }

    #[test]
    fn final_reconcile_waits_for_cooperative_cancellation_until_its_short_deadline() {
        let now = Instant::now();
        let deadline = now + BUILD_CACHE_PRUNE_FINAL_RECONCILE_TIMEOUT;
        assert_eq!(
            final_reconcile_decision(
                Some(StatusDisposition::Waiting {
                    progress: 4,
                    budget: BUILD_CACHE_PRUNE_RUNNING_STALL_TIMEOUT,
                }),
                now,
                deadline,
            ),
            FinalReconcileDecision::Wait
        );
        assert_eq!(
            final_reconcile_decision(None, deadline, deadline),
            FinalReconcileDecision::Fallback
        );
        assert_eq!(
            final_reconcile_decision(
                Some(StatusDisposition::Terminal(PruneCompletion::Cancelled(
                    machine_cancel_reason(),
                ))),
                deadline,
                deadline,
            ),
            FinalReconcileDecision::Complete(PruneCompletion::Cancelled(machine_cancel_reason()))
        );
    }
}
