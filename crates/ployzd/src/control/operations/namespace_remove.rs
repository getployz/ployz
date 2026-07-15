//! Operation-owned namespace remove.

use crate::control::intent::namespace_intent::NamespaceIntentStore;
use crate::control::intent::service::NatsIntentReader;
use crate::control::operation_evidence::{
    AcceptedNamespaceRemoveSubmission, RecordOperationEventError,
};
use crate::control::operations::deploy::{MachineContainerRuntime, MachineContainerRuntimeError};
use crate::control::role_client::machine::{
    NatsMachineContainerRuntime, NatsMachineFactsReader, read_available_machine_facts_by_id,
};
use crate::control::sequencer::OperationControllers;
use crate::roles::machine::protocol::MachineContainerRemoveRpcRequest;
use crate::tasks::TaskSpawner;
use ployz_core::ids::{ContainerId, MachineId, NamespaceId, OperationId};
use ployz_core::intent::RouteBindingState;
use ployz_core::intent::ServingTargetEntry;
use ployz_core::machine::runtime::{ManagedContainerIdentity, ManagedContainerKind};
use ployz_core::operation::{
    FailureMessage, NamespaceRemoveFailure, NamespaceRemoveRunningStage, NamespaceRemoveTransition,
    OperatorHint,
};

use ployz_nats::subjects::INTENT_CHANGED;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct NamespaceRemoveOperation {
    client: async_nats::Client,
    namespace_intent: NamespaceIntentStore,
    controllers: OperationControllers,
    step_timeout: Duration,
    task_registry: TaskSpawner,
}

impl NamespaceRemoveOperation {
    #[must_use]
    pub const fn new(
        client: async_nats::Client,
        namespace_intent: NamespaceIntentStore,
        controllers: OperationControllers,
        step_timeout: Duration,
        task_registry: TaskSpawner,
    ) -> Self {
        Self {
            client,
            namespace_intent,
            controllers,
            step_timeout,
            task_registry,
        }
    }

    pub fn start(&self, accepted: AcceptedNamespaceRemoveSubmission) {
        if !accepted.should_start_execution {
            return;
        }

        let operation_id = accepted.operation_id.clone();
        let runtime = self.clone();
        super::report_task_admission(
            &operation_id,
            self.task_registry.spawn(|| async move {
                runtime.run(accepted).await;
            }),
        );
    }

    pub async fn run(self, accepted: AcceptedNamespaceRemoveSubmission) {
        let operation_id = accepted.operation_id.clone();
        let namespace_id = accepted.namespace_id.clone();
        let mut machine_runtime = NatsMachineContainerRuntime::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let facts_reader = NatsMachineFactsReader::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let intent_reader =
            NatsIntentReader::new(self.client.clone()).with_request_timeout(self.step_timeout);

        self.run_inner(
            accepted,
            &intent_reader,
            &facts_reader,
            &mut machine_runtime,
        )
        .await;
        self.controllers
            .release_namespace(&namespace_id, &operation_id)
            .await;
    }

