use std::future::Future;
use std::time::Duration;

use ployz_core::ids::MachineId;
use ployz_core::operation::{ControlPlaneCommitScope, RouteHostname, RouteTarget};

use super::{DeployExecutionCommand, DeployExecutionError};
use crate::control::operation_evidence::{RecordDeployEvidenceError, RecordDeployTransitionError};

#[derive(Debug, thiserror::Error)]
pub enum DeployOperationRecordError {
    #[error("deploy transition write failed: {0:?}")]
    RecordTransition(RecordDeployTransitionError),
    #[error("deploy evidence write failed: {0:?}")]
    RecordEvidence(RecordDeployEvidenceError),
    #[error("synthetic deploy record failure: {message}")]
    Synthetic { message: &'static str },
}

#[derive(Debug, thiserror::Error)]
pub enum DeployFailureRecordError {
    #[error("failure evidence write timed out after {timeout:?}")]
    TimedOut { timeout: Duration },
    #[error("failure evidence write failed: {0}")]
    Record(DeployOperationRecordError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployExecutionStep {
    RecordOperationEvent,
    RunContainer { machine_id: MachineId },
    RunPreStartHook { machine_id: MachineId },
    WaitHealthy,
    EnsureCertificate { hostname: RouteHostname },
    CommitVolumePins,
    RemoveRoute { route: RouteTarget },
    CommitServingTarget { scope: ControlPlaneCommitScope },
    RemoveServingTarget { scope: ControlPlaneCommitScope },
}

pub(super) async fn with_step_timeout<T, E, F>(
    command: &DeployExecutionCommand,
    step: DeployExecutionStep,
    future: F,
) -> Result<T, DeployExecutionError>
where
    F: Future<Output = Result<T, E>>,
    E: Into<DeployExecutionError>,
{
    let timeout = command.step_timeout();
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| DeployExecutionError::StepTimedOut { step, timeout })?
        .map_err(Into::into)
}
