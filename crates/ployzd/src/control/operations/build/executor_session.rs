use std::time::Duration;

use futures_util::StreamExt;
use ployz_core::build::{
    BuildExecutorAcceptance, BuildExecutorAssignment, BuildExecutorCancelOk,
    BuildExecutorCancelOutcome, BuildExecutorEvidence, BuildExecutorStartOk, BuildExecutorStatus,
    BuildExecutorStatusFailure, BuildExecutorSuccessCleanupEvidence,
    BuildExecutorSuccessCleanupOutcome, BuildLogSummary,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::image::OciPlatform;
use ployz_core::operation::{
    BuildEvidence, BuildOperationFailure, BuildPlatformFailure, FailureMessage,
};
use ployz_nats::subjects::{MachineServiceEndpoint, machine_build_log};

use crate::control::role_client::machine::{MachineCallError, call_machine};
use crate::control::sequencer::AcceptedBuildExecution;
use crate::roles::machine::protocol::{
    MachineBuildCancelDomainError, MachineBuildCancelRpcOk, MachineBuildCancelRpcRequest,
    MachineBuildCleanupOutcome, MachineBuildStartDomainError, MachineBuildStartRpcOk,
    MachineBuildStartRpcRequest, MachineBuildStatusDomainError, MachineBuildStatusRpcOk,
    MachineBuildStatusRpcRequest,
};

use super::driver::{
    BuildOperationDriver, PlatformOutcome, failure_message, machine_failure, platform_failure,
    record_failure,
};
use super::log_stream::{MachineCallOrLog, next_machine_call_or_log};
use super::placement::ClusterBuildExecutorAssignment;
use super::platform_session::PlatformLogSession;

const BUILD_START_ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);
const BUILD_STATUS_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const BUILD_STATUS_POLL_INTERVAL: Duration = Duration::from_secs(1);
const BUILD_FINAL_RECONCILE_TIMEOUT: Duration = Duration::from_secs(5);
const BUILD_CANCEL_REQUEST_TIMEOUT: Duration = Duration::from_secs(1);

