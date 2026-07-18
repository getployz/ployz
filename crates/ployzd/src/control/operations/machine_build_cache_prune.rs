use crate::control::operation_evidence::{
    AcceptedMachineBuildCachePruneSubmission, RecordOperationEventError,
};
use crate::control::role_client::machine::{
    MachineBuildCachePruneError, NatsMachineSubstrateUpdater,
};
use crate::control::sequencer::OperationControllers;
use crate::tasks::TaskSpawner;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::{
    FailureMessage, MachineBuildCachePruneFailure, MachineBuildCachePruneTransition,
};

#[derive(Debug, Clone)]
pub struct MachineBuildCachePruneOperation {
    controllers: OperationControllers,
    updater: NatsMachineSubstrateUpdater,
    task_registry: TaskSpawner,
}

impl MachineBuildCachePruneOperation {
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

    pub async fn start(&self, accepted: AcceptedMachineBuildCachePruneSubmission) {
        if !accepted.should_start_execution {
            return;
        }
        let operation_id = accepted.operation_id.clone();
        let runtime = self.clone();
        let admission = self
            .task_registry
            .spawn(move || async move { runtime.run(accepted).await });
        super::finish_rejected_task_admission(&self.controllers, &operation_id, admission).await;
    }

    async fn run(self, accepted: AcceptedMachineBuildCachePruneSubmission) {
        let operation_id = accepted.operation_id;
        let machine_id = accepted.machine_id;
        if let Err(error) = self
            .controllers
            .repository()
            .record_machine_build_cache_prune_transition(
                &operation_id,
                &machine_id,
                MachineBuildCachePruneTransition::Pruning,
            )
            .await
        {
            self.record_failed(
                &operation_id,
                &machine_id,
                commit_failure(&machine_id, error),
            )
            .await;
            return;
        }
        let evidence = match self
            .updater
            .prune_build_cache(&machine_id, &operation_id)
            .await
        {
            Ok(evidence) => evidence,
            Err(MachineBuildCachePruneError::Unavailable { machine_id, reason }) => {
                self.record_failed(
                    &operation_id,
                    &machine_id,
                    MachineBuildCachePruneFailure::MachineUnavailable {
                        machine_id: machine_id.clone(),
                        message: reason.failure_message(),
                    },
                )
                .await;
                return;
            }
            Err(MachineBuildCachePruneError::PruneFailed {
                machine_id,
                message,
            }) => {
                self.record_failed(
                    &operation_id,
                    &machine_id,
                    MachineBuildCachePruneFailure::PruneRejected {
                        machine_id: machine_id.clone(),
                        message,
                    },
                )
                .await;
                return;
            }
        };
        if let Err(error) = self
            .controllers
            .repository()
            .record_machine_build_cache_prune_transition(
                &operation_id,
                &machine_id,
                MachineBuildCachePruneTransition::Completed { evidence },
            )
            .await
        {
            self.record_failed(
                &operation_id,
                &machine_id,
                commit_failure(&machine_id, error),
            )
            .await;
        }
    }

    async fn record_failed(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        failure: MachineBuildCachePruneFailure,
    ) {
        let _ = self
            .controllers
            .repository()
            .record_machine_build_cache_prune_transition(
                operation_id,
                machine_id,
                MachineBuildCachePruneTransition::Failed { failure },
            )
            .await;
    }
}

fn commit_failure(
    machine_id: &MachineId,
    error: RecordOperationEventError,
) -> MachineBuildCachePruneFailure {
    let message =
        FailureMessage::try_new(format!("failed to record build cache prune event: {error}"))
            .expect("event failure is non-empty");
    MachineBuildCachePruneFailure::StateCommitFailed {
        machine_id: machine_id.clone(),
        message,
    }
}
