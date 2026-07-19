use std::future::Future;
use std::time::Duration;

use ployz_core::build::{
    BUILD_MAX_PLACEMENT_TIMEOUT, BuildExecutorAssignment, BuildExecutorCancelOk,
    BuildExecutorCancelOutcome, BuildExecutorCancelRequest, BuildExecutorCancelResponse,
    BuildExecutorCleanupOutcome, BuildExecutorEvidence, BuildPlatformExecutorAssignment,
    BuildTarget,
};
use ployz_core::deploy::PushedImageReceipt;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::{
    BuildCleanupEvidence, BuildEvidence, BuildOperationFailure, BuildPlatformFailure,
    BuildTimeoutFailure, BuildTransition, CancellationReason, EventSequence, FailureMessage,
    OperationStatus,
};
use ployz_nats::service_runtime::request_json;
use ployz_nats::subjects::{
    BuildExecutorServiceEndpoint, MachineServiceEndpoint, build_executor_service,
};

use crate::control::intent::service::NatsIntentReader;
use crate::control::role_client::machine::{
    MachineCallError, NatsMachineFactsReader, call_machine, read_machine_placement_facts,
};
use crate::control::role_client::machine_convergence::gather_dataplane_statuses;
use crate::control::sequencer::{AcceptedBuildExecution, OperationControllers};
use crate::roles::machine::MachineRuntimeUnavailableReason;
use crate::roles::machine::protocol::{
    MachineBuildCancelDomainError, MachineBuildCancelOutcome, MachineBuildCancelRpcOk,
    MachineBuildCancelRpcRequest, MachineBuildStartDomainError,
};
use crate::tasks::TaskSpawner;

use super::active_registry::{ActiveBuildRegistry, CancellationRequest, RejectedAdmissionOutcome};
#[cfg(test)]
use super::log_stream::{MachineCallOrLog, next_machine_call_or_log};
use super::placement::{ClusterBuildExecutorAssignment, place_build_platforms};
#[cfg(test)]
use crate::roles::machine::protocol::{
    BuildExecutorAcceptance, BuildLogSummary, MachineBuildStartRpcOk,
};
#[cfg(test)]
use ployz_core::build::build_control_request_timeout;

const BUILD_CANCEL_TIMEOUT: Duration = Duration::from_secs(5);
const BUILD_CANCEL_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(1);

async fn within_placement_deadline<T>(
    placement: impl Future<Output = Result<T, BuildOperationFailure>>,
) -> Result<T, BuildOperationFailure> {
    tokio::time::timeout(BUILD_MAX_PLACEMENT_TIMEOUT, placement)
        .await
        .map_err(|_| BuildOperationFailure::ControlUnavailable {
            message: FailureMessage::try_new("build placement exceeded its 180-second deadline")
                .expect("placement timeout failure is non-empty"),
        })?
}

#[derive(Clone)]
pub(crate) struct BuildOperationDriver {
    pub(super) client: async_nats::Client,
    facts: NatsMachineFactsReader,
    intent: NatsIntentReader,
    pub(super) controllers: OperationControllers,
    pub(super) timeout: Duration,
    tasks: TaskSpawner,
    pub(super) active: ActiveBuildRegistry,
}

pub(crate) enum BuildCancelDisposition {
    Accepted { watch_start_sequence: EventSequence },
    NoSuchOperation,
    AlreadyTerminal,
}

impl BuildOperationDriver {
    pub(crate) fn new(
        client: async_nats::Client,
        facts: NatsMachineFactsReader,
        intent: NatsIntentReader,
        controllers: OperationControllers,
        timeout: Duration,
        tasks: TaskSpawner,
    ) -> Self {
        Self {
            client,
            facts,
            intent,
            controllers,
            timeout,
            tasks,
            active: ActiveBuildRegistry::default(),
        }
    }

