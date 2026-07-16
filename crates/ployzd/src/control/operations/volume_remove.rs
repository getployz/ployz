//! Operation-owned explicit volume destruction.

use crate::control::intent::namespace_intent::NamespaceIntentStore;
use crate::control::intent::service::NatsIntentReader;
use crate::control::operation_evidence::AcceptedVolumeRemoveSubmission;
use crate::control::role_client::machine::{
    MachineVolumeRemoveError, NatsMachineContainerRuntime, NatsMachineFactsReader,
    read_available_machine_facts_by_id,
};
use crate::control::sequencer::OperationControllers;
use crate::tasks::TaskSpawner;
use ployz_core::deploy::VolumeName;
use ployz_core::ids::{NamespaceId, OperationId};
use ployz_core::intent::{IntentSnapshot, VolumePinState};
use ployz_core::operation::{
    FailureMessage, VolumeRemoveFailure, VolumeRemoveRunningStage, VolumeRemoveTransition,
};

use crate::roles::machine::protocol::{MachineVolumeRemoveDomainError, ProvisionedVolumePinState};
use ployz_nats::subjects::INTENT_CHANGED;
use std::future::Future;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct VolumeRemoveOperation {
    client: async_nats::Client,
    namespace_intent: NamespaceIntentStore,
    controllers: OperationControllers,
    step_timeout: Duration,
    task_registry: TaskSpawner,
}

impl VolumeRemoveOperation {
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

    pub async fn start(&self, accepted: AcceptedVolumeRemoveSubmission) {
        if !accepted.should_start_execution {
            return;
        }
        let operation_id = accepted.operation_id.clone();
        let runtime = self.clone();
        super::finish_rejected_task_admission(
            &self.controllers,
            &operation_id,
            self.task_registry.spawn(|| async move {
                runtime.run(accepted).await;
            }),
        )
        .await;
    }

    pub async fn run(self, accepted: AcceptedVolumeRemoveSubmission) {
        let operation_id = accepted.operation_id.clone();
        let namespace_id = accepted.namespace_id.clone();
        let intent_reader =
            NatsIntentReader::new(self.client.clone()).with_request_timeout(self.step_timeout);
        let facts_reader = NatsMachineFactsReader::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let machine_runtime = NatsMachineContainerRuntime::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let runtime = NatsVolumeRemoveRuntime {
            operation: &self,
            intent_reader: &intent_reader,
            facts_reader: &facts_reader,
            machine_runtime: &machine_runtime,
        };
        run_volume_remove(
            &runtime,
            &accepted.operation_id,
            &accepted.namespace_id,
            &accepted.volume_name,
        )
        .await;
        self.controllers
            .release_namespace(&namespace_id, &operation_id)
            .await;
    }
}

trait VolumeRemoveRuntime {
    fn read_intent(&self) -> impl Future<Output = Result<IntentSnapshot, FailureMessage>> + Send;
    fn record_transition(
        &self,
        operation_id: &OperationId,
        transition: VolumeRemoveTransition,
    ) -> impl Future<Output = Result<(), FailureMessage>> + Send;
    fn machine_is_fresh(
        &self,
        machine_id: &ployz_core::ids::MachineId,
    ) -> impl Future<Output = bool> + Send;
    fn remove_volume_reference(
        &self,
        machine_id: &ployz_core::ids::MachineId,
        operation_id: &OperationId,
        pin: &VolumePinState,
    ) -> impl Future<Output = Result<(), MachineVolumeRemoveError>> + Send;
    fn destroy_provisioned_dataset(
        &self,
        machine_id: &ployz_core::ids::MachineId,
        operation_id: &OperationId,
        pin: ProvisionedVolumePinState,
    ) -> impl Future<Output = Result<(), MachineVolumeRemoveError>> + Send;
    fn remove_pin(
        &self,
        namespace_id: &NamespaceId,
        volume_name: &VolumeName,
    ) -> impl Future<Output = Result<(), FailureMessage>> + Send;
    fn publish_intent_changed(&self) -> impl Future<Output = ()> + Send;
}

struct NatsVolumeRemoveRuntime<'a> {
    operation: &'a VolumeRemoveOperation,
    intent_reader: &'a NatsIntentReader,
    facts_reader: &'a NatsMachineFactsReader,
    machine_runtime: &'a NatsMachineContainerRuntime,
}