pub(super) async fn run_executor_session(
    driver: &BuildOperationDriver,
    accepted: &AcceptedBuildExecution,
    assignment: ClusterBuildExecutorAssignment,
) -> Result<PlatformOutcome, BuildOperationFailure> {
    let ClusterBuildExecutorAssignment {
        platform,
        machine_id,
    } = assignment;
    let executor = BuildExecutorAssignment::Cluster {
        machine_id: machine_id.clone(),
    };
    let evidence_executor = BuildExecutorEvidence::from_assignment(&executor);
    let id = &accepted.submission.operation_id;
    let logs = driver
        .client
        .subscribe(machine_build_log(&machine_id, id))
        .await
        .map_err(|error| {
            platform_failure(
                platform.clone(),
                machine_id.clone(),
                BuildPlatformFailure::MachineUnavailable {
                    message: failure_message(error),
                },
            )
        })?;
    if !driver.active.claim_executor_start(id, &executor).await {
        return Ok(PlatformOutcome::CancelledBeforeStart);
    }
    let mut log_session = PlatformLogSession::new(
        driver.controllers.repository(),
        id,
        &platform,
        executor.clone(),
        logs,
    );
    let request = build_start_request(accepted, executor.clone(), platform.clone(), driver.timeout);
    let expected = expected_acceptance(id, &machine_id, &platform);
    if !driver
        .active
        .executor_start_is_authorized(id, &executor)
        .await
    {
        driver
            .active
            .release_executor_start_claim(id, &executor)
            .await;
        return Ok(PlatformOutcome::CancelledBeforeStart);
    }

    let admission =
        receive_start_admission(driver, &machine_id, &request, &mut log_session).await?;
    let admission = match admission {
        Ok(ok) => Some(ok),
        Err(MachineCallError::Unavailable(_)) => None,
        Err(error @ MachineCallError::Domain(_)) => {
            let operation_failure = machine_failure(platform.clone(), machine_id.clone(), error);
            let BuildOperationFailure::PlatformFailed { failure, .. } = &operation_failure else {
                unreachable!("machine failure is platform-scoped")
            };
            record_platform_failure(driver, id, platform, evidence_executor, failure.clone())
                .await?;
            return Ok(PlatformOutcome::Failed(operation_failure));
        }
    };
    if let Some(admission) = admission
        && let Err(failure) = validate_executor_acceptance(&expected, &admission.executor)
    {
        let operation_failure =
            platform_failure(platform.clone(), machine_id.clone(), failure.clone());
        record_platform_failure(driver, id, platform, evidence_executor, failure).await?;
        return Ok(PlatformOutcome::Failed(operation_failure));
    }

    let summary =
        monitor_build(driver, &machine_id, &executor, &expected, &mut log_session).await?;
    log_session.drain(summary.log_summary()).await?;
    let BuildSummary::Completed(ok) = summary else {
        return match summary {
            BuildSummary::Failed { failure, .. } => {
                let operation_failure =
                    platform_failure(platform.clone(), machine_id.clone(), failure.clone());
                record_platform_failure(driver, id, platform, evidence_executor, failure).await?;
                Ok(PlatformOutcome::Failed(operation_failure))
            }
            BuildSummary::Cancelled { cleanup, .. } => {
                Ok(PlatformOutcome::Cancelled { executor, cleanup })
            }
            BuildSummary::TimedOut {
                message, cleanup, ..
            } => Ok(PlatformOutcome::TimedOut {
                executor,
                message,
                cleanup,
            }),
            BuildSummary::Completed(_) => unreachable!(),
        };
    };
    if let Err(failure) = validate_completed_image_seed(&machine_id, &ok) {
        let operation_failure =
            platform_failure(platform.clone(), machine_id.clone(), failure.clone());
        record_platform_failure(driver, id, platform, evidence_executor, failure).await?;
        return Ok(PlatformOutcome::Failed(operation_failure));
    }
    let BuildExecutorStartOk {
        acceptance: _,
        cleanup:
            BuildExecutorSuccessCleanupEvidence {
                outcome: BuildExecutorSuccessCleanupOutcome::Confirmed,
            },
        image,
        verified_source,
        toolchain,
        log_summary: _,
    } = *ok;
    driver
        .controllers
        .repository()
        .record_build_evidence(
            id,
            BuildEvidence::VerifiedSource {
                platform: platform.clone(),
                executor: evidence_executor.clone(),
                source: verified_source,
            },
        )
        .await
        .map_err(record_failure)?;
    driver
        .controllers
        .repository()
        .record_build_evidence(
            id,
            BuildEvidence::ToolchainVerified {
                platform: platform.clone(),
                executor: evidence_executor.clone(),
                toolchain,
            },
        )
        .await
        .map_err(record_failure)?;
    driver
        .controllers
        .repository()
        .record_build_evidence(
            id,
            BuildEvidence::PlatformCompleted {
                platform: platform.clone(),
                executor: evidence_executor,
                image: image.clone(),
            },
        )
        .await
        .map_err(record_failure)?;
    Ok(PlatformOutcome::Completed { platform, image })
}

async fn receive_start_admission(
    driver: &BuildOperationDriver,
    machine_id: &MachineId,
    request: &MachineBuildStartRpcRequest,
    log_session: &mut PlatformLogSession<'_>,
) -> Result<
    Result<MachineBuildStartRpcOk, MachineCallError<MachineBuildStartDomainError>>,
    BuildOperationFailure,
> {
    let machine_call = call_machine::<MachineBuildStartRpcOk, MachineBuildStartDomainError>(
        &driver.client,
        BUILD_START_ADMISSION_TIMEOUT,
        machine_id,
        MachineServiceEndpoint::BuildStart,
        request,
    );
    tokio::pin!(machine_call);
    loop {
        let logs_open = log_session.logs_open();
        match next_machine_call_or_log(machine_call.as_mut(), log_session.logs_mut(), logs_open)
            .await
        {
            MachineCallOrLog::Call(result) => return Ok(result),
            MachineCallOrLog::LogsClosed => {
                log_session.record_message(None).await?;
            }
            MachineCallOrLog::Log(message) => {
                log_session.record_message(message).await?;
            }
        }
    }
}