    pub(crate) async fn start(&self, accepted: AcceptedBuildExecution) {
        if !accepted.submission.should_start_execution {
            return;
        }
        self.active
            .start(
                accepted.submission.operation_id.clone(),
                accepted.submission.start_sequence,
                accepted.submission.target.clone(),
            )
            .await;
        let driver = self.clone();
        let operation_id = accepted.submission.operation_id.clone();
        let admission = self.tasks.spawn(move || async move {
            driver.run(accepted).await;
        });
        let Err(error) = admission else {
            return;
        };
        match self.active.claim_rejected_admission(&operation_id).await {
            RejectedAdmissionOutcome::Cancelled(reason) => {
                if let Err(record_error) = self
                    .controllers
                    .repository()
                    .record_build_transition(
                        &operation_id,
                        BuildTransition::Cancelled {
                            reason,
                            cleanup: BuildCleanupEvidence::NotRequired,
                        },
                    )
                    .await
                {
                    eprintln!(
                        "build {} cancellation after rejected task admission could not be recorded: {record_error}",
                        operation_id.as_str()
                    );
                }
            }
            RejectedAdmissionOutcome::Interrupted => {
                super::super::finish_rejected_task_admission(
                    &self.controllers,
                    &operation_id,
                    Err(error),
                )
                .await;
            }
        }
        self.active.remove(&operation_id).await;
    }

    pub(crate) async fn preflight_external(
        &self,
        pool_id: &ployz_core::build::BuildPoolId,
        platforms: &ployz_core::build::BuildPlatforms,
        adapter: &ployz_core::build::BuildAdapter,
    ) -> Result<
        Vec<ployz_core::build::BuildPlatformExecutorAssignment>,
        super::ExternalBuildAdmissionError,
    > {
        super::external_admission::preflight_external_build(
            &self.client,
            &self.intent,
            pool_id,
            platforms,
            adapter,
        )
        .await
    }

    pub(crate) async fn cancel(
        &self,
        operation_id: &OperationId,
        reason: CancellationReason,
    ) -> Result<BuildCancelDisposition, String> {
        let (executors, watch_start_sequence) =
            match self.active.request_cancellation(operation_id, reason).await {
                CancellationRequest::Accepted {
                    executors,
                    watch_start_sequence,
                } => (executors, watch_start_sequence),
                CancellationRequest::Finalizing => {
                    return Ok(BuildCancelDisposition::AlreadyTerminal);
                }
                CancellationRequest::Missing => {
                    let status = self
                        .controllers
                        .repository()
                        .get(operation_id)
                        .await
                        .map_err(|error| error.to_string())?;
                    return Ok(match status {
                        Some(OperationStatus::Build { status }) if status.state().is_terminal() => {
                            BuildCancelDisposition::AlreadyTerminal
                        }
                        Some(OperationStatus::Build { .. }) => {
                            return Err(format!(
                                "build {} is nonterminal but has no active execution",
                                operation_id.as_str()
                            ));
                        }
                        None
                        | Some(OperationStatus::Deploy { .. })
                        | Some(OperationStatus::Cert { .. })
                        | Some(OperationStatus::MachineAdd { .. })
                        | Some(OperationStatus::MachineUpdate { .. })
                        | Some(OperationStatus::MachineBuildCachePrune { .. })
                        | Some(OperationStatus::MachineStoragePrepare { .. })
                        | Some(OperationStatus::MachineLifecycle { .. })
                        | Some(OperationStatus::CoreReplace { .. })
                        | Some(OperationStatus::CredentialGrant { .. })
                        | Some(OperationStatus::NetworkRepair { .. })
                        | Some(OperationStatus::ServiceRestart { .. })
                        | Some(OperationStatus::ManagedDnsReconcile { .. })
                        | Some(OperationStatus::IngressConfigure { .. })
                        | Some(OperationStatus::NamespaceRemove { .. })
                        | Some(OperationStatus::VolumeCreate { .. })
                        | Some(OperationStatus::VolumeRemove { .. }) => {
                            BuildCancelDisposition::NoSuchOperation
                        }
                    });
                }
            };
        futures_util::future::join_all(executors.iter().map(|executor| async move {
            let deadline = tokio::time::Instant::now() + BUILD_CANCEL_TIMEOUT;
            deliver_build_cancel(self, operation_id, executor, deadline).await;
        }))
        .await;
        Ok(BuildCancelDisposition::Accepted {
            watch_start_sequence,
        })
    }

