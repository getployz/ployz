//! Operation-owned service restart.

use crate::intent::service::NatsIntentReader;
use crate::operation_api::admission::OperationControllers;
use crate::operations::deploy::{MachineContainerRuntime, MachineContainerRuntimeError};
use crate::operations::log::{
    AcceptedServiceRestartSubmission, OperationStatusWrite, RecordOperationEventError,
};
use crate::roles::machine::client::{
    MachineContainerInspectError, NatsMachineContainerRuntime, NatsMachineFactsReader,
    read_available_machine_facts_by_id,
};
use crate::roles::machine::protocol::{
    MachineContainerInspectRpcRequest, MachineContainerRestartRpcRequest,
};
use crate::tasks::TaskRegistry;
use ployz_core::ids::{ContainerId, MachineId, OperationId};
use ployz_core::machine_runtime::{
    ContainerHealth, ContainerRuntimeState, ManagedContainerIdentity,
};
use ployz_core::ops::{
    FailureMessage, OperatorHint, ServiceRestartFailure, ServiceRestartRunningStage,
    ServiceRestartTransition, StatusProjectionError,
};
use ployz_core::state::IntentSnapshot;
use std::time::{Duration, Instant};

const RESTART_HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct ServiceRestartOperation {
    client: async_nats::Client,
    controllers: OperationControllers,
    step_timeout: Duration,
    task_registry: TaskRegistry,
}

impl ServiceRestartOperation {
    #[must_use]
    pub const fn new(
        client: async_nats::Client,
        controllers: OperationControllers,
        step_timeout: Duration,
        task_registry: TaskRegistry,
    ) -> Self {
        Self {
            client,
            controllers,
            step_timeout,
            task_registry,
        }
    }

    pub fn start(&self, accepted: AcceptedServiceRestartSubmission) {
        if !accepted.should_start_execution {
            return;
        }

        let runtime = self.clone();
        self.task_registry.spawn(async move {
            runtime.run(accepted).await;
        });
    }

