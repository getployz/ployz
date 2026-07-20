use std::future::Future;
use std::time::Duration;

use ployz_core::deploy::{ReplicaSlot, VolumeName};
use ployz_core::ids::{ContainerId, MachineId, StepId, SubjectTokenError};
use ployz_core::operation::{ControlPlaneCommitScope, RouteHostname, RouteTarget};

use super::{DeployExecutionCommand, DeployExecutionError};
use crate::control::operation_evidence::{RecordDeployEvidenceError, RecordDeployTransitionError};

#[derive(Debug, thiserror::Error)]
pub enum DeployOperationRecordError {
    #[error("deploy transition write failed: {0:?}")]
    RecordTransition(RecordDeployTransitionError),
    #[error("deploy evidence write failed: {0:?}")]
    RecordEvidence(RecordDeployEvidenceError),
    #[cfg(test)]
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
    RunContainer {
        machine_id: MachineId,
    },
    RunPreStartHook {
        machine_id: MachineId,
    },
    StopVolumeOwner {
        machine_id: MachineId,
        container_id: ContainerId,
    },
    QuiesceVolumeConsumer {
        machine_id: MachineId,
        container_id: ContainerId,
    },
    RestartVolumeOwner {
        machine_id: MachineId,
        container_id: ContainerId,
    },
    WaitHealthy,
    EnsureCertificate {
        hostname: RouteHostname,
    },
    CommitVolumePins,
    EnsureVolume {
        machine_id: MachineId,
        volume_name: VolumeName,
    },
    RemoveRoute {
        route: RouteTarget,
    },
    RemoveServingTarget {
        scope: ControlPlaneCommitScope,
    },
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

pub(super) fn deploy_step_id(
    slot: ReplicaSlot,
    machine_id: &MachineId,
) -> Result<StepId, SubjectTokenError> {
    match slot {
        ReplicaSlot::Replicated { number } => StepId::try_new(format!("run_{}", number.get())),
        ReplicaSlot::Global => StepId::try_new(format!("run_global_{}", machine_id.as_str())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::ReplicatedReplicaSlot;

    fn machine(value: &str) -> MachineId {
        MachineId::try_new(value).expect("machine id")
    }

    #[test]
    fn deploy_step_ids_are_mode_specific_and_global_ids_are_machine_keyed() {
        assert_eq!(
            deploy_step_id(
                ReplicaSlot::Replicated {
                    number: ReplicatedReplicaSlot::try_new(2).expect("slot"),
                },
                &machine("machine_a"),
            )
            .expect("step id")
            .as_str(),
            "run_2"
        );
        assert_eq!(
            deploy_step_id(ReplicaSlot::Global, &machine("machine_a"))
                .expect("step id")
                .as_str(),
            "run_global_machine_a"
        );
    }
}