    async fn run(self, accepted: AcceptedBuildExecution) {
        let id = accepted.submission.operation_id.clone();
        let result = self.run_inner(&accepted).await;
        self.record_run_result(&id, result).await;
        self.active.remove(&id).await;
    }

    async fn record_run_result(&self, id: &OperationId, result: Result<(), BuildOperationFailure>) {
        if let Err(failure) = result {
            let transition = match self.active.claim_finalization(id).await {
                Some(reason) => BuildTransition::Cancelled {
                    reason,
                    cleanup: self.active.cleanup_evidence(id, Vec::new()).await,
                },
                None => BuildTransition::Failed { failure },
            };
            let _ = self
                .controllers
                .repository()
                .record_build_transition(id, transition)
                .await;
        }
    }

    async fn run_inner(
        &self,
        accepted: &AcceptedBuildExecution,
    ) -> Result<(), BuildOperationFailure> {
        let placement = within_placement_deadline(self.place(accepted)).await?;
        let id = &accepted.submission.operation_id;
        let outcomes = futures_util::future::join_all(
            placement
                .into_iter()
                .map(|assignment| self.run_platform(accepted, assignment)),
        )
        .await;
        self.finalize_joined_outcomes(id, outcomes).await
    }

    async fn finalize_joined_outcomes(
        &self,
        id: &OperationId,
        outcomes: Vec<Result<PlatformOutcome, BuildOperationFailure>>,
    ) -> Result<(), BuildOperationFailure> {
        let mut images = Vec::new();
        let mut cancelled = Vec::new();
        let mut timed_out = Vec::new();
        let mut first_failure = None;
        for outcome in outcomes {
            match outcome {
                Err(failure) => return Err(failure),
                Ok(PlatformOutcome::Completed { platform, image }) => {
                    images.push((platform, image))
                }
                Ok(PlatformOutcome::Failed(failure)) => {
                    first_failure.get_or_insert(failure);
                }
                Ok(PlatformOutcome::Cancelled { executor, cleanup }) => {
                    cancelled.push((executor, cleanup))
                }
                Ok(PlatformOutcome::CancelledBeforeStart) => {}
                Ok(PlatformOutcome::TimedOut {
                    executor,
                    message,
                    cleanup,
                }) => timed_out.push((executor, message, cleanup)),
            }
        }
        let cancellation_reason = self.active.claim_finalization(id).await;
        if !cancelled.is_empty() || cancellation_reason.is_some() {
            let reason = cancellation_reason.unwrap_or_else(|| {
                CancellationReason::try_new("machine cancelled build").expect("reason")
            });
            let cleanup = self
                .active
                .cleanup_evidence(
                    id,
                    cancelled
                        .into_iter()
                        .filter_map(|(executor, cleanup)| {
                            (cleanup == BuildExecutorCleanupOutcome::Confirmed).then_some(executor)
                        })
                        .collect(),
                )
                .await;
            self.controllers
                .repository()
                .record_build_transition(id, BuildTransition::Cancelled { reason, cleanup })
                .await
                .map_err(record_failure)?;
            return Ok(());
        }
        if let Some(message) = timed_out.first().map(|(_, message, _)| message.clone()) {
            let cleanup = timeout_cleanup(&timed_out);
            self.controllers
                .repository()
                .record_build_transition(
                    id,
                    BuildTransition::TimedOut {
                        failure: BuildTimeoutFailure::DeadlineExceeded { message },
                        cleanup,
                    },
                )
                .await
                .map_err(record_failure)?;
            return Ok(());
        }
        if let Some(failure) = first_failure {
            return Err(failure);
        }
        let receipt = PushedImageReceipt::try_new(images)
            .map_err(|error| receipt_failure(error.to_string()))?;
        self.controllers
            .repository()
            .record_build_transition(id, BuildTransition::Completed { receipt })
            .await
            .map_err(record_failure)?;
        Ok(())
    }

