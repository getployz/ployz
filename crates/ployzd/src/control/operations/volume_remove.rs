//! Operation-owned explicit volume destruction.

use crate::control::intent::namespace_intent::NamespaceIntentStore;
use crate::control::intent::service::NatsIntentReader;
use crate::control::operation_evidence::{
    AcceptedVolumeRemoveSubmission, RecordOperationEventError,
};
use crate::control::role_client::machine::{
    NatsMachineContainerRuntime, NatsMachineFactsReader, read_available_machine_facts_by_id,
};
use crate::control::sequencer::OperationControllers;
use crate::tasks::TaskRegistry;
use ployz_core::deploy::VolumeName;
use ployz_core::ids::{NamespaceId, OperationId};
use ployz_core::ops::{
    FailureMessage, VolumeRemoveFailure, VolumeRemoveRunningStage, VolumeRemoveTransition,
};
use ployz_core::state::{IntentSnapshot, VolumePinState};
use ployz_nats::subjects::INTENT_CHANGED;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct VolumeRemoveOperation {
    client: async_nats::Client,
    namespace_intent: NamespaceIntentStore,
    controllers: OperationControllers,
    step_timeout: Duration,
    task_registry: TaskRegistry,
}

impl VolumeRemoveOperation {
    #[must_use]
    pub const fn new(
        client: async_nats::Client,
        namespace_intent: NamespaceIntentStore,
        controllers: OperationControllers,
        step_timeout: Duration,
        task_registry: TaskRegistry,
    ) -> Self {
        Self {
            client,
            namespace_intent,
            controllers,
            step_timeout,
            task_registry,
        }
    }

    pub fn start(&self, accepted: AcceptedVolumeRemoveSubmission) {
        if !accepted.should_start_execution {
            return;
        }
        let runtime = self.clone();
        self.task_registry.spawn(async move {
            runtime.run(accepted).await;
        });
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
        self.run_inner(accepted, &intent_reader, &facts_reader, &machine_runtime)
            .await;
        self.controllers
            .release_namespace(&namespace_id, &operation_id)
            .await;
    }

    async fn run_inner(
        &self,
        accepted: AcceptedVolumeRemoveSubmission,
        intent_reader: &NatsIntentReader,
        facts_reader: &NatsMachineFactsReader,
        machine_runtime: &NatsMachineContainerRuntime,
    ) {
        let intent = match intent_reader.intent().await {
            Ok(intent) => intent,
            Err(error) => {
                self.record_failed(
                    &accepted.operation_id,
                    VolumeRemoveFailure::IntentReadFailed {
                        namespace_id: accepted.namespace_id,
                        volume_name: accepted.volume_name,
                        message: failure_message(error.to_string()),
                    },
                )
                .await;
                return;
            }
        };
        let pin = match removable_volume_pin(&intent, &accepted.namespace_id, &accepted.volume_name)
        {
            Ok(pin) => pin,
            Err(failure) => {
                self.record_failed(&accepted.operation_id, failure).await;
                return;
            }
        };

        if self.record_running(&accepted.operation_id).await.is_err() {
            return;
        }

        let facts =
            read_available_machine_facts_by_id(facts_reader, [pin.machine_id.clone()]).await;
        if !facts.contains_key(&pin.machine_id) {
            self.record_failed(
                &accepted.operation_id,
                VolumeRemoveFailure::MachineUnavailable {
                    machine_id: pin.machine_id,
                    message: failure_message("machine did not answer runtime fact request"),
                },
            )
            .await;
            return;
        }

        if let Err(error) = machine_runtime
            .remove_volume(
                &pin.machine_id,
                accepted.operation_id.clone(),
                &pin.namespace_id,
                &pin.volume_name,
            )
            .await
        {
            self.record_failed(
                &accepted.operation_id,
                VolumeRemoveFailure::VolumeRemoveFailed {
                    machine_id: pin.machine_id,
                    volume: pin.volume_name,
                    message: failure_message(error.to_string()),
                },
            )
            .await;
            return;
        }

        if let Err(error) = self
            .namespace_intent
            .remove_volume_pin(&accepted.namespace_id, &accepted.volume_name)
            .await
        {
            self.record_failed(
                &accepted.operation_id,
                VolumeRemoveFailure::ControlPlaneCommitFailed {
                    namespace_id: accepted.namespace_id,
                    volume_name: accepted.volume_name,
                    message: failure_message(error.to_string()),
                },
            )
            .await;
            return;
        }
        let _ = self.client.publish(INTENT_CHANGED, Vec::new().into()).await;
        self.record_terminal(&accepted.operation_id, VolumeRemoveTransition::Completed)
            .await;
    }

    async fn record_running(
        &self,
        operation_id: &OperationId,
    ) -> Result<(), RecordOperationEventError> {
        self.controllers
            .repository()
            .record_volume_remove_transition(
                operation_id,
                VolumeRemoveTransition::Running {
                    stage: VolumeRemoveRunningStage::RemovingVolumeData,
                },
            )
            .await
            .map(|_| ())
    }

    async fn record_failed(&self, operation_id: &OperationId, failure: VolumeRemoveFailure) {
        self.record_terminal(operation_id, VolumeRemoveTransition::Failed { failure })
            .await;
    }

    async fn record_terminal(
        &self,
        operation_id: &OperationId,
        transition: VolumeRemoveTransition,
    ) {
        let _ = self
            .controllers
            .repository()
            .record_volume_remove_transition(operation_id, transition)
            .await;
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
        .find(|pin| &pin.namespace_id == namespace_id && &pin.volume_name == volume_name)
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
mod tests {
    use super::removable_volume_pin;
    use ployz_core::deploy::VolumeName;
    use ployz_core::ops::VolumeRemoveFailure;
    use ployz_core::state::{ControlPlaneEpoch, IntentSnapshot, VolumePinState};
    use ployz_test_support::fixtures::serving_target_entry_in;
    use ployz_test_support::ids::{machine_id, namespace_id, service_id};

    #[test]
    fn remove_guard_rejects_a_volume_referenced_by_a_serving_target() {
        let namespace_id = namespace_id("team-a");
        let volume_name = VolumeName::try_new("data").expect("valid volume name");
        let mut target = serving_target_entry_in("team-a", "web", "entry_a");
        target.volume_names = vec![volume_name.clone()];
        let intent = IntentSnapshot {
            epoch: ControlPlaneEpoch::initial(),
            core_machine_id: machine_id("core"),
            active_machines: Vec::new(),
            dataplane_projection: ployz_core::dataplane::DataplaneProjection::try_new(
                Vec::new(),
                None,
            )
            .expect("empty projection"),
            route_bindings: Vec::new(),
            serving_target_entries: vec![target],
            volume_pins: vec![VolumePinState {
                namespace_id: namespace_id.clone(),
                volume_name: volume_name.clone(),
                machine_id: machine_id("machine_a"),
            }],
            nats_authorizations: Vec::new(),
            automatic_hostname_configuration:
                ployz_core::ingress::AutomaticHostnameConfiguration::Ployz,
            ployz_dns_target: ployz_core::ingress::PloyzDnsTargetIntent::Enabled,
            active_certificates: Vec::new(),
        };

        assert_eq!(
            removable_volume_pin(&intent, &namespace_id, &volume_name),
            Err(VolumeRemoveFailure::VolumeInUse {
                namespace_id,
                volume_name,
                referencing_services: vec![service_id("web")],
            })
        );
    }
}