    async fn run_inner(
        &self,
        accepted: AcceptedNamespaceRemoveSubmission,
        intent_reader: &NatsIntentReader,
        facts_reader: &NatsMachineFactsReader,
        machine_runtime: &mut NatsMachineContainerRuntime,
    ) {
        let intent = match intent_reader.intent().await {
            Ok(intent) => intent,
            Err(error) => {
                self.record_failed(
                    &accepted.operation_id,
                    NamespaceRemoveFailure::IntentReadFailed {
                        namespace_id: accepted.namespace_id,
                        message: failure_message(error.to_string()),
                    },
                )
                .await;
                return;
            }
        };
        let active_machines = intent
            .active_machines
            .iter()
            .map(|machine| machine.machine_id.clone())
            .collect::<Vec<_>>();
        let facts_by_machine =
            read_available_machine_facts_by_id(facts_reader, active_machines.clone()).await;
        if let Some(machine_id) = active_machines
            .iter()
            .find(|machine_id| !facts_by_machine.contains_key(*machine_id))
        {
            self.record_failed(
                &accepted.operation_id,
                NamespaceRemoveFailure::MachineUnavailable {
                    machine_id: machine_id.clone(),
                    message: failure_message("machine did not answer runtime fact request"),
                },
            )
            .await;
            return;
        }

        let containers = facts_by_machine
            .values()
            .flat_map(|facts| facts.containers().containers().iter())
            .filter(|container| {
                container.identity.kind == ManagedContainerKind::Service
                    && container.identity.namespace_id == accepted.namespace_id
            })
            .map(|container| RemoveTarget {
                machine_id: container.machine_id.clone(),
                container_id: container.container_id.clone(),
                identity: container.identity.clone(),
            })
            .collect::<Vec<_>>();
        let route_bindings = intent
            .route_bindings
            .into_iter()
            .filter(|binding| binding.namespace_id == accepted.namespace_id)
            .collect::<Vec<_>>();
        let serving_targets = intent
            .serving_target_entries
            .into_iter()
            .filter(|entry| entry.namespace_id == accepted.namespace_id)
            .collect::<Vec<_>>();

        if self
            .record_running(
                &accepted.operation_id,
                NamespaceRemoveRunningStage::RemovingRouteBindings,
            )
            .await
            .is_err()
        {
            return;
        }
        if let Err(message) = self
            .remove_route_bindings(
                &accepted.operation_id,
                &accepted.namespace_id,
                route_bindings,
            )
            .await
        {
            self.record_failed(
                &accepted.operation_id,
                NamespaceRemoveFailure::ControlPlaneCommitFailed {
                    namespace_id: accepted.namespace_id,
                    message: failure_message(message),
                },
            )
            .await;
            return;
        }

        if self
            .record_running(
                &accepted.operation_id,
                NamespaceRemoveRunningStage::RemovingServingTargets,
            )
            .await
            .is_err()
        {
            return;
        }
        if let Err(message) = self
            .remove_serving_targets(&accepted.namespace_id, serving_targets)
            .await
        {
            self.record_failed(
                &accepted.operation_id,
                NamespaceRemoveFailure::ControlPlaneCommitFailed {
                    namespace_id: accepted.namespace_id,
                    message: failure_message(message),
                },
            )
            .await;
            return;
        }

        if self
            .record_running(
                &accepted.operation_id,
                NamespaceRemoveRunningStage::RemovingContainers,
            )
            .await
            .is_err()
        {
            return;
        }
        for target in containers {
            if let Err(error) = machine_runtime
                .remove_container(
                    &target.machine_id,
                    MachineContainerRemoveRpcRequest {
                        operation_id: accepted.operation_id.clone(),
                        container_id: target.container_id.clone(),
                        expected_identity: target.identity.clone(),
                    },
                )
                .await
            {
                self.record_failed(
                    &accepted.operation_id,
                    remove_failure_from_runtime_error(error, &target.container_id),
                )
                .await;
                return;
            }
            let _ = self
                .controllers
                .repository()
                .record_namespace_remove_container(
                    &accepted.operation_id,
                    target.machine_id,
                    target.container_id,
                )
                .await;
        }

        self.record_terminal(&accepted.operation_id, NamespaceRemoveTransition::Completed)
            .await;
    }

    async fn remove_route_bindings(
        &self,
        operation_id: &OperationId,
        namespace_id: &NamespaceId,
        route_bindings: Vec<RouteBindingState>,
    ) -> Result<(), String> {
        for binding in route_bindings {
            let target = binding.target.clone();
            self.namespace_intent
                .remove_route_binding(&target)
                .await
                .map_err(|error| error.to_string())?;
            self.publish_intent_changed().await;
            let _ = self
                .controllers
                .repository()
                .record_namespace_remove_route_binding(operation_id, target)
                .await;
        }
        let _ = namespace_id;
        Ok(())
    }