async fn monitor_build(
    driver: &BuildOperationDriver,
    machine_id: &MachineId,
    executor: &BuildExecutorAssignment,
    expected: &BuildExecutorAcceptance,
    log_session: &mut PlatformLogSession<'_>,
) -> Result<BuildSummary, BuildOperationFailure> {
    let mut watermark = BuildLogSummary::none();
    let mut status_watermark = BuildLogSummary::none();
    let mut silence_deadline = tokio::time::Instant::now() + driver.timeout;
    loop {
        if !driver
            .active
            .executor_start_is_authorized(&expected.operation_id, executor)
            .await
        {
            return reconcile_terminal(
                driver,
                machine_id,
                expected,
                log_session,
                ReconcileReason::Cancelled,
            )
            .await;
        }
        let remaining = silence_deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            return reconcile_terminal(
                driver,
                machine_id,
                expected,
                log_session,
                ReconcileReason::Stalled,
            )
            .await;
        }
        let status_request = MachineBuildStatusRpcRequest {
            acceptance: expected.clone(),
        };
        let status_call = call_machine::<MachineBuildStatusRpcOk, MachineBuildStatusDomainError>(
            &driver.client,
            remaining.min(BUILD_STATUS_REQUEST_TIMEOUT),
            machine_id,
            MachineServiceEndpoint::BuildStatus,
            &status_request,
        );
        tokio::pin!(status_call);
        let deadline = tokio::time::sleep_until(silence_deadline);
        tokio::pin!(deadline);
        let status_result = loop {
            tokio::select! {
                biased;
                result = &mut status_call => break Some(result),
                message = log_session.logs_mut().next(), if log_session.logs_open() => {
                    if log_session.record_message_progress(message).await? {
                        observe_progress(
                            &mut watermark,
                            log_session.observed_log_summary(),
                            driver.timeout,
                            &mut silence_deadline,
                            deadline.as_mut(),
                        );
                    }
                }
                () = &mut deadline => break None,
            }
        };
        let Some(status_result) = status_result else {
            return reconcile_terminal(
                driver,
                machine_id,
                expected,
                log_session,
                ReconcileReason::Stalled,
            )
            .await;
        };
        match status_result {
            Ok(ok) => match status_summary(expected, ok.executor)
                .map_err(|failure| status_operation_failure(expected, failure))?
            {
                StatusSummary::Running(summary) => {
                    if accept_status_progress(&mut status_watermark, summary) {
                        observe_progress(
                            &mut watermark,
                            summary,
                            driver.timeout,
                            &mut silence_deadline,
                            deadline.as_mut(),
                        );
                    }
                }
                StatusSummary::Terminal(summary) => return Ok(*summary),
            },
            Err(MachineCallError::Domain(error)) => {
                validate_status_error_acceptance(expected, &error)
                    .map_err(|failure| status_operation_failure(expected, failure))?;
            }
            Err(MachineCallError::Unavailable(_)) => {}
        }
        wait_for_next_poll(
            log_session,
            &mut watermark,
            driver.timeout,
            &mut silence_deadline,
        )
        .await?;
    }
}

fn accept_status_progress(
    status_watermark: &mut BuildLogSummary,
    observed: BuildLogSummary,
) -> bool {
    if !observed.strictly_advances(status_watermark) {
        return false;
    }
    *status_watermark = observed;
    true
}

async fn wait_for_next_poll(
    log_session: &mut PlatformLogSession<'_>,
    watermark: &mut BuildLogSummary,
    silence_budget: Duration,
    silence_deadline: &mut tokio::time::Instant,
) -> Result<(), BuildOperationFailure> {
    let poll = tokio::time::sleep(BUILD_STATUS_POLL_INTERVAL);
    let deadline = tokio::time::sleep_until(*silence_deadline);
    tokio::pin!(poll, deadline);
    loop {
        tokio::select! {
            biased;
            () = &mut deadline => return Ok(()),
            () = &mut poll => return Ok(()),
            message = log_session.logs_mut().next(), if log_session.logs_open() => {
                if log_session.record_message_progress(message).await? {
                    observe_progress(
                        watermark,
                        log_session.observed_log_summary(),
                        silence_budget,
                        silence_deadline,
                        deadline.as_mut(),
                    );
                }
            }
        }
    }
}