    async fn place(
        &self,
        accepted: &AcceptedBuildExecution,
    ) -> Result<Vec<BuildPlatformExecutorAssignment>, BuildOperationFailure> {
        let id = &accepted.submission.operation_id;
        self.controllers
            .repository()
            .record_build_transition(id, BuildTransition::Placing)
            .await
            .map_err(record_failure)?;
        let placement = match &accepted.submission.target {
            BuildTarget::Cluster => {
                let intent = self.intent.intent().await.map_err(|error| {
                    BuildOperationFailure::ControlUnavailable {
                        message: failure_message(error),
                    }
                })?;
                let projection = intent.dataplane_projection;
                let facts = read_machine_placement_facts(
                    &self.facts,
                    intent
                        .active_machines
                        .into_iter()
                        .map(|machine| (machine.machine_id, machine.lifecycle)),
                )
                .await;
                let dataplane_statuses = gather_dataplane_statuses(
                    &self.facts,
                    projection
                        .declared_members()
                        .iter()
                        .map(|member| &member.machine_id),
                )
                .await;
                place_build_platforms(
                    &accepted.submission.platforms,
                    &accepted.submission.adapter,
                    &facts,
                    &projection,
                    &dataplane_statuses,
                )
                .map_err(|failure| *failure)?
                .into_iter()
                .map(|assignment| BuildPlatformExecutorAssignment {
                    platform: assignment.platform,
                    executor: BuildExecutorAssignment::Cluster {
                        machine_id: assignment.machine_id,
                    },
                })
                .collect()
            }
            BuildTarget::External { .. } if accepted.planned_assignments.is_empty() => {
                return Err(BuildOperationFailure::ControlUnavailable {
                    message: failure_message("external build lost its admitted placement"),
                });
            }
            BuildTarget::External { .. } => accepted.planned_assignments.clone(),
        };
        for assignment in &placement {
            self.controllers
                .repository()
                .record_build_evidence(
                    id,
                    BuildEvidence::PlatformPlaced {
                        platform: assignment.platform.clone(),
                        executor: BuildExecutorEvidence::from_assignment(&assignment.executor),
                    },
                )
                .await
                .map_err(record_failure)?;
        }
        self.controllers
            .repository()
            .record_build_transition(id, BuildTransition::Building)
            .await
            .map_err(record_failure)?;
        Ok(placement)
    }

    async fn run_platform(
        &self,
        accepted: &AcceptedBuildExecution,
        assignment: BuildPlatformExecutorAssignment,
    ) -> Result<PlatformOutcome, BuildOperationFailure> {
        match assignment.executor {
            BuildExecutorAssignment::Cluster { machine_id } => {
                super::executor_session::run_executor_session(
                    self,
                    accepted,
                    ClusterBuildExecutorAssignment {
                        platform: assignment.platform,
                        machine_id,
                    },
                )
                .await
            }
            executor @ BuildExecutorAssignment::External { .. } => {
                super::external_executor_session::run_external_executor_session(
                    self,
                    accepted,
                    BuildPlatformExecutorAssignment {
                        platform: assignment.platform,
                        executor,
                    },
                )
                .await
            }
        }
    }
}

