//! Runtime for operation-owned machine lifecycle changes (drain/resume).
//!
//! Lifecycle is operator intent about a machine (which may be unreachable), so
//! it is control-side durable authority, committed to the machine's roster row
//! in the core database.

use crate::controllers::OperationControllers;
use crate::machine_roster::{MachineLifecycleUpdate, MachineRosterStore};
use crate::operations::log::AcceptedMachineLifecycleSubmission;
use crate::tasks::TaskRegistry;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::ops::{FailureMessage, MachineLifecycleFailure, MachineLifecycleTransition};
use ployz_core::subjects::INTENT_CHANGED;

#[derive(Debug, Clone)]
pub struct MachineLifecycleOperationRuntime {
    intent_change_client: async_nats::Client,
    controllers: OperationControllers,
    machine_roster: MachineRosterStore,
    task_registry: TaskRegistry,
}

impl MachineLifecycleOperationRuntime {
    #[must_use]
    pub fn new(
        intent_change_client: async_nats::Client,
        controllers: OperationControllers,
        machine_roster: MachineRosterStore,
        task_registry: TaskRegistry,
    ) -> Self {
        Self {
            intent_change_client,
            controllers,
            machine_roster,
            task_registry,
        }
    }

    pub fn start(&self, accepted: AcceptedMachineLifecycleSubmission) {
        if !accepted.should_start_execution {
            return;
        }

        let runtime = self.clone();
        self.task_registry.spawn(async move {
            runtime.run(accepted).await;
        });
    }

    pub async fn run(self, accepted: AcceptedMachineLifecycleSubmission) {
        let operation_id = accepted.operation_id;
        let machine_id = accepted.machine_id;
        let target = accepted.target;

        match self.machine_roster.set_lifecycle(&machine_id, target).await {
            Ok(MachineLifecycleUpdate::NoSuchMachine) => {
                self.record_terminal(
                    &operation_id,
                    &machine_id,
                    MachineLifecycleTransition::Failed {
                        failure: MachineLifecycleFailure::NoSuchMachine {
                            machine_id: machine_id.clone(),
                        },
                    },
                )
                .await;
            }
            Ok(MachineLifecycleUpdate::Changed) => {
                // Fanout only: readers re-issue intent.get; a missed poke is
                // repaired by the drumbeat rebroadcast.
                let _ = self
                    .intent_change_client
                    .publish(INTENT_CHANGED, Vec::new().into())
                    .await;
                self.record_terminal(
                    &operation_id,
                    &machine_id,
                    MachineLifecycleTransition::Completed,
                )
                .await;
            }
            Ok(MachineLifecycleUpdate::Unchanged) => {
                self.record_terminal(
                    &operation_id,
                    &machine_id,
                    MachineLifecycleTransition::Completed,
                )
                .await;
            }
            Err(error) => {
                self.record_state_commit_failed(
                    &operation_id,
                    &machine_id,
                    &format!("failed to commit machine lifecycle: {error}"),
                )
                .await;
            }
        }
    }

    async fn record_state_commit_failed(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        message: &str,
    ) {
        self.record_terminal(
            operation_id,
            machine_id,
            MachineLifecycleTransition::Failed {
                failure: MachineLifecycleFailure::StateCommitFailed {
                    message: FailureMessage::try_new(message.to_owned())
                        .expect("state commit failure message is non-empty"),
                },
            },
        )
        .await;
    }

    async fn record_terminal(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        transition: MachineLifecycleTransition,
    ) {
        if let Err(error) = self
            .controllers
            .repository()
            .record_machine_lifecycle_transition(operation_id, machine_id, transition)
            .await
        {
            eprintln!("failed to record machine-lifecycle terminal event: {error}");
        }
    }
}
