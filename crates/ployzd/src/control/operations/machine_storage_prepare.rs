//! Operation-owned machine-local ZFS preparation.

use std::time::{Duration, Instant};

use crate::control::operation_evidence::{
    AcceptedMachineStoragePrepareSubmission, RecordOperationEventError,
};
use crate::control::role_client::machine::{
    MachineStoragePrepareError, NatsMachineSubstrateUpdater,
};
use crate::control::sequencer::OperationControllers;
use crate::roles::machine::MachineRuntimeUnavailableReason;
use crate::roles::machine::protocol::MachineStoragePrepareRpcRequest;
use crate::tasks::TaskSpawner;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::{
    FailureMessage, MachineStoragePrepareFailure, MachineStoragePrepareTransition,
};

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

    async fn run(self, accepted: AcceptedMachineStoragePrepareSubmission) {
        self.clone().run_inner(accepted).await;
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
        let pool = match self
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
            Ok(pool) => pool,
            Err(MachineStoragePrepareError::PreparationFailed {
                machine_id,
                failure,
            }) => {
                self.record_failed(
                    &operation_id,
                    &machine_id,
                    MachineStoragePrepareFailure::PreparationRejected {
                        machine_id: machine_id.clone(),
                        failure,
                    },
                )
                .await;
                return;
            }
            Err(MachineStoragePrepareError::Unavailable { reason, .. }) => {
                match self
                    .recover_prepare_report(&operation_id, &machine_id, reason)
                    .await
                {
                    Ok(pool) => pool,
                    Err(failure) => {
                        self.record_failed(&operation_id, &machine_id, failure)
                            .await;
                        return;
                    }
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

    async fn recover_prepare_report(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        initial_reason: MachineRuntimeUnavailableReason,
    ) -> Result<ployz_core::deploy::ZfsPoolName, MachineStoragePrepareFailure> {
        if !storage_report_retryable(&initial_reason) {
            return Err(MachineStoragePrepareFailure::MachineUnavailable {
                machine_id: machine_id.clone(),
                message: initial_reason.failure_message(),
            });
        }
        // Recovery gets its own full evidence deadline after the long-running
        // prepare RPC has returned without a usable response.
        let deadline = storage_report_deadline(Instant::now());
        loop {
            match self
                .updater
                .report_storage_prepare(machine_id, operation_id)
                .await
            {
                Ok(Some(pool)) => return Ok(pool),
                Ok(None) if Instant::now() < deadline => {}
                Ok(None) => {
                    return Err(MachineStoragePrepareFailure::EvidenceUnavailable {
                        machine_id: machine_id.clone(),
                        message: storage_failure_message(
                            "storage preparation did not produce terminal evidence before the recovery deadline",
                        ),
                    });
                }
                Err(MachineStoragePrepareError::Unavailable { reason, .. })
                    if storage_report_retryable(&reason) && Instant::now() < deadline => {}
                Err(error) => return Err(operation_failure(error)),
            }
            tokio::time::sleep(REPORT_POLL_INTERVAL).await;
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
            failure,
        } => MachineStoragePrepareFailure::PreparationRejected {
            machine_id,
            failure,
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

fn storage_report_deadline(now: Instant) -> Instant {
    now + ployz_core::storage::MACHINE_STORAGE_PREPARE_RPC_TIMEOUT
}

fn storage_report_retryable(reason: &MachineRuntimeUnavailableReason) -> bool {
    match reason {
        MachineRuntimeUnavailableReason::RequestTimedOut
        | MachineRuntimeUnavailableReason::RequestFailed { .. }
        | MachineRuntimeUnavailableReason::ServiceUnavailable { .. }
        | MachineRuntimeUnavailableReason::ServiceTimedOut { .. }
        | MachineRuntimeUnavailableReason::ServiceInternal { .. } => true,
        MachineRuntimeUnavailableReason::EncodeRequest { .. }
        | MachineRuntimeUnavailableReason::NoResponders
        | MachineRuntimeUnavailableReason::InvalidSubject
        | MachineRuntimeUnavailableReason::MaxPayloadExceeded
        | MachineRuntimeUnavailableReason::ServiceBadRequest { .. }
        | MachineRuntimeUnavailableReason::ServiceConflict { .. }
        | MachineRuntimeUnavailableReason::ServiceResponseTooLarge
        | MachineRuntimeUnavailableReason::MalformedServiceError { .. }
        | MachineRuntimeUnavailableReason::DecodeResponse { .. }
        | MachineRuntimeUnavailableReason::WrongResponder { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_recovery_uses_a_fresh_full_deadline() {
        let now = Instant::now();
        assert_eq!(
            storage_report_deadline(now).duration_since(now),
            ployz_core::storage::MACHINE_STORAGE_PREPARE_RPC_TIMEOUT
        );
    }

    #[test]
    fn report_recovery_fails_fast_for_permanent_unavailability() {
        assert!(!storage_report_retryable(
            &MachineRuntimeUnavailableReason::NoResponders
        ));
        assert!(!storage_report_retryable(
            &MachineRuntimeUnavailableReason::InvalidSubject
        ));
        assert!(storage_report_retryable(
            &MachineRuntimeUnavailableReason::RequestTimedOut
        ));
    }
}