async fn deliver_build_cancel(
    driver: &BuildOperationDriver,
    operation_id: &OperationId,
    assignment: &BuildExecutorAssignment,
    deadline: tokio::time::Instant,
) {
    match assignment {
        BuildExecutorAssignment::Cluster { machine_id } => {
            deliver_build_cancel_to_machine(machine_id, deadline, |request_timeout| {
                let request = MachineBuildCancelRpcRequest {
                    operation_id: operation_id.clone(),
                    assignment: assignment.clone(),
                };
                async move {
                    call_machine::<MachineBuildCancelRpcOk, MachineBuildCancelDomainError>(
                        &driver.client,
                        request_timeout,
                        machine_id,
                        MachineServiceEndpoint::BuildCancel,
                        &request,
                    )
                    .await
                }
            })
            .await;
        }
        BuildExecutorAssignment::External {
            pool_id,
            executor_id,
            image_seed: _,
        } => {
            while let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now())
            {
                let subject = build_executor_service(
                    pool_id,
                    executor_id,
                    BuildExecutorServiceEndpoint::BuildCancel,
                );
                let request = BuildExecutorCancelRequest {
                    operation_id: operation_id.clone(),
                    assignment: assignment.clone(),
                };
                let result = request_json::<_, BuildExecutorCancelResponse>(
                    &driver.client,
                    subject,
                    &request,
                    remaining.min(BUILD_CANCEL_ATTEMPT_TIMEOUT),
                )
                .await;
                let retry = match result {
                    Ok(BuildExecutorCancelResponse::Ok(BuildExecutorCancelOk {
                        assignment: actual,
                        outcome: BuildExecutorCancelOutcome::NotRunning,
                    })) if actual == *assignment => true,
                    Ok(BuildExecutorCancelResponse::Ok(BuildExecutorCancelOk {
                        assignment: actual,
                        outcome: BuildExecutorCancelOutcome::Requested,
                    })) if actual == *assignment => false,
                    Ok(BuildExecutorCancelResponse::Ok(_))
                    | Ok(BuildExecutorCancelResponse::DomainError { .. }) => false,
                    Err(_) => true,
                };
                if !retry {
                    break;
                }
                let Some(delay) = cancel_retry_delay(tokio::time::Instant::now(), deadline) else {
                    break;
                };
                tokio::time::sleep(delay).await;
            }
        }
    }
}

async fn deliver_build_cancel_to_machine<Call, CallFuture>(
    machine_id: &MachineId,
    deadline: tokio::time::Instant,
    mut call: Call,
) where
    Call: FnMut(Duration) -> CallFuture,
    CallFuture: Future<
        Output = Result<MachineBuildCancelRpcOk, MachineCallError<MachineBuildCancelDomainError>>,
    >,
{
    while let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) {
        let result = call(remaining.min(BUILD_CANCEL_ATTEMPT_TIMEOUT)).await;
        if !cancel_delivery_should_retry(machine_id, &result) {
            break;
        }
        let Some(delay) = cancel_retry_delay(tokio::time::Instant::now(), deadline) else {
            break;
        };
        tokio::time::sleep(delay).await;
    }
}

