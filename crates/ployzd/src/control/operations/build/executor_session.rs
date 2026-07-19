use std::time::Duration;

use ployz_core::build::{
    BuildExecutorAcceptance, BuildExecutorAssignment, BuildExecutorEvidence, BuildLogSummary,
    build_control_request_timeout,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::image::OciPlatform;
use ployz_core::operation::{
    BuildEvidence, BuildOperationFailure, BuildPlatformFailure, FailureMessage,
};
use ployz_nats::subjects::{MachineServiceEndpoint, machine_build_log};

use crate::control::role_client::machine::{MachineCallError, call_machine};
use crate::control::sequencer::AcceptedBuildExecution;
use crate::roles::machine::MachineRuntimeUnavailableReason;
use crate::roles::machine::protocol::{
    MachineBuildCleanupOutcome, MachineBuildStartDomainError, MachineBuildStartRpcOk,
    MachineBuildStartRpcRequest,
};

use super::driver::{
    BuildOperationDriver, PlatformOutcome, failure_message, machine_failure, platform_failure,
    record_failure,
};
use super::log_stream::{MachineCallOrLog, next_machine_call_or_log};
use super::placement::ClusterBuildExecutorAssignment;
use super::platform_session::PlatformLogSession;

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
    let result = receive_executor_result(driver, &machine_id, &request, &mut log_session).await?;
    let summary = match result {
        Ok(ok) => BuildSummary::Completed(ok),
        Err(MachineCallError::Domain(MachineBuildStartDomainError::PlatformFailed {
            acceptance,
            failure,
            log_summary,
        })) => BuildSummary::Failed {
            acceptance: *acceptance,
            failure,
            log_summary,
        },
        Err(MachineCallError::Domain(MachineBuildStartDomainError::Cancelled {
            acceptance,
            cleanup,
            log_summary,
        })) => BuildSummary::Cancelled {
            acceptance: *acceptance,
            cleanup,
            log_summary,
        },
        Err(MachineCallError::Domain(MachineBuildStartDomainError::TimedOut {
            acceptance,
            message,
            cleanup,
            log_summary,
        })) => BuildSummary::TimedOut {
            acceptance: *acceptance,
            message,
            cleanup,
            log_summary,
        },
        Err(MachineCallError::Unavailable(
            reason @ (MachineRuntimeUnavailableReason::RequestTimedOut
            | MachineRuntimeUnavailableReason::ServiceTimedOut { .. }),
        )) => {
            log_session.drain(BuildLogSummary::none()).await?;
            return Ok(PlatformOutcome::TimedOut {
                executor,
                message: reason.failure_message(),
                cleanup: MachineBuildCleanupOutcome::Unconfirmed,
            });
        }
        Err(error) => {
            let operation_failure = machine_failure(platform.clone(), machine_id.clone(), error);
            let BuildOperationFailure::PlatformFailed { failure, .. } = &operation_failure else {
                unreachable!("machine failure is platform-scoped")
            };
            record_platform_failure(driver, id, platform, evidence_executor, failure.clone())
                .await?;
            return Ok(PlatformOutcome::Failed(operation_failure));
        }
    };
    if let Err(failure) = validate_executor_acceptance(
        &expected_acceptance(id, &machine_id, &platform),
        summary.acceptance(),
    ) {
        let operation_failure =
            platform_failure(platform.clone(), machine_id.clone(), failure.clone());
        record_platform_failure(driver, id, platform, evidence_executor, failure).await?;
        return Ok(PlatformOutcome::Failed(operation_failure));
    }
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
    let ok = ok.into_executor();
    driver
        .controllers
        .repository()
        .record_build_evidence(
            id,
            BuildEvidence::VerifiedCommit {
                platform: platform.clone(),
                executor: evidence_executor.clone(),
                commit: ok.verified_commit,
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
                toolchain: ok.toolchain,
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
                image: ok.image.clone(),
            },
        )
        .await
        .map_err(record_failure)?;
    Ok(PlatformOutcome::Completed {
        platform,
        image: ok.image,
    })
}

async fn receive_executor_result(
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
        build_control_request_timeout(driver.timeout),
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
            MachineCallOrLog::LogsClosed => log_session.record_message(None).await?,
            MachineCallOrLog::Log(message) => log_session.record_message(message).await?,
        }
    }
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
    completed: &MachineBuildStartRpcOk,
) -> Result<(), BuildPlatformFailure> {
    if completed.executor.image.seed != *machine_id {
        return Err(BuildPlatformFailure::MachineUnavailable {
            message: failure_message("build executor returned an image from a different seed"),
        });
    }
    Ok(())
}

fn execution_timeout_millis(timeout: Duration) -> u64 {
    timeout.as_millis().try_into().unwrap_or(u64::MAX)
}

enum BuildSummary {
    Completed(MachineBuildStartRpcOk),
    Failed {
        acceptance: BuildExecutorAcceptance,
        failure: BuildPlatformFailure,
        log_summary: BuildLogSummary,
    },
    Cancelled {
        acceptance: BuildExecutorAcceptance,
        cleanup: MachineBuildCleanupOutcome,
        log_summary: BuildLogSummary,
    },
    TimedOut {
        acceptance: BuildExecutorAcceptance,
        message: FailureMessage,
        cleanup: MachineBuildCleanupOutcome,
        log_summary: BuildLogSummary,
    },
}

impl BuildSummary {
    fn acceptance(&self) -> &BuildExecutorAcceptance {
        match self {
            Self::Completed(ok) => &ok.executor.acceptance,
            Self::Failed { acceptance, .. }
            | Self::Cancelled { acceptance, .. }
            | Self::TimedOut { acceptance, .. } => acceptance,
        }
    }

    fn log_summary(&self) -> BuildLogSummary {
        match self {
            Self::Completed(ok) => ok.executor.log_summary,
            Self::Failed { log_summary, .. }
            | Self::Cancelled { log_summary, .. }
            | Self::TimedOut { log_summary, .. } => *log_summary,
        }
    }
}
