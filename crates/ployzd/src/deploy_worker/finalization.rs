use ployz_core::ops::DeployTransition;
use ployz_core::state::{ActiveServiceCommit, ActiveServiceCommitRequest};
use std::time::Duration;

use super::failure::{DeployExecutionFailure, failure};
use super::{
    ActiveServiceCommitRejection, ActiveServiceCommitter, DeployCompletionRecord,
    DeployCompletionRecordFailure, DeployContainer, DeployExecutionCommand, DeployExecutionError,
    DeployExecutionStep, DeployOperationRecorder,
};

pub(super) async fn finalize_successful_deploy<A, R>(
    command: &DeployExecutionCommand,
    active_state: &mut A,
    recorder: &mut R,
) -> Result<DeployCompletionRecord, DeployFinalizationError>
where
    A: ActiveServiceCommitter,
    R: DeployOperationRecorder,
{
    commit_active_service_with_timeout(
        command.active_service_commit_request(),
        command.step_timeout(),
        active_state,
    )
    .await
    .map_err(DeployFinalizationError::RecordFailed)?;

    Ok(record_deploy_completion(command, recorder)
        .await
        .unwrap_or_else(|reason| DeployCompletionRecord::Missing { reason }))
}

#[derive(Debug)]
pub(super) enum DeployFinalizationError {
    RecordFailed(DeployExecutionError),
}

impl DeployFinalizationError {
    pub(super) fn into_execution_failure(
        self,
        deploy_containers: &[DeployContainer],
    ) -> DeployExecutionFailure {
        match self {
            Self::RecordFailed(source) => failure(source, deploy_containers),
        }
    }
}

async fn commit_active_service_with_timeout<A>(
    request: ActiveServiceCommitRequest,
    timeout: Duration,
    active_state: &mut A,
) -> Result<(), DeployExecutionError>
where
    A: ActiveServiceCommitter,
{
    match tokio::time::timeout(
        timeout,
        commit_active_service_request(active_state, request),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(DeployExecutionError::StepTimedOut {
            step: DeployExecutionStep::CommitActiveService,
            timeout,
        }),
    }
}

async fn record_deploy_completion<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
) -> Result<DeployCompletionRecord, DeployCompletionRecordFailure>
where
    R: DeployOperationRecorder,
{
    match tokio::time::timeout(
        command.step_timeout(),
        recorder.record_deploy_transition(&command.operation_id, DeployTransition::Completed),
    )
    .await
    {
        Ok(Ok(())) => Ok(DeployCompletionRecord::Recorded),
        Ok(Err(_source)) => Err(DeployCompletionRecordFailure::RecordRejected),
        Err(_) => Err(DeployCompletionRecordFailure::TimedOut {
            timeout: command.step_timeout(),
        }),
    }
}

async fn commit_active_service_request<A>(
    active_state: &mut A,
    request: ActiveServiceCommitRequest,
) -> Result<(), DeployExecutionError>
where
    A: ActiveServiceCommitter,
{
    match active_state
        .commit_active_service(request)
        .await
        .map_err(DeployExecutionError::CommitActiveService)?
    {
        ActiveServiceCommit::Stored { .. } | ActiveServiceCommit::AlreadyCommitted { .. } => Ok(()),
        ActiveServiceCommit::Stale { reason } => {
            Err(DeployExecutionError::ActiveServiceCommitRejected {
                reason: ActiveServiceCommitRejection::Stale { reason },
            })
        }
        ActiveServiceCommit::Contended {
            current_revision,
            attempted_revision,
            expected_current,
        } => Err(DeployExecutionError::ActiveServiceCommitRejected {
            reason: ActiveServiceCommitRejection::Contended {
                current_revision,
                attempted_revision,
                expected_current,
            },
        }),
    }
}
