use ployz_core::ops::DeployTransition;
use ployz_core::state::{ActiveServiceCommit, ActiveServiceCommitRequest};
use std::time::Duration;

use super::{
    ActiveServiceCommitter, DeployCompletionRecord, DeployCompletionRecordFailure,
    DeployExecutionCommand, DeployExecutionError, DeployOperationRecorder,
};

pub(super) async fn finalize_successful_deploy<A, R>(
    command: &DeployExecutionCommand,
    active_state: &mut A,
    recorder: &mut R,
) -> Result<DeployCompletionRecord, DeployExecutionError>
where
    A: ActiveServiceCommitter,
    R: DeployOperationRecorder,
{
    commit_active_service_with_timeout(
        command.active_service_commit_request(),
        command.step_timeout(),
        active_state,
    )
    .await?;

    Ok(record_deploy_completion(command, recorder).await)
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
            step: super::DeployExecutionStep::CommitActiveService,
            timeout,
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
        ActiveServiceCommit::ActiveServiceChanged {
            expected_current,
            current_revision,
            attempted_revision,
        } => Err(DeployExecutionError::ActiveServiceCommitRejected {
            expected_current,
            current_revision,
            attempted_revision,
        }),
    }
}

async fn record_deploy_completion<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
) -> DeployCompletionRecord
where
    R: DeployOperationRecorder,
{
    match tokio::time::timeout(
        command.step_timeout(),
        recorder.record_deploy_transition(&command.operation_id, DeployTransition::Completed),
    )
    .await
    {
        Ok(Ok(())) => DeployCompletionRecord::Recorded,
        Ok(Err(_)) => DeployCompletionRecord::Uncertain {
            reason: DeployCompletionRecordFailure::RecordRejected,
        },
        Err(_) => DeployCompletionRecord::Uncertain {
            reason: DeployCompletionRecordFailure::TimedOut {
                timeout: command.step_timeout(),
            },
        },
    }
}