fn cancel_delivery_should_retry(
    machine_id: &MachineId,
    result: &Result<MachineBuildCancelRpcOk, MachineCallError<MachineBuildCancelDomainError>>,
) -> bool {
    let expected_assignment = BuildExecutorAssignment::Cluster {
        machine_id: machine_id.clone(),
    };
    match result {
        Ok(ok)
            if ok.executor.assignment == expected_assignment
                && ok.executor.outcome == MachineBuildCancelOutcome::NotRunning =>
        {
            true
        }
        Ok(ok)
            if ok.executor.assignment == expected_assignment
                && ok.executor.outcome == MachineBuildCancelOutcome::Requested =>
        {
            false
        }
        Ok(_)
        | Err(MachineCallError::Domain(MachineBuildCancelDomainError::CancelFailed {
            message: _,
        }))
        | Err(MachineCallError::Domain(MachineBuildCancelDomainError::AssignmentMismatch {
            expected: _,
            actual: _,
        })) => false,
        Err(MachineCallError::Unavailable(reason)) => match reason {
            MachineRuntimeUnavailableReason::RequestTimedOut
            | MachineRuntimeUnavailableReason::NoResponders
            | MachineRuntimeUnavailableReason::RequestFailed { message: _ }
            | MachineRuntimeUnavailableReason::ServiceUnavailable { message: _ }
            | MachineRuntimeUnavailableReason::ServiceTimedOut { message: _ }
            | MachineRuntimeUnavailableReason::ServiceInternal { message: _ } => true,
            MachineRuntimeUnavailableReason::EncodeRequest { message: _ }
            | MachineRuntimeUnavailableReason::InvalidSubject
            | MachineRuntimeUnavailableReason::MaxPayloadExceeded
            | MachineRuntimeUnavailableReason::ServiceBadRequest { message: _ }
            | MachineRuntimeUnavailableReason::ServiceConflict { message: _ }
            | MachineRuntimeUnavailableReason::ServiceResponseTooLarge
            | MachineRuntimeUnavailableReason::MalformedServiceError { message: _ }
            | MachineRuntimeUnavailableReason::DecodeResponse { message: _ }
            | MachineRuntimeUnavailableReason::WrongResponder {
                actual_machine_id: _,
            } => false,
        },
    }
}

fn cancel_retry_delay(
    now: tokio::time::Instant,
    deadline: tokio::time::Instant,
) -> Option<Duration> {
    let remaining = deadline.checked_duration_since(now)?;
    (!remaining.is_zero()).then(|| remaining.min(Duration::from_millis(25)))
}

#[cfg(test)]
fn validate_executor_acceptance(
    expected: &BuildExecutorAcceptance,
    actual: &BuildExecutorAcceptance,
) -> Result<(), BuildPlatformFailure> {
    if actual != expected {
        return Err(BuildPlatformFailure::MachineUnavailable {
            message: failure_message("build executor returned mismatched acceptance provenance"),
        });
    }
    Ok(())
}

#[cfg(test)]
fn execution_timeout_millis(timeout: Duration) -> u64 {
    timeout.as_millis().try_into().unwrap_or(u64::MAX)
}

pub(super) enum PlatformOutcome {
    Completed {
        platform: ployz_core::image::OciPlatform,
        image: ployz_core::deploy::PlatformImage,
    },
    Failed(BuildOperationFailure),
    Cancelled {
        executor: BuildExecutorAssignment,
        cleanup: BuildExecutorCleanupOutcome,
    },
    CancelledBeforeStart,
    TimedOut {
        executor: BuildExecutorAssignment,
        message: FailureMessage,
        cleanup: BuildExecutorCleanupOutcome,
    },
}

fn timeout_cleanup(
    outcomes: &[(
        BuildExecutorAssignment,
        FailureMessage,
        BuildExecutorCleanupOutcome,
    )],
) -> BuildCleanupEvidence {
    let unconfirmed = outcomes
        .iter()
        .filter(|(_, _, cleanup)| *cleanup == BuildExecutorCleanupOutcome::Unconfirmed)
        .map(|(executor, _, _)| executor.clone())
        .collect::<Vec<_>>();
    if unconfirmed.is_empty() {
        cleanup_for_assignments(outcomes.iter().map(|(executor, _, _)| executor), true)
    } else {
        cleanup_for_assignments(unconfirmed.iter(), false)
    }
}