fn observe_progress(
    watermark: &mut BuildLogSummary,
    observed: BuildLogSummary,
    silence_budget: Duration,
    silence_deadline: &mut tokio::time::Instant,
    mut deadline: std::pin::Pin<&mut tokio::time::Sleep>,
) {
    let combined = BuildLogSummary::new(
        watermark
            .final_log_sequence
            .max(observed.final_log_sequence),
        watermark.omitted_log_bytes.max(observed.omitted_log_bytes),
    );
    if combined.strictly_advances(watermark) {
        *watermark = combined;
        *silence_deadline = tokio::time::Instant::now() + silence_budget;
        deadline.as_mut().reset(*silence_deadline);
    }
}

async fn reconcile_terminal(
    driver: &BuildOperationDriver,
    machine_id: &MachineId,
    expected: &BuildExecutorAcceptance,
    log_session: &mut PlatformLogSession<'_>,
    reason: ReconcileReason,
) -> Result<BuildSummary, BuildOperationFailure> {
    request_bounded_cancel(driver, machine_id, expected).await;
    let reconcile_deadline = tokio::time::Instant::now() + BUILD_FINAL_RECONCILE_TIMEOUT;
    while let Some(remaining) =
        reconcile_deadline.checked_duration_since(tokio::time::Instant::now())
    {
        if remaining.is_zero() {
            break;
        }
        let request = MachineBuildStatusRpcRequest {
            acceptance: expected.clone(),
        };
        let call = call_machine::<MachineBuildStatusRpcOk, MachineBuildStatusDomainError>(
            &driver.client,
            remaining.min(BUILD_STATUS_REQUEST_TIMEOUT),
            machine_id,
            MachineServiceEndpoint::BuildStatus,
            &request,
        );
        tokio::pin!(call);
        let deadline = tokio::time::sleep_until(reconcile_deadline);
        tokio::pin!(deadline);
        let result = loop {
            tokio::select! {
                biased;
                result = &mut call => break Some(result),
                message = log_session.logs_mut().next(), if log_session.logs_open() => {
                    log_session.record_message(message).await?;
                }
                () = &mut deadline => break None,
            }
        };
        let Some(result) = result else {
            break;
        };
        match result {
            Ok(ok) => match status_summary(expected, ok.executor)
                .map_err(|failure| status_operation_failure(expected, failure))?
            {
                StatusSummary::Terminal(summary) => return Ok(*summary),
                StatusSummary::Running(_) => {}
            },
            Err(MachineCallError::Domain(error)) => {
                validate_status_error_acceptance(expected, &error)
                    .map_err(|failure| status_operation_failure(expected, failure))?;
            }
            Err(MachineCallError::Unavailable(_)) => {}
        }
        if !wait_for_reconcile_poll(log_session, reconcile_deadline).await? {
            break;
        }
    }
    Ok(match reason {
        ReconcileReason::Cancelled => BuildSummary::Cancelled {
            cleanup: MachineBuildCleanupOutcome::Unconfirmed,
            log_summary: log_session.observed_log_summary(),
        },
        ReconcileReason::Stalled => BuildSummary::TimedOut {
            message: failure_message("machine build stopped making progress"),
            cleanup: MachineBuildCleanupOutcome::Unconfirmed,
            log_summary: log_session.observed_log_summary(),
        },
    })
}

async fn wait_for_reconcile_poll(
    log_session: &mut PlatformLogSession<'_>,
    deadline: tokio::time::Instant,
) -> Result<bool, BuildOperationFailure> {
    let poll = tokio::time::sleep(BUILD_STATUS_POLL_INTERVAL);
    let deadline = tokio::time::sleep_until(deadline);
    tokio::pin!(poll, deadline);
    loop {
        tokio::select! {
            biased;
            () = &mut deadline => return Ok(false),
            () = &mut poll => return Ok(true),
            message = log_session.logs_mut().next(), if log_session.logs_open() => {
                log_session.record_message(message).await?;
            }
        }
    }
}