    pub async fn run(self, accepted: AcceptedServiceRestartSubmission) {
        let namespace_id = accepted.namespace_id.clone();
        let operation_id = accepted.operation_id.clone();
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
        accepted: AcceptedServiceRestartSubmission,
        intent_reader: &NatsIntentReader,
        facts_reader: &NatsMachineFactsReader,
        machine_runtime: &mut NatsMachineContainerRuntime,
    ) {
        if self
            .record_running(
                &accepted.operation_id,
                ServiceRestartRunningStage::RestartingContainers,
            )
            .await
            .is_err()
        {
            return;
        }

        let intent = match intent_reader.intent().await {
            Ok(intent) => intent,
            Err(error) => {
                self.record_failed(
                    &accepted.operation_id,
                    ServiceRestartFailure::IntentReadFailed {
                        namespace_id: accepted.namespace_id,
                        service_id: accepted.service_id,
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
                ServiceRestartFailure::MachineUnavailable {
                    machine_id: machine_id.clone(),
                    message: failure_message("machine did not answer runtime fact request"),
                },
            )
            .await;
            return;
        }

        let targets = match restart_targets(&intent, facts_by_machine.values(), &accepted) {
            RestartTargetSelection::NoSuchService => {
                self.record_failed(
                    &accepted.operation_id,
                    ServiceRestartFailure::NoSuchService {
                        namespace_id: accepted.namespace_id,
                        service_id: accepted.service_id,
                    },
                )
                .await;
                return;
            }
            RestartTargetSelection::NoRunningContainers => {
                self.record_failed(
                    &accepted.operation_id,
                    ServiceRestartFailure::NoRunningContainers {
                        namespace_id: accepted.namespace_id,
                        service_id: accepted.service_id,
                    },
                )
                .await;
                return;
            }
            RestartTargetSelection::Targets(targets) => targets,
        };

        for target in targets {
            if self
                .record_running(
                    &accepted.operation_id,
                    ServiceRestartRunningStage::RestartingContainers,
                )
                .await
                .is_err()
            {
                return;
            }
            if let Err(error) = machine_runtime
                .restart_container(
                    &target.machine_id,
                    MachineContainerRestartRpcRequest {
                        operation_id: accepted.operation_id.clone(),
                        container_id: target.container_id.clone(),
                        expected_identity: target.identity.clone(),
                    },
                )
                .await
            {
                self.record_failed(
                    &accepted.operation_id,
                    restart_failure_from_runtime_error(error, &target.container_id),
                )
                .await;
                return;
            }
            let _ = self
                .controllers
                .repository()
                .record_service_restart_container(
                    &accepted.operation_id,
                    target.machine_id.clone(),
                    target.container_id.clone(),
                )
                .await;
            if self
                .record_running(
                    &accepted.operation_id,
                    ServiceRestartRunningStage::WaitingForHealth,
                )
                .await
                .is_err()
            {
                return;
            }
            if let Err(failure) = self
                .wait_running(&accepted.operation_id, machine_runtime, &target)
                .await
            {
                self.record_failed(&accepted.operation_id, failure).await;
                return;
            }
        }

        self.record_terminal(&accepted.operation_id, ServiceRestartTransition::Completed)
            .await;
    }

    async fn wait_running(
        &self,
        operation_id: &OperationId,
        machine_runtime: &mut NatsMachineContainerRuntime,
        target: &RestartTarget,
    ) -> Result<(), ServiceRestartFailure> {
        let deadline = Instant::now() + self.step_timeout;
        loop {
            let inspection = machine_runtime
                .inspect_container(
                    &target.machine_id,
                    MachineContainerInspectRpcRequest {
                        container_id: target.container_id.clone(),
                    },
                )
                .await
                .map_err(|error| health_failure_from_inspect_error(error, &target.container_id))?;
            if let Some(observation) = inspection.observation {
                match restart_health_observation(operation_id, target, observation.state)? {
                    RestartHealthObservation::Ready => return Ok(()),
                    RestartHealthObservation::Waiting => {}
                }
            }
            if Instant::now() >= deadline {
                return Err(ServiceRestartFailure::Timeout {
                    machine_id: target.machine_id.clone(),
                    container_id: target.container_id.clone(),
                    timeout_seconds: timeout_seconds(self.step_timeout),
                });
            }
            tokio::time::sleep(RESTART_HEALTH_POLL_INTERVAL).await;
        }
    }

    async fn record_running(
        &self,
        operation_id: &OperationId,
        stage: ServiceRestartRunningStage,
    ) -> Result<(), RecordOperationEventError> {
        match self
            .controllers
            .repository()
            .record_service_restart_transition(
                operation_id,
                ServiceRestartTransition::Running { stage },
            )
            .await
        {
            Ok(OperationStatusWrite::Stored)
            | Ok(OperationStatusWrite::AlreadySatisfied {
                current_sequence: _,
            }) => Ok(()),
            Err(error @ RecordOperationEventError::StoreStatus(_)) => Err(error),
            Err(error @ RecordOperationEventError::MissingOperation { .. }) => Err(error),
            Err(error @ RecordOperationEventError::InvalidNextSequence(_)) => Err(error),
            Err(
                error @ RecordOperationEventError::ProjectStatus(
                    StatusProjectionError::MissingOperation { .. },
                ),
            ) => Err(error),
            Err(
                error @ RecordOperationEventError::ProjectStatus(
                    StatusProjectionError::OperationKindMismatch { .. },
                ),
            ) => Err(error),
            Err(
                error @ RecordOperationEventError::ProjectStatus(
                    StatusProjectionError::OperationSubjectMismatch { .. },
                ),
            ) => Err(error),
            Err(
                error @ RecordOperationEventError::ProjectStatus(
                    StatusProjectionError::OperationEventMismatch { .. },
                ),
            ) => Err(error),
            Err(
                error @ RecordOperationEventError::ProjectStatus(
                    StatusProjectionError::TerminalState { .. },
                ),
            ) => Err(error),
            Err(
                error @ RecordOperationEventError::ProjectStatus(
                    StatusProjectionError::InvalidTransition { .. },
                ),
            ) => Err(error),
            Err(
                error @ RecordOperationEventError::ProjectStatus(
                    StatusProjectionError::ManagedLeaseCancellationUnsupported { .. },
                ),
            ) => Err(error),
        }
    }

    async fn record_failed(&self, operation_id: &OperationId, failure: ServiceRestartFailure) {
        self.record_terminal(operation_id, ServiceRestartTransition::Failed { failure })
            .await;
    }

    async fn record_terminal(
        &self,
        operation_id: &OperationId,
        transition: ServiceRestartTransition,
    ) {
        let _ = self
            .controllers
            .repository()
            .record_service_restart_transition(operation_id, transition)
            .await;
    }
}

#[derive(Debug, Clone)]
struct RestartTarget {
    machine_id: MachineId,
    container_id: ContainerId,
    identity: ManagedContainerIdentity,
}

enum RestartTargetSelection {
    NoSuchService,
    NoRunningContainers,
    Targets(Vec<RestartTarget>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestartHealthObservation {
    Ready,
    Waiting,
}

fn restart_health_observation(
    operation_id: &OperationId,
    target: &RestartTarget,
    state: ContainerRuntimeState,
) -> Result<RestartHealthObservation, ServiceRestartFailure> {
    match state {
        ContainerRuntimeState::Running {
            health: ContainerHealth::None | ContainerHealth::Healthy,
            ..
        } => Ok(RestartHealthObservation::Ready),
        ContainerRuntimeState::Running {
            health: ContainerHealth::Starting,
            ..
        } => Ok(RestartHealthObservation::Waiting),
        ContainerRuntimeState::Running {
            health: ContainerHealth::Unhealthy,
            ..
        } => Err(ServiceRestartFailure::HealthGateFailed {
            machine_id: target.machine_id.clone(),
            container_id: target.container_id.clone(),
            message: failure_message("container healthcheck reported unhealthy"),
            log_hint: log_hint(operation_id, &target.container_id),
        }),
        ContainerRuntimeState::Exited => Err(ServiceRestartFailure::HealthGateFailed {
            machine_id: target.machine_id.clone(),
            container_id: target.container_id.clone(),
            message: failure_message("container exited after restart"),
            log_hint: log_hint(operation_id, &target.container_id),
        }),
    }
}

fn restart_targets<'a>(
    intent: &IntentSnapshot,
    facts: impl IntoIterator<Item = &'a ployz_core::machine_runtime::MachineFactsSnapshot>,
    accepted: &AcceptedServiceRestartSubmission,
) -> RestartTargetSelection {
    let service_exists = intent.serving_target_entries.iter().any(|entry| {
        entry.namespace_id == accepted.namespace_id && entry.service_id == accepted.service_id
    });
    if !service_exists {
        return RestartTargetSelection::NoSuchService;
    }

    let mut targets = facts
        .into_iter()
        .flat_map(|snapshot| snapshot.containers().containers().iter())
        .filter(|container| {
            container.is_running_service()
                && container.identity.namespace_id == accepted.namespace_id
                && container.identity.service_id == accepted.service_id
        })
        .map(|container| RestartTarget {
            machine_id: container.machine_id.clone(),
            container_id: container.container_id.clone(),
            identity: container.identity.clone(),
        })
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| {
        left.machine_id
            .cmp(&right.machine_id)
            .then(left.container_id.cmp(&right.container_id))
    });
    if targets.is_empty() {
        RestartTargetSelection::NoRunningContainers
    } else {
        RestartTargetSelection::Targets(targets)
    }
}

fn restart_failure_from_runtime_error(
    error: MachineContainerRuntimeError,
    fallback_container_id: &ContainerId,
) -> ServiceRestartFailure {
    match error {
        MachineContainerRuntimeError::ImagePullFailed {
            machine_id,
            message,
            ..
        } => ServiceRestartFailure::ContainerRestartFailed {
            machine_id,
            container_id: fallback_container_id.clone(),
            message,
            inspect_hint: inspect_hint(fallback_container_id),
        },
        MachineContainerRuntimeError::Unavailable { machine_id, reason } => {
            ServiceRestartFailure::MachineUnavailable {
                machine_id,
                message: reason.failure_message(),
            }
        }
        MachineContainerRuntimeError::RestartContainerFailed {
            machine_id,
            container_id,
            message,
            inspect_hint,
        } => ServiceRestartFailure::ContainerRestartFailed {
            machine_id,
            container_id,
            message,
            inspect_hint,
        },
        MachineContainerRuntimeError::OperationStepAmbiguous { machine_id, .. } => {
            ServiceRestartFailure::ContainerRestartFailed {
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
        | MachineContainerRuntimeError::RemoveContainerFailed {
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
        } => ServiceRestartFailure::ContainerRestartFailed {
            machine_id,
            container_id,
            message,
            inspect_hint,
        },
    }
}

fn health_failure_from_inspect_error(
    error: MachineContainerInspectError,
    container_id: &ContainerId,
) -> ServiceRestartFailure {
    match error {
        MachineContainerInspectError::Unavailable { machine_id, reason } => {
            ServiceRestartFailure::MachineUnavailable {
                machine_id,
                message: reason.failure_message(),
            }
        }
        MachineContainerInspectError::InspectFailed {
            machine_id,
            message,
            ..
        } => ServiceRestartFailure::HealthGateFailed {
            machine_id,
            container_id: container_id.clone(),
            message,
            log_hint: inspect_hint(container_id),
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

fn log_hint(operation_id: &OperationId, container_id: &ContainerId) -> OperatorHint {
    OperatorHint::try_new(format!(
        "ployzctl ops status {}; ployzctl logs {}",
        operation_id.as_str(),
        container_id.as_str()
    ))
    .expect("generated log hint is non-empty")
}

fn timeout_seconds(timeout: Duration) -> u32 {
    timeout.as_secs().min(u64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::{NamespaceId, NamespaceRevisionEntryId, ServiceId, StepId};
    use ployz_core::machine_runtime::ManagedContainerKind;

    #[test]
    fn restart_health_observation_accepts_running_without_healthcheck() {
        let outcome = restart_health_observation(
            &operation_id(),
            &target(),
            ContainerRuntimeState::running_unroutable(),
        );

        assert_eq!(outcome, Ok(RestartHealthObservation::Ready));
    }

    #[test]
    fn restart_health_observation_waits_for_starting_healthcheck() {
        let outcome = restart_health_observation(
            &operation_id(),
            &target(),
            ContainerRuntimeState::running_at_with_health(None, ContainerHealth::Starting),
        );

        assert_eq!(outcome, Ok(RestartHealthObservation::Waiting));
    }

    #[test]
    fn restart_health_observation_accepts_healthy_healthcheck() {
        let outcome = restart_health_observation(
            &operation_id(),
            &target(),
            ContainerRuntimeState::running_at_with_health(None, ContainerHealth::Healthy),
        );

        assert_eq!(outcome, Ok(RestartHealthObservation::Ready));
    }

    #[test]
    fn restart_health_observation_rejects_unhealthy_healthcheck() {
        let outcome = restart_health_observation(
            &operation_id(),
            &target(),
            ContainerRuntimeState::running_at_with_health(None, ContainerHealth::Unhealthy),
        );

        let Err(ServiceRestartFailure::HealthGateFailed { message, .. }) = outcome else {
            panic!("expected health gate failure");
        };
        assert_eq!(
            message,
            failure_message("container healthcheck reported unhealthy")
        );
    }

    #[test]
    fn restart_health_observation_rejects_exited_container() {
        let outcome =
            restart_health_observation(&operation_id(), &target(), ContainerRuntimeState::Exited);

        let Err(ServiceRestartFailure::HealthGateFailed { message, .. }) = outcome else {
            panic!("expected health gate failure");
        };
        assert_eq!(message, failure_message("container exited after restart"));
    }

    fn target() -> RestartTarget {
        RestartTarget {
            machine_id: MachineId::try_new("machine-a").expect("valid machine id"),
            container_id: ContainerId::try_new("container-a").expect("valid container id"),
            identity: ManagedContainerIdentity {
                namespace_id: NamespaceId::try_new("default").expect("valid namespace id"),
                service_id: ServiceId::try_new("web").expect("valid service id"),
                namespace_revision_entry_id: NamespaceRevisionEntryId::try_new("entry-a")
                    .expect("valid namespace revision entry id"),
                operation_id: operation_id(),
                step_id: StepId::try_new("step-a").expect("valid step id"),
                kind: ManagedContainerKind::Service,
            },
        }
    }

    fn operation_id() -> OperationId {
        OperationId::try_new("op-test").expect("valid operation id")
    }
}