impl VolumeRemoveRuntime for NatsVolumeRemoveRuntime<'_> {
    async fn read_intent(&self) -> Result<IntentSnapshot, FailureMessage> {
        self.intent_reader
            .intent()
            .await
            .map_err(|error| failure_message(error.to_string()))
    }

    async fn record_transition(
        &self,
        operation_id: &OperationId,
        transition: VolumeRemoveTransition,
    ) -> Result<(), FailureMessage> {
        self.operation
            .controllers
            .repository()
            .record_volume_remove_transition(operation_id, transition)
            .await
            .map(|_| ())
            .map_err(|error| failure_message(error.to_string()))
    }

    async fn machine_is_fresh(&self, machine_id: &ployz_core::ids::MachineId) -> bool {
        read_available_machine_facts_by_id(self.facts_reader, [machine_id.clone()])
            .await
            .contains_key(machine_id)
    }

    async fn remove_volume_reference(
        &self,
        machine_id: &ployz_core::ids::MachineId,
        operation_id: &OperationId,
        pin: &VolumePinState,
    ) -> Result<(), MachineVolumeRemoveError> {
        self.machine_runtime
            .remove_volume_reference(machine_id, operation_id.clone(), pin)
            .await
    }

    async fn destroy_provisioned_dataset(
        &self,
        machine_id: &ployz_core::ids::MachineId,
        operation_id: &OperationId,
        pin: ProvisionedVolumePinState,
    ) -> Result<(), MachineVolumeRemoveError> {
        self.machine_runtime
            .destroy_provisioned_volume_dataset(machine_id, operation_id.clone(), pin)
            .await
    }

    async fn remove_pin(
        &self,
        namespace_id: &NamespaceId,
        volume_name: &VolumeName,
    ) -> Result<(), FailureMessage> {
        self.operation
            .namespace_intent
            .remove_volume_pin(namespace_id, volume_name)
            .await
            .map_err(|error| failure_message(error.to_string()))
    }

    async fn publish_intent_changed(&self) {
        let _ = self
            .operation
            .client
            .publish(INTENT_CHANGED, Vec::new().into())
            .await;
    }
}

async fn run_volume_remove<R: VolumeRemoveRuntime>(
    runtime: &R,
    operation_id: &OperationId,
    namespace_id: &NamespaceId,
    volume_name: &VolumeName,
) {
    let intent = match runtime.read_intent().await {
        Ok(intent) => intent,
        Err(message) => {
            record_failed(
                runtime,
                operation_id,
                VolumeRemoveFailure::IntentReadFailed {
                    namespace_id: namespace_id.clone(),
                    volume_name: volume_name.clone(),
                    message,
                },
            )
            .await;
            return;
        }
    };
    let pin = match removable_volume_pin(&intent, namespace_id, volume_name) {
        Ok(pin) => pin,
        Err(failure) => {
            record_failed(runtime, operation_id, failure).await;
            return;
        }
    };

    if record_running(
        runtime,
        operation_id,
        VolumeRemoveRunningStage::RemovingVolumeData,
    )
    .await
    .is_err()
    {
        return;
    }

    if !runtime.machine_is_fresh(pin.machine_id()).await {
        record_failed(
            runtime,
            operation_id,
            VolumeRemoveFailure::MachineUnavailable {
                machine_id: pin.machine_id().clone(),
                message: failure_message("machine did not answer runtime fact request"),
            },
        )
        .await;
        return;
    }

    if let Err(error) = runtime
        .remove_volume_reference(pin.machine_id(), operation_id, &pin)
        .await
    {
        record_failed(
            runtime,
            operation_id,
            VolumeRemoveFailure::VolumeRemoveFailed {
                machine_id: pin.machine_id().clone(),
                volume: pin.volume_name().clone(),
                message: volume_reference_remove_failure_message(error),
            },
        )
        .await;
        return;
    }

    if let Ok(provisioned_pin) = ProvisionedVolumePinState::try_new(pin.clone()) {
        let dataset = provisioned_pin.dataset().clone();
        if record_running(
            runtime,
            operation_id,
            VolumeRemoveRunningStage::RemovingDataset,
        )
        .await
        .is_err()
        {
            return;
        }
        if let Err(error) = runtime
            .destroy_provisioned_dataset(pin.machine_id(), operation_id, provisioned_pin)
            .await
        {
            record_failed(
                runtime,
                operation_id,
                VolumeRemoveFailure::DatasetDestroyFailed {
                    machine_id: pin.machine_id().clone(),
                    dataset,
                    message: dataset_destroy_failure_message(error),
                },
            )
            .await;
            return;
        }
    }

    if let Err(message) = runtime.remove_pin(namespace_id, volume_name).await {
        record_failed(
            runtime,
            operation_id,
            VolumeRemoveFailure::ControlPlaneCommitFailed {
                namespace_id: namespace_id.clone(),
                volume_name: volume_name.clone(),
                message,
            },
        )
        .await;
        return;
    }
    runtime.publish_intent_changed().await;
    let _ = runtime
        .record_transition(operation_id, VolumeRemoveTransition::Completed)
        .await;
}