async fn request_bounded_cancel(
    driver: &BuildOperationDriver,
    machine_id: &MachineId,
    expected: &BuildExecutorAcceptance,
) {
    let request = MachineBuildCancelRpcRequest {
        operation_id: expected.operation_id.clone(),
        assignment: expected.assignment.clone(),
    };
    let result = call_machine::<MachineBuildCancelRpcOk, MachineBuildCancelDomainError>(
        &driver.client,
        BUILD_CANCEL_REQUEST_TIMEOUT,
        machine_id,
        MachineServiceEndpoint::BuildCancel,
        &request,
    )
    .await;
    match result {
        Ok(MachineBuildCancelRpcOk {
            executor:
                BuildExecutorCancelOk {
                    assignment,
                    outcome:
                        BuildExecutorCancelOutcome::Requested | BuildExecutorCancelOutcome::NotRunning,
                },
            ..
        }) if assignment == expected.assignment => {}
        Ok(_) | Err(_) => {}
    }
}

fn status_summary(
    expected: &BuildExecutorAcceptance,
    status: BuildExecutorStatus,
) -> Result<StatusSummary, BuildPlatformFailure> {
    match status {
        BuildExecutorStatus::Running {
            acceptance,
            log_summary,
        } => {
            validate_status_acceptance(expected, &acceptance)?;
            Ok(StatusSummary::Running(log_summary))
        }
        BuildExecutorStatus::Completed { result } => {
            validate_status_acceptance(expected, &result.acceptance)?;
            Ok(StatusSummary::Terminal(Box::new(BuildSummary::Completed(
                result,
            ))))
        }
        BuildExecutorStatus::Failed {
            acceptance,
            failure,
            cleanup,
            log_summary,
        } => {
            validate_status_acceptance(expected, &acceptance)?;
            Ok(StatusSummary::Terminal(Box::new(match failure {
                BuildExecutorStatusFailure::PlatformFailed { failure } => BuildSummary::Failed {
                    failure,
                    log_summary,
                },
                BuildExecutorStatusFailure::Stalled { message } => BuildSummary::TimedOut {
                    message,
                    cleanup,
                    log_summary,
                },
            })))
        }
        BuildExecutorStatus::Cancelled {
            acceptance,
            cleanup,
            log_summary,
        } => {
            validate_status_acceptance(expected, &acceptance)?;
            Ok(StatusSummary::Terminal(Box::new(BuildSummary::Cancelled {
                cleanup,
                log_summary,
            })))
        }
    }
}

fn validate_status_error_acceptance(
    expected: &BuildExecutorAcceptance,
    error: &MachineBuildStatusDomainError,
) -> Result<(), BuildPlatformFailure> {
    let actual = match error {
        MachineBuildStatusDomainError::NotFound { acceptance }
        | MachineBuildStatusDomainError::EvidenceUnavailable {
            acceptance,
            message: _,
        } => acceptance,
    };
    validate_status_acceptance(expected, actual)
}

fn validate_status_acceptance(
    expected: &BuildExecutorAcceptance,
    actual: &BuildExecutorAcceptance,
) -> Result<(), BuildPlatformFailure> {
    validate_executor_acceptance(expected, actual)
}

fn status_operation_failure(
    expected: &BuildExecutorAcceptance,
    failure: BuildPlatformFailure,
) -> BuildOperationFailure {
    let BuildExecutorAssignment::Cluster { machine_id } = &expected.assignment else {
        unreachable!("machine status acceptance is cluster-scoped")
    };
    platform_failure(expected.platform.clone(), machine_id.clone(), failure)
}

async fn record_platform_failure(
    driver: &BuildOperationDriver,
    operation_id: &OperationId,
    platform: OciPlatform,
    executor: BuildExecutorEvidence,
    failure: BuildPlatformFailure,
) -> Result<(), BuildOperationFailure> {
    driver
        .controllers
        .repository()
        .record_build_evidence(
            operation_id,
            BuildEvidence::PlatformFailed {
                platform,
                executor,
                failure,
            },
        )
        .await
        .map_err(record_failure)?;
    Ok(())
}

fn build_start_request(
    accepted: &AcceptedBuildExecution,
    assignment: BuildExecutorAssignment,
    platform: OciPlatform,
    timeout: Duration,
) -> MachineBuildStartRpcRequest {
    MachineBuildStartRpcRequest {
        operation_id: accepted.submission.operation_id.clone(),
        assignment,
        source: accepted.source.clone(),
        adapter: accepted.submission.adapter.clone(),
        platform,
        timeout_millis: execution_timeout_millis(timeout),
    }
}