    async fn remove_serving_targets(
        &self,
        _namespace_id: &NamespaceId,
        serving_targets: Vec<ServingTargetEntry>,
    ) -> Result<(), String> {
        for entry in serving_targets {
            self.namespace_intent
                .remove_serving_target_entry(&entry)
                .await
                .map_err(|error| error.to_string())?;
            self.publish_intent_changed().await;
        }
        Ok(())
    }

    async fn publish_intent_changed(&self) {
        let _ = self.client.publish(INTENT_CHANGED, Vec::new().into()).await;
    }

    async fn record_running(
        &self,
        operation_id: &OperationId,
        stage: NamespaceRemoveRunningStage,
    ) -> Result<(), RecordOperationEventError> {
        self.controllers
            .repository()
            .record_namespace_remove_transition(
                operation_id,
                NamespaceRemoveTransition::Running { stage },
            )
            .await
            .map(|_| ())
    }

    async fn record_failed(&self, operation_id: &OperationId, failure: NamespaceRemoveFailure) {
        self.record_terminal(operation_id, NamespaceRemoveTransition::Failed { failure })
            .await;
    }

    async fn record_terminal(
        &self,
        operation_id: &OperationId,
        transition: NamespaceRemoveTransition,
    ) {
        let _ = self
            .controllers
            .repository()
            .record_namespace_remove_transition(operation_id, transition)
            .await;
    }
}

#[derive(Debug, Clone)]
struct RemoveTarget {
    machine_id: MachineId,
    container_id: ContainerId,
    identity: ManagedContainerIdentity,
}

fn remove_failure_from_runtime_error(
    error: MachineContainerRuntimeError,
    fallback_container_id: &ContainerId,
) -> NamespaceRemoveFailure {
    match error {
        MachineContainerRuntimeError::ImagePullFailed {
            machine_id,
            message,
            ..
        } => NamespaceRemoveFailure::ContainerRemoveFailed {
            machine_id,
            container_id: fallback_container_id.clone(),
            message,
            inspect_hint: inspect_hint(fallback_container_id),
        },
        MachineContainerRuntimeError::Unavailable { machine_id, reason } => {
            NamespaceRemoveFailure::MachineUnavailable {
                machine_id,
                message: reason.failure_message(),
            }
        }
        MachineContainerRuntimeError::RemoveContainerFailed {
            machine_id,
            container_id,
            message,
            inspect_hint,
        } => NamespaceRemoveFailure::ContainerRemoveFailed {
            machine_id,
            container_id,
            message,
            inspect_hint,
        },
        MachineContainerRuntimeError::OperationStepAmbiguous { machine_id, .. } => {
            NamespaceRemoveFailure::ContainerRemoveFailed {
                machine_id,
                container_id: fallback_container_id.clone(),
                message: failure_message(
                    "machine runtime found multiple operation-step containers",
                ),
                inspect_hint: inspect_hint(fallback_container_id),
            }
        }
        MachineContainerRuntimeError::CreatedContainerStartFailed {
            machine_id,
            container_id,
            message,
            inspect_hint,
        }
        | MachineContainerRuntimeError::ExistingContainerStartFailed {
            machine_id,
            container_id,
            message,
            inspect_hint,
        }
        | MachineContainerRuntimeError::OperationStepContainerNotStartable {
            machine_id,
            container_id,
            message,
            inspect_hint,
        }
        | MachineContainerRuntimeError::RestartContainerFailed {
            machine_id,
            container_id,
            message,
            inspect_hint,
        }
        | MachineContainerRuntimeError::StopContainerFailed {
            machine_id,
            container_id,
            message,
            inspect_hint,
        } => NamespaceRemoveFailure::ContainerRemoveFailed {
            machine_id,
            container_id,
            message,
            inspect_hint,
        },
    }
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("generated failure message is non-empty")
}

fn inspect_hint(container_id: &ContainerId) -> OperatorHint {
    OperatorHint::try_new(format!("ployz container inspect {}", container_id.as_str()))
        .expect("generated inspect hint is non-empty")
}