fn cleanup_for_assignments<'a>(
    assignments: impl Iterator<Item = &'a BuildExecutorAssignment>,
    confirmed: bool,
) -> BuildCleanupEvidence {
    let assignments = assignments.cloned().collect::<Vec<_>>();
    match assignments.first() {
        Some(BuildExecutorAssignment::Cluster { .. }) if confirmed => {
            BuildCleanupEvidence::Completed {
                machine_ids: assignments
                    .into_iter()
                    .filter_map(|assignment| match assignment {
                        BuildExecutorAssignment::Cluster { machine_id } => Some(machine_id),
                        BuildExecutorAssignment::External { .. } => None,
                    })
                    .collect(),
            }
        }
        Some(BuildExecutorAssignment::Cluster { .. }) => BuildCleanupEvidence::Unconfirmed {
            machine_ids: assignments
                .into_iter()
                .filter_map(|assignment| match assignment {
                    BuildExecutorAssignment::Cluster { machine_id } => Some(machine_id),
                    BuildExecutorAssignment::External { .. } => None,
                })
                .collect(),
        },
        Some(BuildExecutorAssignment::External { .. }) if confirmed => {
            BuildCleanupEvidence::ExternalCompleted {
                executors: assignments
                    .iter()
                    .map(BuildExecutorEvidence::from_assignment)
                    .collect(),
            }
        }
        Some(BuildExecutorAssignment::External { .. }) => {
            BuildCleanupEvidence::ExternalUnconfirmed {
                executors: assignments
                    .iter()
                    .map(BuildExecutorEvidence::from_assignment)
                    .collect(),
            }
        }
        None => BuildCleanupEvidence::NotRequired,
    }
}

pub(super) fn platform_failure(
    platform: ployz_core::image::OciPlatform,
    machine_id: MachineId,
    failure: BuildPlatformFailure,
) -> BuildOperationFailure {
    BuildOperationFailure::PlatformFailed {
        platform,
        machine_id,
        failure,
    }
}

pub(super) fn failure_message(error: impl std::fmt::Display) -> FailureMessage {
    FailureMessage::try_new(error.to_string()).expect("error is non-empty")
}

pub(super) fn record_failure(error: impl std::fmt::Display) -> BuildOperationFailure {
    BuildOperationFailure::EvidenceRecordingFailed {
        message: failure_message(error),
    }
}
fn receipt_failure(message: String) -> BuildOperationFailure {
    BuildOperationFailure::ReceiptAssemblyFailed {
        message: FailureMessage::try_new(message).expect("error is non-empty"),
    }
}
pub(super) fn machine_failure(
    platform: ployz_core::image::OciPlatform,
    machine_id: MachineId,
    error: MachineCallError<MachineBuildStartDomainError>,
) -> BuildOperationFailure {
    let failure = match error {
        MachineCallError::Unavailable(reason) => machine_unavailable_failure(reason),
        MachineCallError::Domain(MachineBuildStartDomainError::AlreadyRunning) => {
            BuildPlatformFailure::MachineUnavailable {
                message: failure_message("machine reports this build is already running"),
            }
        }
        MachineCallError::Domain(MachineBuildStartDomainError::RuntimeUnavailable) => {
            BuildPlatformFailure::MachineUnavailable {
                message: failure_message("machine build runtime is unavailable"),
            }
        }
        MachineCallError::Domain(MachineBuildStartDomainError::RuntimeStopped) => {
            BuildPlatformFailure::MachineUnavailable {
                message: failure_message("machine build runtime is shutting down"),
            }
        }
        MachineCallError::Domain(MachineBuildStartDomainError::PlatformMismatch {
            expected,
            actual,
        }) => BuildPlatformFailure::PlatformMismatch { expected, actual },
        MachineCallError::Domain(MachineBuildStartDomainError::InvalidTimeout {
            timeout_millis: _,
        }) => BuildPlatformFailure::MachineUnavailable {
            message: failure_message("machine rejected the build execution timeout"),
        },
        MachineCallError::Domain(MachineBuildStartDomainError::AssignmentMismatch {
            expected: _,
            actual: _,
        }) => BuildPlatformFailure::MachineUnavailable {
            message: failure_message("machine rejected build executor provenance"),
        },
        MachineCallError::Domain(MachineBuildStartDomainError::ExecutorIdentityMismatch {
            expected: _,
            actual: _,
        }) => BuildPlatformFailure::MachineUnavailable {
            message: failure_message("machine rejected external executor identity provenance"),
        },
        MachineCallError::Domain(MachineBuildStartDomainError::ToolchainUnavailable {
            adapter: _,
        }) => BuildPlatformFailure::MachineUnavailable {
            message: failure_message("machine rejected unavailable build toolchain"),
        },
        MachineCallError::Domain(MachineBuildStartDomainError::ImageSeedUnavailable {
            image_seed,
        }) => BuildPlatformFailure::ImageSeedUnavailable { image_seed },
        MachineCallError::Domain(MachineBuildStartDomainError::PlatformFailed {
            acceptance: _,
            failure,
            log_summary: _,
        }) => failure,
        MachineCallError::Domain(MachineBuildStartDomainError::Cancelled {
            acceptance: _,
            cleanup: _,
            log_summary: _,
        }) => BuildPlatformFailure::MachineUnavailable {
            message: failure_message("machine cancelled the build unexpectedly"),
        },
        MachineCallError::Domain(MachineBuildStartDomainError::TimedOut {
            acceptance: _,
            message,
            cleanup: _,
            log_summary: _,
        }) => BuildPlatformFailure::MachineUnavailable { message },
    };
    BuildOperationFailure::PlatformFailed {
        platform,
        machine_id,
        failure,
    }
}

