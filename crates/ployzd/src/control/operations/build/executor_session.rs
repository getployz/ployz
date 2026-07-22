use std::time::Duration;

use futures_util::StreamExt;
use ployz_core::build::{
    BUILD_CONTROL_RESPONSE_MARGIN, BUILD_FORCE_CLEANUP_TIMEOUT, BUILD_TASK_DRAIN_TIMEOUT,
    BuildExecutorAcceptance, BuildExecutorAssignment, BuildExecutorCancelOk,
    BuildExecutorCancelOutcome, BuildExecutorCompletion, BuildExecutorEvidence, BuildExecutorState,
    BuildExecutorStatus, BuildExecutorStatusFailure, BuildExecutorSuccessCleanupEvidence,
    BuildExecutorSuccessCleanupOutcome, BuildLogSummary,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::image::OciPlatform;
use ployz_core::operation::{BuildEvidence, BuildOperationFailure, BuildPlatformFailure};
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
const BUILD_FINAL_RECONCILE_TIMEOUT: Duration = BUILD_TASK_DRAIN_TIMEOUT
    .saturating_add(BUILD_FORCE_CLEANUP_TIMEOUT)
    .saturating_add(BUILD_CONTROL_RESPONSE_MARGIN);
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
    let expected = BuildExecutorAcceptance::from_start_request(&request);
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
        Ok(ok) => ok,
        Err(MachineCallError::Unavailable(_)) => {
            match recover_uncertain_admission(
                driver,
                &machine_id,
                &request,
                &expected,
                &mut log_session,
            )
            .await
            {
                Ok(ok) => ok,
                Err(operation_failure) => {
                    let BuildOperationFailure::PlatformFailed { failure, .. } = &operation_failure
                    else {
                        unreachable!("machine admission failure is platform-scoped")
                    };
                    record_platform_failure(
                        driver,
                        id,
                        platform,
                        evidence_executor,
                        failure.clone(),
                    )
                    .await?;
                    return Ok(PlatformOutcome::Failed(operation_failure));
                }
            }
        }
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
    if let Err(failure) = validate_executor_acceptance(&expected, &admission.executor) {
        let operation_failure =
            platform_failure(platform.clone(), machine_id.clone(), failure.clone());
        record_platform_failure(driver, id, platform, evidence_executor, failure).await?;
        return Ok(PlatformOutcome::Failed(operation_failure));
    }

    let status = match monitor_build(driver, &machine_id, &executor, &expected, &mut log_session)
        .await
    {
        Ok(status) => status,
        Err(operation_failure @ BuildOperationFailure::PlatformFailed { .. }) => {
            let BuildOperationFailure::PlatformFailed { failure, .. } = &operation_failure else {
                unreachable!("matched platform failure")
            };
            record_platform_failure(driver, id, platform, evidence_executor, failure.clone())
                .await?;
            return Ok(PlatformOutcome::Failed(operation_failure));
        }
        Err(failure) => return Err(failure),
    };
    log_session.drain(status.log_summary()).await?;
    let BuildExecutorStatus {
        acceptance: _,
        state,
    } = status;
    let result = match state {
        BuildExecutorState::Completed { result } => *result,
        BuildExecutorState::Failed {
            failure, cleanup, ..
        } => match failure {
            BuildExecutorStatusFailure::Stalled { message } => {
                return Ok(PlatformOutcome::TimedOut {
                    executor,
                    message,
                    cleanup,
                });
            }
            BuildExecutorStatusFailure::PlatformFailed { failure } => {
                let operation_failure =
                    platform_failure(platform.clone(), machine_id.clone(), failure.clone());
                record_platform_failure(driver, id, platform, evidence_executor, failure).await?;
                return Ok(PlatformOutcome::Failed(operation_failure));
            }
        },
        BuildExecutorState::Cancelled { cleanup, .. } => {
            return Ok(PlatformOutcome::Cancelled { executor, cleanup });
        }
        BuildExecutorState::Running { .. } => unreachable!("monitor returns only terminal status"),
    };
    if let Err(failure) = validate_completed_image_seed(&machine_id, &result) {
        let operation_failure =
            platform_failure(platform.clone(), machine_id.clone(), failure.clone());
        record_platform_failure(driver, id, platform, evidence_executor, failure).await?;
        return Ok(PlatformOutcome::Failed(operation_failure));
    }
    let BuildExecutorCompletion {
        cleanup:
            BuildExecutorSuccessCleanupEvidence {
                outcome: BuildExecutorSuccessCleanupOutcome::Confirmed,
            },
        image,
        verified_source,
        toolchain,
        log_summary: _,
    } = result;
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

async fn recover_uncertain_admission(
    driver: &BuildOperationDriver,
    machine_id: &MachineId,
    request: &MachineBuildStartRpcRequest,
    expected: &BuildExecutorAcceptance,
    log_session: &mut PlatformLogSession<'_>,
) -> Result<MachineBuildStartRpcOk, BuildOperationFailure> {
    let status_request = MachineBuildStatusRpcRequest {
        acceptance: expected.clone(),
    };
    match call_machine::<MachineBuildStatusRpcOk, MachineBuildStatusDomainError>(
        &driver.client,
        BUILD_STATUS_REQUEST_TIMEOUT,
        machine_id,
        MachineServiceEndpoint::BuildStatus,
        &status_request,
    )
    .await
    {
        Ok(ok) => {
            validate_status(expected, ok.executor)
                .map_err(|failure| status_operation_failure(expected, failure))?;
            return Ok(MachineBuildStartRpcOk::from((
                machine_id.clone(),
                expected.clone(),
            )));
        }
        Err(MachineCallError::Domain(MachineBuildStatusDomainError::EvidenceUnavailable {
            acceptance,
            message,
        })) => {
            validate_status_acceptance(expected, &acceptance)
                .map_err(|failure| status_operation_failure(expected, failure))?;
            return Err(status_operation_failure(
                expected,
                BuildPlatformFailure::MachineUnavailable { message },
            ));
        }
        Err(MachineCallError::Domain(MachineBuildStatusDomainError::NotFound { acceptance })) => {
            validate_status_acceptance(expected, &acceptance)
                .map_err(|failure| status_operation_failure(expected, failure))?;
        }
        Err(MachineCallError::Unavailable(_)) => {}
    }

    match receive_start_admission(driver, machine_id, request, log_session).await? {
        Ok(ok) => Ok(ok),
        Err(error) => Err(machine_failure(
            expected.platform.clone(),
            machine_id.clone(),
            error,
        )),
    }
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
) -> Result<BuildExecutorStatus, BuildOperationFailure> {
    let mut watermark = BuildLogSummary::none();
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
                &mut watermark,
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
                &mut watermark,
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
                        merge_progress(
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
                &mut watermark,
                ReconcileReason::Stalled,
            )
            .await;
        };
        match status_result {
            Ok(ok) => {
                let status = validate_status(expected, ok.executor)
                    .map_err(|failure| status_operation_failure(expected, failure))?;
                match status.state {
                    BuildExecutorState::Running {
                        log_summary: summary,
                    } => {
                        merge_progress(
                            &mut watermark,
                            summary,
                            driver.timeout,
                            &mut silence_deadline,
                            deadline.as_mut(),
                        );
                    }
                    BuildExecutorState::Completed { .. }
                    | BuildExecutorState::Failed { .. }
                    | BuildExecutorState::Cancelled { .. } => {
                        validate_terminal_progress(&status, watermark)
                            .map_err(|failure| status_operation_failure(expected, failure))?;
                        return Ok(status);
                    }
                }
            }
            Err(MachineCallError::Domain(MachineBuildStatusDomainError::EvidenceUnavailable {
                acceptance,
                message,
            })) => {
                validate_status_acceptance(expected, &acceptance)
                    .map_err(|failure| status_operation_failure(expected, failure))?;
                return Err(status_operation_failure(
                    expected,
                    BuildPlatformFailure::MachineUnavailable { message },
                ));
            }
            Err(MachineCallError::Domain(MachineBuildStatusDomainError::NotFound {
                acceptance,
            })) => {
                validate_status_acceptance(expected, &acceptance)
                    .map_err(|failure| status_operation_failure(expected, failure))?;
                return Err(status_operation_failure(
                    expected,
                    BuildPlatformFailure::MachineUnavailable {
                        message: failure_message("accepted machine build status was not found"),
                    },
                ));
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
                    merge_progress(
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

fn merge_progress(
    watermark: &mut BuildLogSummary,
    observed: BuildLogSummary,
    silence_budget: Duration,
    silence_deadline: &mut tokio::time::Instant,
    mut deadline: std::pin::Pin<&mut tokio::time::Sleep>,
) -> bool {
    if merge_progress_without_deadline(watermark, observed) {
        *silence_deadline = tokio::time::Instant::now() + silence_budget;
        deadline.as_mut().reset(*silence_deadline);
        return true;
    }
    false
}

fn validate_terminal_progress(
    status: &BuildExecutorStatus,
    watermark: BuildLogSummary,
) -> Result<(), BuildPlatformFailure> {
    let terminal = status.log_summary();
    if terminal == watermark || terminal.strictly_advances(&watermark) {
        return Ok(());
    }
    Err(progress_regression_failure())
}

fn progress_regression_failure() -> BuildPlatformFailure {
    BuildPlatformFailure::MachineUnavailable {
        message: failure_message("build executor returned regressing log progress"),
    }
}

pub(super) async fn reconcile_terminal(
    driver: &BuildOperationDriver,
    machine_id: &MachineId,
    expected: &BuildExecutorAcceptance,
    log_session: &mut PlatformLogSession<'_>,
    watermark: &mut BuildLogSummary,
    reason: ReconcileReason,
) -> Result<BuildExecutorStatus, BuildOperationFailure> {
    let reconcile_deadline = tokio::time::Instant::now() + BUILD_FINAL_RECONCILE_TIMEOUT;
    while let Some(remaining) =
        reconcile_deadline.checked_duration_since(tokio::time::Instant::now())
    {
        if remaining.is_zero() {
            break;
        }
        request_bounded_cancel(driver, machine_id, expected, reconcile_deadline).await;
        let Some(remaining) =
            reconcile_deadline.checked_duration_since(tokio::time::Instant::now())
        else {
            break;
        };
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
                    if log_session.record_message_progress(message).await? {
                        merge_progress_without_deadline(
                            watermark,
                            log_session.observed_log_summary(),
                        );
                    }
                }
                () = &mut deadline => break None,
            }
        };
        let Some(result) = result else {
            break;
        };
        match result {
            Ok(ok) => {
                let status = validate_status(expected, ok.executor)
                    .map_err(|failure| status_operation_failure(expected, failure))?;
                if status.is_terminal() {
                    validate_terminal_progress(&status, *watermark)
                        .map_err(|failure| status_operation_failure(expected, failure))?;
                    return Ok(status);
                }
                merge_progress_without_deadline(watermark, status.log_summary());
            }
            Err(MachineCallError::Domain(MachineBuildStatusDomainError::EvidenceUnavailable {
                acceptance,
                message,
            })) => {
                validate_status_acceptance(expected, &acceptance)
                    .map_err(|failure| status_operation_failure(expected, failure))?;
                return Err(status_operation_failure(
                    expected,
                    BuildPlatformFailure::MachineUnavailable { message },
                ));
            }
            Err(MachineCallError::Domain(MachineBuildStatusDomainError::NotFound {
                acceptance,
            })) => {
                validate_status_acceptance(expected, &acceptance)
                    .map_err(|failure| status_operation_failure(expected, failure))?;
            }
            Err(MachineCallError::Unavailable(_)) => {}
        }
        if !wait_for_reconcile_poll(log_session, watermark, reconcile_deadline).await? {
            break;
        }
    }
    Ok(BuildExecutorStatus {
        acceptance: expected.clone(),
        state: match reason {
            ReconcileReason::Cancelled => BuildExecutorState::Cancelled {
                cleanup: MachineBuildCleanupOutcome::Unconfirmed,
                log_summary: *watermark,
            },
            ReconcileReason::Stalled => BuildExecutorState::Failed {
                failure: BuildExecutorStatusFailure::Stalled {
                    message: failure_message("machine build stopped making progress"),
                },
                cleanup: MachineBuildCleanupOutcome::Unconfirmed,
                log_summary: *watermark,
            },
        },
    })
}

async fn wait_for_reconcile_poll(
    log_session: &mut PlatformLogSession<'_>,
    watermark: &mut BuildLogSummary,
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
                if log_session.record_message_progress(message).await? {
                    merge_progress_without_deadline(
                        watermark,
                        log_session.observed_log_summary(),
                    );
                }
            }
        }
    }
}

