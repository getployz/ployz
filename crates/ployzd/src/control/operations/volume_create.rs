//! Operation-owned explicit volume admission and creation.

use std::time::Duration;

use crate::control::intent::namespace_intent::NamespaceIntentStore;
use crate::control::intent::service::NatsIntentReader;
use crate::control::operation_evidence::{AcceptedVolumeCreateSubmission, OperationRepository};
use crate::control::role_client::machine::{
    MachineVolumeEnsureError, NatsMachineContainerRuntime, NatsMachineFactsReader,
};
use crate::control::sequencer::OperationControllers;
use crate::tasks::TaskSpawner;
use async_trait::async_trait;
use ployz_core::deploy::{SingleVolumeAdmissionInput, VolumeSpec, admit_volume};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::intent::VolumePinState;
use ployz_core::machine::{StorageCapability, StorageTestimony, VolumeEnsureFailure};
use ployz_core::operation::{
    FailureMessage, VolumeCreateFailure, VolumeCreateRequest, VolumeCreateRunningStage,
    VolumeCreateTransition,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeStorageObservation {
    NoAnswer,
    Answered(Option<StorageCapability>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeCreateContext {
    pub accepted_machine_ids: Vec<MachineId>,
    pub volume_pins: Vec<VolumePinState>,
    pub storage: VolumeStorageObservation,
}

#[async_trait]
pub trait VolumeCreateContextLoader: Send {
    async fn load_context(
        &mut self,
        request: &VolumeCreateRequest,
    ) -> Result<VolumeCreateContext, String>;
}

#[async_trait]
pub trait VolumeCreateRecorder: Send {
    async fn record(
        &mut self,
        operation_id: &OperationId,
        transition: VolumeCreateTransition,
    ) -> Result<(), String>;
}

#[async_trait]
pub trait VolumePinCommitter: Send {
    async fn replace_volume_pin(&mut self, pin: VolumePinState) -> Result<(), String>;
}

#[async_trait]
pub trait VolumeEnsureRuntime: Send {
    async fn ensure_volume(&mut self, pin: &VolumePinState)
    -> Result<(), VolumeEnsureAttemptError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeEnsureAttemptError {
    Unavailable {
        machine_id: MachineId,
        message: FailureMessage,
    },
    Failed(VolumeEnsureFailure),
}

#[derive(Debug, Clone)]
pub struct VolumeCreateOperation {
    client: async_nats::Client,
    namespace_intent: NamespaceIntentStore,
    controllers: OperationControllers,
    step_timeout: Duration,
    task_registry: TaskSpawner,
}

impl VolumeCreateOperation {
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

    pub async fn start(&self, accepted: AcceptedVolumeCreateSubmission) {
        if !accepted.should_start_execution {
            return;
        }
        let operation_id = accepted.request.operation_id.clone();
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

    #[tracing::instrument(name = "operation", level = "error", skip_all, fields(kind = "volume_create", operation_id = accepted.request.operation_id.as_str()))]
    async fn run(self, accepted: AcceptedVolumeCreateSubmission) {
        let namespace_id = accepted.request.namespace_id.clone();
        let operation_id = accepted.request.operation_id.clone();
        let intent_reader =
            NatsIntentReader::new(self.client.clone()).with_request_timeout(self.step_timeout);
        let facts_reader = NatsMachineFactsReader::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let mut context = NatsVolumeCreateContextLoader {
            intent_reader,
            facts_reader,
        };
        let mut recorder = RepositoryVolumeCreateRecorder {
            repository: self.controllers.repository().clone(),
        };
        let mut pin_committer = IntentVolumePinCommitter {
            namespace_intent: self.namespace_intent,
            intent_change_client: self.client.clone(),
        };
        let mut volume_runtime = NatsVolumeEnsureRuntime {
            runtime: NatsMachineContainerRuntime::new(self.client)
                .with_request_timeout(self.step_timeout),
        };
        if let Err(message) = execute_volume_create(
            accepted.request,
            VolumeCreatePorts {
                context: &mut context,
                recorder: &mut recorder,
                pin_committer: &mut pin_committer,
                volume_runtime: &mut volume_runtime,
            },
        )
        .await
        {
            tracing::error!(
                operation_id = operation_id.as_str(),
                error = %message,
                "volume create stopped after evidence failure"
            );
        }
        self.controllers
            .release_namespace(&namespace_id, &operation_id)
            .await;
    }
}

struct NatsVolumeCreateContextLoader {
    intent_reader: NatsIntentReader,
    facts_reader: NatsMachineFactsReader,
}

#[async_trait]
impl VolumeCreateContextLoader for NatsVolumeCreateContextLoader {
    async fn load_context(
        &mut self,
        request: &VolumeCreateRequest,
    ) -> Result<VolumeCreateContext, String> {
        let intent = self
            .intent_reader
            .intent()
            .await
            .map_err(|error| error.to_string())?;
        let storage = match request.spec {
            VolumeSpec::Plain => VolumeStorageObservation::NoAnswer,
            VolumeSpec::Provisioned { .. } => {
                match self.facts_reader.machine_facts(&request.machine_id).await {
                    Ok(facts) => VolumeStorageObservation::Answered(facts.storage().cloned()),
                    Err(_) => VolumeStorageObservation::NoAnswer,
                }
            }
        };
        Ok(VolumeCreateContext {
            accepted_machine_ids: intent
                .active_machines
                .into_iter()
                .map(|machine| machine.machine_id)
                .collect(),
            volume_pins: intent.volume_pins,
            storage,
        })
    }
}

struct RepositoryVolumeCreateRecorder {
    repository: OperationRepository,
}

#[async_trait]
impl VolumeCreateRecorder for RepositoryVolumeCreateRecorder {
    async fn record(
        &mut self,
        operation_id: &OperationId,
        transition: VolumeCreateTransition,
    ) -> Result<(), String> {
        self.repository
            .record_volume_create_transition(operation_id, transition)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

struct IntentVolumePinCommitter {
    namespace_intent: NamespaceIntentStore,
    intent_change_client: async_nats::Client,
}

#[async_trait]
impl VolumePinCommitter for IntentVolumePinCommitter {
    async fn replace_volume_pin(&mut self, pin: VolumePinState) -> Result<(), String> {
        self.namespace_intent
            .replace_volume_pin(pin)
            .await
            .map_err(|error| error.to_string())?;
        if let Err(error) = self
            .intent_change_client
            .publish(ployz_nats::subjects::INTENT_CHANGED, Vec::new().into())
            .await
        {
            tracing::warn!(error = %error, "intent-changed publish failed after volume pin commit");
        }
        Ok(())
    }
}

struct NatsVolumeEnsureRuntime {
    runtime: NatsMachineContainerRuntime,
}

#[async_trait]
impl VolumeEnsureRuntime for NatsVolumeEnsureRuntime {
    async fn ensure_volume(
        &mut self,
        pin: &VolumePinState,
    ) -> Result<(), VolumeEnsureAttemptError> {
        self.runtime
            .ensure_volume(pin.machine_id(), pin)
            .await
            .map_err(|error| match error {
                MachineVolumeEnsureError::Unavailable {
                    machine_id,
                    volume_name: _,
                    reason,
                } => VolumeEnsureAttemptError::Unavailable {
                    machine_id,
                    message: reason.failure_message(),
                },
                MachineVolumeEnsureError::Domain {
                    machine_id: _,
                    volume_name: _,
                    failure,
                } => VolumeEnsureAttemptError::Failed(failure),
            })
    }
}

pub struct VolumeCreatePorts<'a, L, R, C, E> {
    pub context: &'a mut L,
    pub recorder: &'a mut R,
    pub pin_committer: &'a mut C,
    pub volume_runtime: &'a mut E,
}

/// Executes the bounded operation after the Submitted event has been accepted.
/// A recorder failure stops before the next effect because unrecorded progress
/// must never be followed by machine or intent mutation.
pub async fn execute_volume_create<L, R, C, E>(
    request: VolumeCreateRequest,
    ports: VolumeCreatePorts<'_, L, R, C, E>,
) -> Result<(), String>
where
    L: VolumeCreateContextLoader,
    R: VolumeCreateRecorder,
    C: VolumePinCommitter,
    E: VolumeEnsureRuntime,
{
    let VolumeCreatePorts {
        context,
        recorder,
        pin_committer,
        volume_runtime,
    } = ports;
    recorder
        .record(&request.operation_id, VolumeCreateTransition::Planning)
        .await?;

    let context = match context.load_context(&request).await {
        Ok(context) => context,
        Err(message) => {
            return record_failure(
                recorder,
                &request.operation_id,
                VolumeCreateFailure::IntentReadFailed {
                    message: failure_message(message),
                },
            )
            .await;
        }
    };
    if !context.accepted_machine_ids.contains(&request.machine_id) {
        return record_failure(
            recorder,
            &request.operation_id,
            VolumeCreateFailure::MachineNotAccepted {
                machine_id: request.machine_id,
            },
        )
        .await;
    }

    let pin = match admitted_pin(&request, &context) {
        Ok(pin) => pin,
        Err(failure) => {
            return record_failure(
                recorder,
                &request.operation_id,
                VolumeCreateFailure::AdmissionFailed { failure },
            )
            .await;
        }
    };

    recorder
        .record(
            &request.operation_id,
            VolumeCreateTransition::Running {
                stage: VolumeCreateRunningStage::CommittingPin,
            },
        )
        .await?;
    if let Err(message) = pin_committer.replace_volume_pin(pin.clone()).await {
        return record_failure(
            recorder,
            &request.operation_id,
            VolumeCreateFailure::PinCommitFailed {
                pin,
                message: failure_message(message),
            },
        )
        .await;
    }

    recorder
        .record(
            &request.operation_id,
            VolumeCreateTransition::Running {
                stage: VolumeCreateRunningStage::EnsuringVolume,
            },
        )
        .await?;
    if let Err(error) = volume_runtime.ensure_volume(&pin).await {
        let failure = match error {
            VolumeEnsureAttemptError::Unavailable {
                machine_id,
                message,
            } => VolumeCreateFailure::MachineUnavailable {
                machine_id,
                message,
            },
            VolumeEnsureAttemptError::Failed(failure) => VolumeCreateFailure::EnsureFailed {
                machine_id: request.machine_id,
                volume_name: request.volume_name,
                failure,
            },
        };
        return record_failure(recorder, &request.operation_id, failure).await;
    }

    recorder
        .record(&request.operation_id, VolumeCreateTransition::Completed)
        .await
}

fn admitted_pin(
    request: &VolumeCreateRequest,
    context: &VolumeCreateContext,
) -> Result<VolumePinState, ployz_core::deploy::VolumeAdmissionFailure> {
    let storage_testimony = match &context.storage {
        VolumeStorageObservation::NoAnswer => StorageTestimony::NoAnswer,
        VolumeStorageObservation::Answered(storage) => StorageTestimony::Answered(storage.as_ref()),
    };
    let decision = admit_volume(SingleVolumeAdmissionInput {
        namespace_id: &request.namespace_id,
        volume_name: &request.volume_name,
        declaration: &request.spec,
        volume_pins: &context.volume_pins,
        selected_machine_id: &request.machine_id,
        storage_testimony,
    })?;
    Ok(decision.desired_pin().clone())
}

async fn record_failure<R: VolumeCreateRecorder>(
    recorder: &mut R,
    operation_id: &OperationId,
    failure: VolumeCreateFailure,
) -> Result<(), String> {
    recorder
        .record(operation_id, VolumeCreateTransition::Failed { failure })
        .await
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).unwrap_or_else(|_| {
        FailureMessage::try_new("volume create failed").expect("static failure message is valid")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::{VolumeMaxSizeBytes, VolumeName, ZfsPoolName};
    use ployz_core::intent::VolumeKind;
    use ployz_core::machine::{PoolCapacityFacts, VolumeEnsureFailure};
    use ployz_test_support::ids::{machine_id, namespace_id, operation_id};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Effect {
        Recorded(VolumeCreateTransition),
        PinCommitted(VolumePinState),
        VolumeEnsured(VolumePinState),
    }

    struct FakeContext {
        result: Result<VolumeCreateContext, String>,
        loads: usize,
    }

    #[async_trait]
    impl VolumeCreateContextLoader for FakeContext {
        async fn load_context(
            &mut self,
            _request: &VolumeCreateRequest,
        ) -> Result<VolumeCreateContext, String> {
            self.loads += 1;
            self.result.clone()
        }
    }

    struct FakeRecorder {
        effects: Arc<Mutex<Vec<Effect>>>,
    }

    #[async_trait]
    impl VolumeCreateRecorder for FakeRecorder {
        async fn record(
            &mut self,
            _operation_id: &OperationId,
            transition: VolumeCreateTransition,
        ) -> Result<(), String> {
            self.effects
                .lock()
                .expect("effect log lock is available")
                .push(Effect::Recorded(transition));
            Ok(())
        }
    }

    struct FakeCommitter {
        effects: Arc<Mutex<Vec<Effect>>>,
        committed: Vec<VolumePinState>,
        failure: Option<String>,
    }

    #[async_trait]
    impl VolumePinCommitter for FakeCommitter {
        async fn replace_volume_pin(&mut self, pin: VolumePinState) -> Result<(), String> {
            if let Some(failure) = &self.failure {
                return Err(failure.clone());
            }
            self.effects
                .lock()
                .expect("effect log lock is available")
                .push(Effect::PinCommitted(pin.clone()));
            self.committed.retain(|existing| {
                existing.namespace_id() != pin.namespace_id()
                    || existing.volume_name() != pin.volume_name()
            });
            self.committed.push(pin);
            Ok(())
        }
    }

    struct FakeRuntime {
        effects: Arc<Mutex<Vec<Effect>>>,
        result: Result<(), VolumeEnsureAttemptError>,
    }

    #[async_trait]
    impl VolumeEnsureRuntime for FakeRuntime {
        async fn ensure_volume(
            &mut self,
            pin: &VolumePinState,
        ) -> Result<(), VolumeEnsureAttemptError> {
            self.effects
                .lock()
                .expect("effect log lock is available")
                .push(Effect::VolumeEnsured(pin.clone()));
            self.result.clone()
        }
    }

    fn request(spec: VolumeSpec) -> VolumeCreateRequest {
        VolumeCreateRequest {
            operation_id: operation_id("op_volume_create"),
            namespace_id: namespace_id("team_a"),
            volume_name: VolumeName::try_new("data").expect("valid volume name"),
            machine_id: machine_id("machine_a"),
            spec,
        }
    }

    fn plain_context() -> VolumeCreateContext {
        VolumeCreateContext {
            accepted_machine_ids: vec![machine_id("machine_a")],
            volume_pins: Vec::new(),
            storage: VolumeStorageObservation::NoAnswer,
        }
    }

    fn provisioned_context() -> VolumeCreateContext {
        let pool = ZfsPoolName::try_new("tank").expect("valid pool");
        VolumeCreateContext {
            accepted_machine_ids: vec![machine_id("machine_a")],
            volume_pins: Vec::new(),
            storage: VolumeStorageObservation::Answered(Some(StorageCapability::Ready {
                pool,
                capacity: PoolCapacityFacts {
                    total_bytes: 100,
                    provisioned_used_bytes: 0,
                    free_bytes: 100,
                    child_quotas: Vec::new(),
                },
            })),
        }
    }

    async fn run(
        request: VolumeCreateRequest,
        context: VolumeCreateContext,
        runtime_result: Result<(), VolumeEnsureAttemptError>,
        initial_pins: Vec<VolumePinState>,
    ) -> (Vec<Effect>, Vec<VolumePinState>) {
        run_with_commit_failure(request, context, runtime_result, initial_pins, None).await
    }

    async fn run_with_commit_failure(
        request: VolumeCreateRequest,
        context: VolumeCreateContext,
        runtime_result: Result<(), VolumeEnsureAttemptError>,
        initial_pins: Vec<VolumePinState>,
        commit_failure: Option<String>,
    ) -> (Vec<Effect>, Vec<VolumePinState>) {
        let effects = Arc::new(Mutex::new(Vec::new()));
        let mut context = FakeContext {
            result: Ok(context),
            loads: 0,
        };
        let mut recorder = FakeRecorder {
            effects: Arc::clone(&effects),
        };
        let mut committer = FakeCommitter {
            effects: Arc::clone(&effects),
            committed: initial_pins,
            failure: commit_failure,
        };
        let mut runtime = FakeRuntime {
            effects: Arc::clone(&effects),
            result: runtime_result,
        };
        execute_volume_create(
            request,
            VolumeCreatePorts {
                context: &mut context,
                recorder: &mut recorder,
                pin_committer: &mut committer,
                volume_runtime: &mut runtime,
            },
        )
        .await
        .expect("recorder remains available");
        let effects = effects
            .lock()
            .expect("effect log lock is available")
            .clone();
        (effects, committer.committed)
    }

    #[tokio::test]
    async fn plain_create_commits_pin_before_ensure_and_completes() {
        let (effects, pins) = run(
            request(VolumeSpec::Plain),
            plain_context(),
            Ok(()),
            Vec::new(),
        )
        .await;
        assert!(matches!(
            effects.first(),
            Some(Effect::Recorded(VolumeCreateTransition::Planning))
        ));
        let commit = effects
            .iter()
            .position(|effect| matches!(effect, Effect::PinCommitted(_)));
        let ensure = effects
            .iter()
            .position(|effect| matches!(effect, Effect::VolumeEnsured(_)));
        assert!(matches!((commit, ensure), (Some(commit), Some(ensure)) if commit < ensure));
        assert!(matches!(
            effects.last(),
            Some(Effect::Recorded(VolumeCreateTransition::Completed))
        ));
        assert_eq!(pins.len(), 1);
    }

    #[tokio::test]
    async fn provisioned_create_uses_live_storage_testimony() {
        let spec = VolumeSpec::Provisioned {
            max_size_bytes: VolumeMaxSizeBytes::try_new(40).expect("non-zero size"),
        };
        let (_, pins) = run(request(spec), provisioned_context(), Ok(()), Vec::new()).await;
        assert!(matches!(
            pins.first().map(VolumePinState::kind),
            Some(VolumeKind::Provisioned { .. })
        ));
    }

    #[tokio::test]
    async fn machine_silence_is_a_typed_terminal_admission_failure() {
        let spec = VolumeSpec::Provisioned {
            max_size_bytes: VolumeMaxSizeBytes::try_new(40).expect("non-zero size"),
        };
        let context = VolumeCreateContext {
            accepted_machine_ids: vec![machine_id("machine_a")],
            volume_pins: Vec::new(),
            storage: VolumeStorageObservation::NoAnswer,
        };
        let (effects, pins) = run(request(spec), context, Ok(()), Vec::new()).await;
        assert!(pins.is_empty());
        assert!(matches!(
            effects.last(),
            Some(Effect::Recorded(VolumeCreateTransition::Failed {
                failure: VolumeCreateFailure::AdmissionFailed {
                    failure: ployz_core::deploy::VolumeAdmissionFailure::MachineSilent { .. }
                }
            }))
        ));
    }

    #[tokio::test]
    async fn unaccepted_machine_fails_before_pin_or_machine_effect() {
        let context = VolumeCreateContext {
            accepted_machine_ids: vec![machine_id("machine_b")],
            ..plain_context()
        };
        let (effects, pins) = run(request(VolumeSpec::Plain), context, Ok(()), Vec::new()).await;
        assert!(pins.is_empty());
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::PinCommitted(_) | Effect::VolumeEnsured(_)))
        );
        assert!(matches!(
            effects.last(),
            Some(Effect::Recorded(VolumeCreateTransition::Failed {
                failure: VolumeCreateFailure::MachineNotAccepted { .. }
            }))
        ));
    }

    #[tokio::test]
    async fn pin_commit_failure_is_terminal_and_stops_before_machine_effect() {
        let (effects, pins) = run_with_commit_failure(
            request(VolumeSpec::Plain),
            plain_context(),
            Ok(()),
            Vec::new(),
            Some(String::new()),
        )
        .await;
        assert!(pins.is_empty());
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::VolumeEnsured(_)))
        );
        assert!(matches!(
            effects.last(),
            Some(Effect::Recorded(VolumeCreateTransition::Failed {
                failure: VolumeCreateFailure::PinCommitFailed { message, .. }
            })) if message.as_str() == "volume create failed"
        ));
    }

    #[tokio::test]
    async fn unavailable_machine_rpc_is_not_reported_as_a_domain_failure() {
        let (effects, retained) = run(
            request(VolumeSpec::Plain),
            plain_context(),
            Err(VolumeEnsureAttemptError::Unavailable {
                machine_id: machine_id("machine_a"),
                message: FailureMessage::try_new("request timed out")
                    .expect("valid failure message"),
            }),
            Vec::new(),
        )
        .await;
        assert_eq!(retained.len(), 1);
        assert!(matches!(
            effects.last(),
            Some(Effect::Recorded(VolumeCreateTransition::Failed {
                failure: VolumeCreateFailure::MachineUnavailable { .. }
            }))
        ));
    }

    #[tokio::test]
    async fn ensure_failure_retains_pin_and_retry_converges() {
        let ensure_failure = VolumeEnsureFailure::DockerEnsureFailed {
            volume_name: VolumeName::try_new("data").expect("valid volume name"),
            retained_dataset: None,
            message: "docker unavailable".to_owned(),
        };
        let request = request(VolumeSpec::Plain);
        let (first_effects, retained) = run(
            request.clone(),
            plain_context(),
            Err(VolumeEnsureAttemptError::Failed(ensure_failure)),
            Vec::new(),
        )
        .await;
        assert_eq!(retained.len(), 1);
        assert!(matches!(
            first_effects.last(),
            Some(Effect::Recorded(VolumeCreateTransition::Failed {
                failure: VolumeCreateFailure::EnsureFailed { .. }
            }))
        ));
        let retry_context = VolumeCreateContext {
            volume_pins: retained.clone(),
            ..plain_context()
        };
        let mut retry_request = request;
        retry_request.operation_id = operation_id("op_volume_create_retry");
        let (retry_effects, retried) = run(retry_request, retry_context, Ok(()), retained).await;
        assert_eq!(retried.len(), 1);
        assert!(matches!(
            retry_effects.last(),
            Some(Effect::Recorded(VolumeCreateTransition::Completed))
        ));
    }
}