fn expected_acceptance(
    operation_id: &OperationId,
    machine_id: &MachineId,
    platform: &OciPlatform,
) -> BuildExecutorAcceptance {
    BuildExecutorAcceptance {
        operation_id: operation_id.clone(),
        assignment: BuildExecutorAssignment::Cluster {
            machine_id: machine_id.clone(),
        },
        platform: platform.clone(),
    }
}

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

fn validate_completed_image_seed(
    machine_id: &MachineId,
    completed: &BuildExecutorStartOk,
) -> Result<(), BuildPlatformFailure> {
    if completed.image.seed != *machine_id {
        return Err(BuildPlatformFailure::MachineUnavailable {
            message: failure_message("build executor returned an image from a different seed"),
        });
    }
    Ok(())
}

fn execution_timeout_millis(timeout: Duration) -> u64 {
    timeout.as_millis().try_into().unwrap_or(u64::MAX)
}

enum StatusSummary {
    Running(BuildLogSummary),
    Terminal(Box<BuildSummary>),
}

enum ReconcileReason {
    Cancelled,
    Stalled,
}

enum BuildSummary {
    Completed(Box<BuildExecutorStartOk>),
    Failed {
        failure: BuildPlatformFailure,
        log_summary: BuildLogSummary,
    },
    Cancelled {
        cleanup: MachineBuildCleanupOutcome,
        log_summary: BuildLogSummary,
    },
    TimedOut {
        message: FailureMessage,
        cleanup: MachineBuildCleanupOutcome,
        log_summary: BuildLogSummary,
    },
}

impl BuildSummary {
    const fn log_summary(&self) -> BuildLogSummary {
        match self {
            Self::Completed(ok) => ok.log_summary,
            Self::Failed { log_summary, .. }
            | Self::Cancelled { log_summary, .. }
            | Self::TimedOut { log_summary, .. } => *log_summary,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acceptance(machine: &str) -> BuildExecutorAcceptance {
        let machine_id = MachineId::try_new(machine).expect("machine id");
        BuildExecutorAcceptance {
            operation_id: OperationId::try_new("build-1").expect("operation id"),
            assignment: BuildExecutorAssignment::Cluster { machine_id },
            platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
        }
    }

    #[test]
    fn status_progress_must_advance_without_component_regression() {
        let mut watermark = BuildLogSummary::new(4, 8);

        assert!(!accept_status_progress(
            &mut watermark,
            BuildLogSummary::new(5, 7)
        ));
        assert!(!accept_status_progress(
            &mut watermark,
            BuildLogSummary::new(4, 8)
        ));
        assert_eq!(watermark, BuildLogSummary::new(4, 8));
        assert!(accept_status_progress(
            &mut watermark,
            BuildLogSummary::new(5, 8)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn strict_progress_renews_the_silence_deadline() {
        let budget = Duration::from_secs(10);
        let mut watermark = BuildLogSummary::none();
        let mut silence_deadline = tokio::time::Instant::now() + budget;
        let deadline = tokio::time::sleep_until(silence_deadline);
        tokio::pin!(deadline);

        tokio::time::advance(Duration::from_secs(9)).await;
        observe_progress(
            &mut watermark,
            BuildLogSummary::new(1, 0),
            budget,
            &mut silence_deadline,
            deadline.as_mut(),
        );
        tokio::time::advance(Duration::from_secs(9)).await;

        assert!(!deadline.is_elapsed());
        tokio::time::advance(Duration::from_secs(1)).await;
        deadline.await;
    }

    #[test]
    fn running_status_rejects_wrong_acceptance_provenance() {
        let expected = acceptance("machine-a");
        let actual = acceptance("machine-b");

        assert!(matches!(
            status_summary(
                &expected,
                BuildExecutorStatus::Running {
                    acceptance: actual,
                    log_summary: BuildLogSummary::none(),
                },
            ),
            Err(BuildPlatformFailure::MachineUnavailable { message })
                if message.as_str() == "build executor returned mismatched acceptance provenance"
        ));
    }
}