fn machine_unavailable_failure(reason: MachineRuntimeUnavailableReason) -> BuildPlatformFailure {
    let message = match reason {
        MachineRuntimeUnavailableReason::EncodeRequest { message } => failure_message(format!(
            "machine runtime request could not be encoded: {message}"
        )),
        MachineRuntimeUnavailableReason::RequestTimedOut => {
            failure_message("machine runtime request timed out")
        }
        MachineRuntimeUnavailableReason::NoResponders => {
            failure_message("machine runtime has no responders")
        }
        MachineRuntimeUnavailableReason::InvalidSubject => {
            failure_message("machine runtime subject was invalid")
        }
        MachineRuntimeUnavailableReason::MaxPayloadExceeded => {
            failure_message("machine runtime request exceeded NATS max payload")
        }
        MachineRuntimeUnavailableReason::RequestFailed { message } => {
            failure_message(format!("machine runtime request failed: {message}"))
        }
        MachineRuntimeUnavailableReason::ServiceBadRequest { message } => {
            failure_message(format!("machine runtime rejected the request: {message}"))
        }
        MachineRuntimeUnavailableReason::ServiceConflict { message } => {
            failure_message(format!("machine runtime reported a conflict: {message}"))
        }
        MachineRuntimeUnavailableReason::ServiceResponseTooLarge => {
            failure_message("machine runtime response_too_large")
        }
        MachineRuntimeUnavailableReason::ServiceUnavailable { message } => {
            failure_message(format!("machine runtime service unavailable: {message}"))
        }
        MachineRuntimeUnavailableReason::ServiceTimedOut { message } => {
            failure_message(format!("machine runtime service timed out: {message}"))
        }
        MachineRuntimeUnavailableReason::ServiceInternal { message } => failure_message(format!(
            "machine runtime service failed internally: {message}"
        )),
        MachineRuntimeUnavailableReason::MalformedServiceError { message } => failure_message(
            format!("machine runtime returned malformed service error headers: {message}"),
        ),
        MachineRuntimeUnavailableReason::DecodeResponse { message } => failure_message(format!(
            "machine runtime response could not be decoded: {message}"
        )),
        MachineRuntimeUnavailableReason::WrongResponder { actual_machine_id } => {
            failure_message(format!(
                "machine runtime replied for a different machine: {}",
                actual_machine_id.as_str()
            ))
        }
    };
    BuildPlatformFailure::MachineUnavailable { message }
}

#[cfg(test)]
mod tests;
