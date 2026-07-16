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
use ployz_core::intent::{IntentSnapshot, VolumeKind, VolumePinState};
use ployz_core::operation::{
    FailureMessage, VolumeRemoveFailure, VolumeRemoveRunningStage, VolumeRemoveTransition,
};

use crate::roles::machine::protocol::{MachineVolumeRemoveDomainError, MachineVolumeRemoveEffect};
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
    fn remove_volume(
        &self,
        machine_id: &ployz_core::ids::MachineId,
        operation_id: &OperationId,
        pin: &VolumePinState,
        effect: MachineVolumeRemoveEffect,
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

    async fn remove_volume(
        &self,
        machine_id: &ployz_core::ids::MachineId,
        operation_id: &OperationId,
        pin: &VolumePinState,
        effect: MachineVolumeRemoveEffect,
    ) -> Result<(), MachineVolumeRemoveError> {
        self.machine_runtime
            .remove_volume(machine_id, operation_id.clone(), pin, effect)
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
        .remove_volume(
            pin.machine_id(),
            operation_id,
            &pin,
            MachineVolumeRemoveEffect::DockerReference,
        )
        .await
    {
        record_failed(
            runtime,
            operation_id,
            VolumeRemoveFailure::VolumeRemoveFailed {
                machine_id: pin.machine_id().clone(),
                volume: pin.volume_name().clone(),
                message: failure_message(error.to_string()),
            },
        )
        .await;
        return;
    }

    if let VolumeKind::Provisioned { dataset, .. } = pin.kind() {
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
            .remove_volume(
                pin.machine_id(),
                operation_id,
                &pin,
                MachineVolumeRemoveEffect::ProvisionedDataset,
            )
            .await
        {
            record_failed(
                runtime,
                operation_id,
                VolumeRemoveFailure::DatasetDestroyFailed {
                    machine_id: pin.machine_id().clone(),
                    dataset: dataset.clone(),
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
        MachineVolumeRemoveError::Domain { error, .. } => {
            failure_message(format!("machine rejected dataset destroy: {error:?}"))
        }
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
mod tests {
    use super::*;
    use ployz_core::deploy::{DatasetName, VolumeMaxSizeBytes, VolumeName, ZfsPoolName};
    use ployz_core::intent::recovery::ControlPlaneEpoch;
    use ployz_core::intent::{IntentSnapshot, VolumeKind, VolumePinState};
    use ployz_core::operation::{FailureMessage, VolumeRemoveFailure};
    use ployz_core::storage::StorageEffectFailure;

    use ployz_test_support::fixtures::serving_target_entry_in;
    use ployz_test_support::ids::{machine_id, namespace_id, operation_id, service_id};
    use std::sync::Mutex;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct EffectCall {
        effect: MachineVolumeRemoveEffect,
        pin: VolumePinState,
    }

    struct FakeRuntime {
        state: Mutex<FakeState>,
    }

    struct FakeState {
        intent: IntentSnapshot,
        fresh: bool,
        failed_stage_record: Option<VolumeRemoveRunningStage>,
        docker_result: Result<(), MachineVolumeRemoveError>,
        dataset_result: Result<(), MachineVolumeRemoveError>,
        commit_result: Result<(), FailureMessage>,
        transitions: Vec<VolumeRemoveTransition>,
        transition_operation_ids: Vec<OperationId>,
        effects: Vec<EffectCall>,
        pin_present: bool,
        published: usize,
    }

    impl FakeRuntime {
        fn new(pin: VolumePinState) -> Self {
            Self {
                state: Mutex::new(FakeState {
                    intent: intent_with_pin(pin),
                    fresh: true,
                    failed_stage_record: None,
                    docker_result: Ok(()),
                    dataset_result: Ok(()),
                    commit_result: Ok(()),
                    transitions: Vec::new(),
                    transition_operation_ids: Vec::new(),
                    effects: Vec::new(),
                    pin_present: true,
                    published: 0,
                }),
            }
        }

        async fn run(&self) {
            self.run_for("op_remove").await;
        }

        async fn run_for(&self, operation: &str) {
            run_volume_remove(
                self,
                &operation_id(operation),
                &namespace_id("team-a"),
                &volume_name(),
            )
            .await;
        }
    }

    impl VolumeRemoveRuntime for FakeRuntime {
        async fn read_intent(&self) -> Result<IntentSnapshot, FailureMessage> {
            Ok(self.state.lock().unwrap().intent.clone())
        }

        async fn record_transition(
            &self,
            operation_id: &OperationId,
            transition: VolumeRemoveTransition,
        ) -> Result<(), FailureMessage> {
            let mut state = self.state.lock().unwrap();
            if let VolumeRemoveTransition::Running { stage } = transition
                && state.failed_stage_record == Some(stage)
            {
                return Err(failure_message("stage record failed"));
            }
            state.transition_operation_ids.push(operation_id.clone());
            state.transitions.push(transition);
            Ok(())
        }

        async fn machine_is_fresh(&self, _machine_id: &ployz_core::ids::MachineId) -> bool {
            self.state.lock().unwrap().fresh
        }

        async fn remove_volume(
            &self,
            _machine_id: &ployz_core::ids::MachineId,
            _operation_id: &OperationId,
            pin: &VolumePinState,
            effect: MachineVolumeRemoveEffect,
        ) -> Result<(), MachineVolumeRemoveError> {
            let mut state = self.state.lock().unwrap();
            state.effects.push(EffectCall {
                effect,
                pin: pin.clone(),
            });
            match effect {
                MachineVolumeRemoveEffect::DockerReference => state.docker_result.clone(),
                MachineVolumeRemoveEffect::ProvisionedDataset => state.dataset_result.clone(),
            }
        }

        async fn remove_pin(
            &self,
            _namespace_id: &NamespaceId,
            _volume_name: &VolumeName,
        ) -> Result<(), FailureMessage> {
            let mut state = self.state.lock().unwrap();
            state.commit_result.clone()?;
            state.pin_present = false;
            Ok(())
        }

        async fn publish_intent_changed(&self) {
            self.state.lock().unwrap().published += 1;
        }
    }

    fn volume_name() -> VolumeName {
        VolumeName::try_new("data").expect("valid volume")
    }

    fn plain_pin() -> VolumePinState {
        VolumePinState::plain(
            namespace_id("team-a"),
            volume_name(),
            machine_id("machine_a"),
        )
    }

    fn provisioned_pin() -> VolumePinState {
        let namespace_id = namespace_id("team-a");
        let volume_name = volume_name();
        VolumePinState::try_new(
            namespace_id.clone(),
            volume_name.clone(),
            machine_id("machine_a"),
            VolumeKind::Provisioned {
                dataset: DatasetName::for_volume(
                    &ZfsPoolName::try_new("stored-pool").expect("valid pool"),
                    &namespace_id,
                    &volume_name,
                )
                .expect("valid dataset"),
                max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("valid quota"),
            },
        )
        .expect("valid pin")
    }

    fn intent_with_pin(pin: VolumePinState) -> IntentSnapshot {
        IntentSnapshot {
            epoch: ControlPlaneEpoch::initial(),
            core_machine_id: machine_id("core"),
            active_machines: Vec::new(),
            dataplane_projection: ployz_core::network::DataplaneProjection::try_new(
                Vec::new(),
                None,
            )
            .expect("empty projection"),
            route_bindings: Vec::new(),
            serving_target_entries: Vec::new(),
            volume_pins: vec![pin],
            nats_authorizations: Vec::new(),
            automatic_hostname_configuration:
                ployz_core::ingress::AutomaticHostnameConfiguration::Ployz,
            ployz_dns_target: ployz_core::ingress::PloyzDnsTargetIntent::Enabled,
            active_certificates: Vec::new(),
        }
    }

    fn failed_transition(state: &FakeState) -> &VolumeRemoveFailure {
        let Some(VolumeRemoveTransition::Failed { failure }) = state.transitions.last() else {
            panic!("expected terminal failure transition");
        };
        failure
    }

    #[tokio::test]
    async fn provisioned_remove_records_ordered_stages_and_removes_pin_after_both_effects() {
        let pin = provisioned_pin();
        let runtime = FakeRuntime::new(pin.clone());

        runtime.run().await;

        let state = runtime.state.lock().unwrap();
        assert_eq!(
            state.effects,
            [
                EffectCall {
                    effect: MachineVolumeRemoveEffect::DockerReference,
                    pin: pin.clone(),
                },
                EffectCall {
                    effect: MachineVolumeRemoveEffect::ProvisionedDataset,
                    pin,
                },
            ]
        );
        assert_eq!(
            state.transitions,
            [
                VolumeRemoveTransition::Running {
                    stage: VolumeRemoveRunningStage::RemovingVolumeData,
                },
                VolumeRemoveTransition::Running {
                    stage: VolumeRemoveRunningStage::RemovingDataset,
                },
                VolumeRemoveTransition::Completed,
            ]
        );
        assert!(!state.pin_present);
        assert_eq!(state.published, 1);
    }

    #[tokio::test]
    async fn plain_remove_never_requests_dataset_destruction() {
        let runtime = FakeRuntime::new(plain_pin());

        runtime.run().await;

        let state = runtime.state.lock().unwrap();
        assert_eq!(state.effects.len(), 1);
        assert_eq!(
            state.effects[0].effect,
            MachineVolumeRemoveEffect::DockerReference
        );
        assert!(!state.pin_present);
        assert_eq!(
            state.transitions.last(),
            Some(&VolumeRemoveTransition::Completed)
        );
    }

    #[tokio::test]
    async fn docker_failure_retains_pin_and_never_requests_dataset() {
        let runtime = FakeRuntime::new(provisioned_pin());
        runtime.state.lock().unwrap().docker_result = Err(MachineVolumeRemoveError::Domain {
            machine_id: machine_id("machine_a"),
            error: MachineVolumeRemoveDomainError::DockerRemoveFailed {
                message: failure_message("volume is in use"),
            },
        });

        runtime.run().await;

        let state = runtime.state.lock().unwrap();
        assert_eq!(state.effects.len(), 1);
        assert!(state.pin_present);
        assert!(matches!(
            failed_transition(&state),
            VolumeRemoveFailure::VolumeRemoveFailed { .. }
        ));
    }

    #[tokio::test]
    async fn dataset_stage_record_failure_stops_before_dataset_effect() {
        let runtime = FakeRuntime::new(provisioned_pin());
        runtime.state.lock().unwrap().failed_stage_record =
            Some(VolumeRemoveRunningStage::RemovingDataset);

        runtime.run().await;

        let state = runtime.state.lock().unwrap();
        assert_eq!(state.effects.len(), 1);
        assert!(state.pin_present);
        assert_eq!(
            state.transitions,
            [VolumeRemoveTransition::Running {
                stage: VolumeRemoveRunningStage::RemovingVolumeData,
            }]
        );
    }

    #[tokio::test]
    async fn dataset_failure_preserves_exact_dataset_evidence_and_pin() {
        let pin = provisioned_pin();
        let VolumeKind::Provisioned { dataset, .. } = pin.kind() else {
            panic!("provisioned pin expected");
        };
        let dataset = dataset.clone();
        let runtime = FakeRuntime::new(pin);
        runtime.state.lock().unwrap().dataset_result = Err(MachineVolumeRemoveError::Domain {
            machine_id: machine_id("machine_a"),
            error: MachineVolumeRemoveDomainError::DatasetDestroyFailed {
                dataset: dataset.clone(),
                failure: StorageEffectFailure::DestructiveEffect {
                    message: "dataset busy".to_owned(),
                },
            },
        });

        runtime.run().await;

        let state = runtime.state.lock().unwrap();
        assert!(state.pin_present);
        assert_eq!(
            failed_transition(&state),
            &VolumeRemoveFailure::DatasetDestroyFailed {
                machine_id: machine_id("machine_a"),
                dataset,
                message: failure_message("destructive ZFS effect refused: dataset busy"),
            }
        );
    }

    #[tokio::test]
    async fn dataset_unavailability_maps_to_public_dataset_failure() {
        let pin = provisioned_pin();
        let VolumeKind::Provisioned { dataset, .. } = pin.kind() else {
            panic!("provisioned pin expected");
        };
        let dataset = dataset.clone();
        let runtime = FakeRuntime::new(pin);
        runtime.state.lock().unwrap().dataset_result = Err(MachineVolumeRemoveError::Unavailable {
            machine_id: machine_id("machine_a"),
            message: failure_message("machine timed out"),
        });

        runtime.run().await;

        let state = runtime.state.lock().unwrap();
        assert_eq!(
            failed_transition(&state),
            &VolumeRemoveFailure::DatasetDestroyFailed {
                machine_id: machine_id("machine_a"),
                dataset,
                message: failure_message("machine timed out"),
            }
        );
        assert!(state.pin_present);
    }

    #[tokio::test]
    async fn commit_failure_retains_pin_and_retry_repeats_idempotent_effects_to_completion() {
        let runtime = FakeRuntime::new(provisioned_pin());
        runtime.state.lock().unwrap().commit_result = Err(failure_message("commit failed"));

        runtime.run_for("op_remove_first_attempt").await;
        {
            let state = runtime.state.lock().unwrap();
            assert!(state.pin_present);
            assert_eq!(state.effects.len(), 2);
            assert!(matches!(
                failed_transition(&state),
                VolumeRemoveFailure::ControlPlaneCommitFailed { .. }
            ));
        }

        runtime.state.lock().unwrap().commit_result = Ok(());
        runtime.run_for("op_remove_retry").await;

        let state = runtime.state.lock().unwrap();
        assert_eq!(state.effects.len(), 4);
        assert!(!state.pin_present);
        assert_eq!(
            state.transition_operation_ids,
            [
                operation_id("op_remove_first_attempt"),
                operation_id("op_remove_first_attempt"),
                operation_id("op_remove_first_attempt"),
                operation_id("op_remove_retry"),
                operation_id("op_remove_retry"),
                operation_id("op_remove_retry"),
            ]
        );
        assert_eq!(
            state.transitions.last(),
            Some(&VolumeRemoveTransition::Completed)
        );
    }

    #[tokio::test]
    async fn in_use_guard_runs_no_machine_effects() {
        let runtime = FakeRuntime::new(provisioned_pin());
        let mut target = serving_target_entry_in("team-a", "web", "entry_a");
        target.volume_names = vec![volume_name()];
        runtime.state.lock().unwrap().intent.serving_target_entries = vec![target];

        runtime.run().await;

        let state = runtime.state.lock().unwrap();
        assert!(state.effects.is_empty());
        assert!(state.pin_present);
        assert!(matches!(
            failed_transition(&state),
            VolumeRemoveFailure::VolumeInUse { .. }
        ));
    }

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
            dataplane_projection: ployz_core::network::DataplaneProjection::try_new(
                Vec::new(),
                None,
            )
            .expect("empty projection"),
            route_bindings: Vec::new(),
            serving_target_entries: vec![target],
            volume_pins: vec![VolumePinState::plain(
                namespace_id.clone(),
                volume_name.clone(),
                machine_id("machine_a"),
            )],
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