async fn request_bounded_cancel(
    driver: &BuildOperationDriver,
    machine_id: &MachineId,
    expected: &BuildExecutorAcceptance,
    deadline: tokio::time::Instant,
) {
    let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
        return;
    };
    if remaining.is_zero() {
        return;
    }
    let request = MachineBuildCancelRpcRequest {
        operation_id: expected.operation_id.clone(),
        assignment: expected.assignment.clone(),
    };
    let result = call_machine::<MachineBuildCancelRpcOk, MachineBuildCancelDomainError>(
        &driver.client,
        remaining.min(BUILD_CANCEL_REQUEST_TIMEOUT),
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

fn merge_progress_without_deadline(
    watermark: &mut BuildLogSummary,
    observed: BuildLogSummary,
) -> bool {
    let combined = BuildLogSummary::new(
        watermark
            .final_log_sequence
            .max(observed.final_log_sequence),
        watermark.omitted_log_bytes.max(observed.omitted_log_bytes),
    );
    let advanced = combined != *watermark;
    *watermark = combined;
    advanced
}

fn validate_status(
    expected: &BuildExecutorAcceptance,
    status: BuildExecutorStatus,
) -> Result<BuildExecutorStatus, BuildPlatformFailure> {
    validate_status_acceptance(expected, &status.acceptance)?;
    Ok(status)
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

pub(super) fn build_start_request(
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
    completed: &BuildExecutorCompletion,
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

pub(super) enum ReconcileReason {
    Cancelled,
    Stalled,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acceptance(machine: &str) -> BuildExecutorAcceptance {
        let machine_id = MachineId::try_new(machine).expect("machine id");
        BuildExecutorAcceptance::from_start_request(&MachineBuildStartRpcRequest {
            operation_id: OperationId::try_new("build-1").expect("operation id"),
            assignment: BuildExecutorAssignment::Cluster { machine_id },
            source: ployz_core::build::GitSource::try_new(
                "https://example.test/repo.git",
                "0123456789abcdef0123456789abcdef01234567",
                "git",
                "secret",
                None::<String>,
            )
            .expect("source")
            .into(),
            adapter: ployz_core::build::BuildAdapter::Dockerfile {
                dockerfile: ployz_core::build::BuildContextPath::try_new("Dockerfile")
                    .expect("dockerfile"),
                target: None,
            },
            platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
            timeout_millis: 1_000,
        })
    }

    #[test]
    fn partial_progress_observations_merge_into_one_componentwise_watermark() {
        let mut watermark = BuildLogSummary::new(4, 8);

        assert!(merge_progress_without_deadline(
            &mut watermark,
            BuildLogSummary::new(5, 0)
        ));
        assert_eq!(watermark, BuildLogSummary::new(5, 8));
        assert!(!merge_progress_without_deadline(
            &mut watermark,
            BuildLogSummary::new(4, 7)
        ));
        assert!(!merge_progress_without_deadline(
            &mut watermark,
            BuildLogSummary::new(5, 8)
        ));
        assert_eq!(watermark, BuildLogSummary::new(5, 8));
    }

    #[test]
    fn final_reconciliation_stays_within_the_operation_budget() {
        assert_eq!(BUILD_FINAL_RECONCILE_TIMEOUT, Duration::from_secs(65));
        assert!(BUILD_FINAL_RECONCILE_TIMEOUT < Duration::from_secs(70));
    }

    #[test]
    fn terminal_progress_must_cover_the_combined_control_watermark() {
        let status = BuildExecutorStatus {
            acceptance: acceptance("machine-a"),
            state: BuildExecutorState::Cancelled {
                cleanup: MachineBuildCleanupOutcome::Confirmed,
                log_summary: BuildLogSummary::new(5, 7),
            },
        };
        assert!(matches!(
            validate_terminal_progress(&status, BuildLogSummary::new(5, 8)),
            Err(BuildPlatformFailure::MachineUnavailable { message })
                if message.as_str() == "build executor returned regressing log progress"
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
        assert!(merge_progress(
            &mut watermark,
            BuildLogSummary::new(1, 0),
            budget,
            &mut silence_deadline,
            deadline.as_mut(),
        ));
        tokio::time::advance(Duration::from_secs(9)).await;

        assert!(!deadline.is_elapsed());
        tokio::time::advance(Duration::from_secs(1)).await;
        deadline.await;
    }

    #[tokio::test(start_paused = true)]
    async fn stale_partial_progress_does_not_renew_the_silence_deadline() {
        let budget = Duration::from_secs(10);
        let mut watermark = BuildLogSummary::new(5, 8);
        let mut silence_deadline = tokio::time::Instant::now() + budget;
        let original_deadline = silence_deadline;
        let deadline = tokio::time::sleep_until(silence_deadline);
        tokio::pin!(deadline);

        tokio::time::advance(Duration::from_secs(9)).await;
        assert!(!merge_progress(
            &mut watermark,
            BuildLogSummary::new(4, 7),
            budget,
            &mut silence_deadline,
            deadline.as_mut(),
        ));
        tokio::time::advance(Duration::from_secs(1)).await;

        assert_eq!(silence_deadline, original_deadline);
        assert_eq!(watermark, BuildLogSummary::new(5, 8));
    }

    #[test]
    fn running_status_rejects_wrong_acceptance_provenance() {
        let expected = acceptance("machine-a");
        let actual = acceptance("machine-b");

        assert!(matches!(
            validate_status(
                &expected,
                BuildExecutorStatus {
                    acceptance: actual,
                    state: BuildExecutorState::Running {
                        log_summary: BuildLogSummary::none(),
                    },
                },
            ),
            Err(BuildPlatformFailure::MachineUnavailable { message })
                if message.as_str() == "build executor returned mismatched acceptance provenance"
        ));
    }
}
