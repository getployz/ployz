//! Operation-owned machine-local ZFS preparation.

use std::time::{Duration, Instant};

use crate::control::operation_evidence::{
    AcceptedMachineStoragePrepareSubmission, RecordOperationEventError,
};
use crate::control::role_client::machine::{
    MachineStoragePrepareError, NatsMachineSubstrateUpdater,
};
use crate::control::sequencer::OperationControllers;
use crate::roles::machine::protocol::MachineStoragePrepareRpcRequest;
use crate::tasks::TaskSpawner;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::{
    FailureMessage, MachineStoragePrepareFailure, MachineStoragePrepareTransition,
};

const REPORT_TIMEOUT: Duration = Duration::from_secs(120);
const REPORT_POLL_INTERVAL: Duration = Duration::from_secs(1);

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
        let machine_id = accepted.machine_id.clone();
        let runtime = self.clone();
        let admission = self.task_registry.spawn(|| async move {
            runtime.run(accepted).await;
        });
        let rejected = admission.is_err();
        super::finish_rejected_task_admission(&self.controllers, &operation_id, admission).await;
        if rejected {
            self.controllers
                .release_machine(&machine_id, &operation_id)
                .await;
        }
    }

    async fn run(self, accepted: AcceptedMachineStoragePrepareSubmission) {
        let operation_id = accepted.operation_id.clone();
        let machine_id = accepted.machine_id.clone();
        self.clone().run_inner(accepted).await;
        self.controllers
            .release_machine(&machine_id, &operation_id)
            .await;
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
        if let Err(error) = self
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
            self.record_failed(&operation_id, &machine_id, operation_failure(error))
                .await;
            return;
        }
        let deadline = Instant::now() + REPORT_TIMEOUT;
        let pool = loop {
            match self
                .updater
                .report_storage_prepare(&machine_id, &operation_id)
                .await
            {
                Ok(Some(pool)) => break pool,
                Ok(None) if Instant::now() < deadline => {
                    tokio::time::sleep(REPORT_POLL_INTERVAL).await;
                }
                Ok(None) => {
                    self.record_failed(
                        &operation_id,
                        &machine_id,
                        MachineStoragePrepareFailure::EvidenceUnavailable {
                            machine_id: machine_id.clone(),
                            message: storage_failure_message(
                                "storage preparation did not produce terminal evidence before the deadline",
                            ),
                        },
                    )
                    .await;
                    return;
                }
                Err(MachineStoragePrepareError::Unavailable { .. })
                    if Instant::now() < deadline =>
                {
                    tokio::time::sleep(REPORT_POLL_INTERVAL).await;
                }
                Err(error) => {
                    self.record_failed(&operation_id, &machine_id, operation_failure(error))
                        .await;
                    return;
                }
            }
        };
        if let Err(error) = self
            .controllers
            .repository()
            .record_machine_storage_prepare_transition(
                &operation_id,
                &machine_id,
                MachineStoragePrepareTransition::Completed { pool },
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
            message,
        } => MachineStoragePrepareFailure::PreparationRejected {
            machine_id,
            message,
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