async fn record_running<R: VolumeRemoveRuntime>(
    runtime: &R,
    operation_id: &OperationId,
    stage: VolumeRemoveRunningStage,
) -> Result<(), FailureMessage> {
    runtime
        .record_transition(operation_id, VolumeRemoveTransition::Running { stage })
        .await
}

async fn record_failed<R: VolumeRemoveRuntime>(
    runtime: &R,
    operation_id: &OperationId,
    failure: VolumeRemoveFailure,
) {
    let _ = runtime
        .record_transition(operation_id, VolumeRemoveTransition::Failed { failure })
        .await;
}

fn dataset_destroy_failure_message(error: MachineVolumeRemoveError) -> FailureMessage {
    match error {
        MachineVolumeRemoveError::Unavailable { message, .. } => message,
        MachineVolumeRemoveError::Domain {
            error: MachineVolumeRemoveDomainError::DatasetDestroyFailed { failure, .. },
            ..
        } => failure_message(failure.to_string()),
        MachineVolumeRemoveError::Domain {
            error: MachineVolumeRemoveDomainError::DockerRemoveFailed { message },
            ..
        } => message,
        MachineVolumeRemoveError::Domain {
            error:
                MachineVolumeRemoveDomainError::MachineMismatch {
                    expected_machine_id,
                    responder_machine_id,
                },
            ..
        } => failure_message(format!(
            "dataset destroy reached machine {} for pin owned by {}",
            responder_machine_id.as_str(),
            expected_machine_id.as_str()
        )),
    }
}

fn volume_reference_remove_failure_message(error: MachineVolumeRemoveError) -> FailureMessage {
    match error {
        MachineVolumeRemoveError::Unavailable { message, .. } => message,
        MachineVolumeRemoveError::Domain {
            error: MachineVolumeRemoveDomainError::DockerRemoveFailed { message },
            ..
        } => message,
        MachineVolumeRemoveError::Domain {
            error:
                MachineVolumeRemoveDomainError::MachineMismatch {
                    expected_machine_id,
                    responder_machine_id,
                },
            ..
        } => failure_message(format!(
            "volume remove reached machine {} for pin owned by {}",
            responder_machine_id.as_str(),
            expected_machine_id.as_str()
        )),
        MachineVolumeRemoveError::Domain {
            error: MachineVolumeRemoveDomainError::DatasetDestroyFailed { dataset, failure },
            ..
        } => failure_message(format!(
            "machine reported dataset destroy failure for {} while removing the Docker volume reference: {failure}",
            dataset.as_str()
        )),
    }
}

fn removable_volume_pin(
    intent: &IntentSnapshot,
    namespace_id: &NamespaceId,
    volume_name: &VolumeName,
) -> Result<VolumePinState, VolumeRemoveFailure> {
    let Some(pin) = intent
        .volume_pins
        .iter()
        .find(|pin| pin.namespace_id() == namespace_id && pin.volume_name() == volume_name)
    else {
        return Err(VolumeRemoveFailure::VolumeNotFound {
            namespace_id: namespace_id.clone(),
            volume_name: volume_name.clone(),
        });
    };
    let referencing_services = intent.services_referencing_volume(namespace_id, volume_name);
    if !referencing_services.is_empty() {
        return Err(VolumeRemoveFailure::VolumeInUse {
            namespace_id: namespace_id.clone(),
            volume_name: volume_name.clone(),
            referencing_services,
        });
    }
    Ok(pin.clone())
}

fn failure_message(value: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(value).expect("generated failure message is non-empty")
}

#[cfg(test)]
mod tests;
