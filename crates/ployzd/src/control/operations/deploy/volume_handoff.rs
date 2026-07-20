use std::collections::BTreeMap;

use ployz_core::deploy::{
    DeployCleanupContainer, DeployVolumeHandoffApplied, DeployVolumeHandoffAppliedParticipant,
    DeployVolumeHandoffParticipant, DeployVolumeHandoffPriorState, DeployVolumeHandoffStopOutcome,
    NonEmptyAppliedVolumeHandoffParticipants, NonEmptyVolumeHandoffParticipants,
    NonEmptyVolumeNames,
};
use ployz_core::ids::{MachineId, ServiceId};
use ployz_core::machine::runtime::ManagedContainerIdentity;
use ployz_core::operation::{
    DeployEvidence, DeployVolumeHandoffRestartFailure, DeployVolumeHandoffRestorationUnconfirmed,
    DeployVolumeHandoffRollbackContainerOutcome, DeployVolumeHandoffRollbackOutcome,
    DeployVolumeHandoffStopUncertain, FailureMessage, OperatorHint, RetainedArtifact,
};

use super::failure::DeployExecutionFailure;
use super::{
    DeployExecutionCommand, DeployExecutionError, DeployExecutionStep, DeployOperationRecorder,
    MachineContainerRuntime, MachineContainerRuntimeError, record_evidence,
    record_failure_evidence, with_step_timeout,
};
use crate::roles::machine::protocol::{
    MachineContainerRestartRpcRequest, MachineContainerStopOutcome, MachineContainerStopRpcRequest,
};

#[derive(Debug, Clone, Default)]
pub(super) struct VolumeHandoffRollbackState {
    services: BTreeMap<ServiceId, ServiceVolumeHandoffState>,
}

#[derive(Debug, Clone, Default)]
struct ServiceVolumeHandoffState {
    owners: Vec<VolumeOwnerState>,
    consumers: Vec<DeployCleanupContainer>,
    possible_consumers: Vec<PossibleVolumeConsumer>,
}

#[derive(Debug, Clone)]
struct VolumeOwnerState {
    participant: DeployVolumeHandoffParticipant,
    stop: VolumeOwnerStopState,
}

#[derive(Debug, Clone)]
enum VolumeOwnerStopState {
    Attempting,
    Applied(DeployVolumeHandoffStopOutcome),
    Uncertain(DeployVolumeHandoffStopUncertain),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PossibleVolumeConsumer {
    machine_id: MachineId,
    expected_identity: ManagedContainerIdentity,
}

pub(super) struct VolumeHandoffQuiescence {
    services: BTreeMap<ServiceId, QuiescedService>,
    retained_consumers: Vec<DeployCleanupContainer>,
    retained_artifacts: Vec<RetainedArtifact>,
}

pub(super) enum ServiceStartTracking {
    Ordinary,
    VolumeHandoff(ServiceId),
}

struct QuiescedService {
    owners: Vec<VolumeOwnerState>,
    quiescence_confirmed: bool,
}

impl VolumeHandoffRollbackState {
    pub(super) fn begin_consumer_start(
        &mut self,
        service_id: &ServiceId,
        machine_id: MachineId,
        expected_identity: ManagedContainerIdentity,
    ) {
        let possible = PossibleVolumeConsumer {
            machine_id,
            expected_identity,
        };
        let state = self.services.entry(service_id.clone()).or_default();
        if !state.possible_consumers.contains(&possible) {
            state.possible_consumers.push(possible);
        }
    }

    pub(super) fn consumer_started(
        &mut self,
        service_id: &ServiceId,
        target: DeployCleanupContainer,
    ) {
        let state = self.services.entry(service_id.clone()).or_default();
        state
            .possible_consumers
            .retain(|possible| possible.expected_identity != target.identity);
        if !state.consumers.contains(&target) {
            state.consumers.push(target);
        }
    }

    pub(super) fn consumer_did_not_start(
        &mut self,
        service_id: &ServiceId,
        expected_identity: &ManagedContainerIdentity,
    ) {
        if let Some(state) = self.services.get_mut(service_id) {
            state
                .possible_consumers
                .retain(|possible| &possible.expected_identity != expected_identity);
        }
    }

    pub(super) fn ambiguous_consumers(
        &mut self,
        service_id: &ServiceId,
        expected_identity: &ManagedContainerIdentity,
        targets: impl IntoIterator<Item = DeployCleanupContainer>,
    ) {
        let targets = targets.into_iter().collect::<Vec<_>>();
        if targets.is_empty() {
            return;
        }
        self.consumer_did_not_start(service_id, expected_identity);
        let state = self.services.entry(service_id.clone()).or_default();
        for target in targets {
            if !state.consumers.contains(&target) {
                state.consumers.push(target);
            }
        }
    }
}

impl ServiceStartTracking {
    pub(super) fn begin_consumer_start(
        &self,
        state: &mut VolumeHandoffRollbackState,
        machine_id: MachineId,
        expected_identity: ManagedContainerIdentity,
    ) {
        let Self::VolumeHandoff(service_id) = self else {
            return;
        };
        state.begin_consumer_start(service_id, machine_id, expected_identity);
    }

    pub(super) fn consumer_started(
        &self,
        state: &mut VolumeHandoffRollbackState,
        target: DeployCleanupContainer,
    ) {
        let Self::VolumeHandoff(service_id) = self else {
            return;
        };
        state.consumer_started(service_id, target);
    }

    pub(super) fn consumer_did_not_start(
        &self,
        state: &mut VolumeHandoffRollbackState,
        expected_identity: &ManagedContainerIdentity,
    ) {
        let Self::VolumeHandoff(service_id) = self else {
            return;
        };
        state.consumer_did_not_start(service_id, expected_identity);
    }

    pub(super) fn ambiguous_consumers(
        &self,
        state: &mut VolumeHandoffRollbackState,
        expected_identity: &ManagedContainerIdentity,
        targets: impl IntoIterator<Item = DeployCleanupContainer>,
    ) {
        let Self::VolumeHandoff(service_id) = self else {
            return;
        };
        state.ambiguous_consumers(service_id, expected_identity, targets);
    }
}

impl VolumeHandoffQuiescence {
    pub(super) fn retained_consumers(&self) -> &[DeployCleanupContainer] {
        &self.retained_consumers
    }

    pub(super) fn retained_artifacts(&self) -> &[RetainedArtifact] {
        &self.retained_artifacts
    }
}

pub(super) async fn stop_volume_owners<R, N>(
    command: &DeployExecutionCommand,
    service_id: &ServiceId,
    replacement_machine_id: &MachineId,
    participants: &NonEmptyVolumeHandoffParticipants,
    state: &mut VolumeHandoffRollbackState,
    recorder: &mut R,
    machine_runtime: &mut N,
) -> Result<ServiceStartTracking, DeployExecutionError>
where
    R: DeployOperationRecorder,
    N: MachineContainerRuntime,
{
    let mut applied_participants = Vec::new();
    for participant in participants.as_slice() {
        begin_owner_stop(state, service_id, participant.clone());
        let target = &participant.target;
        let result = with_step_timeout(
            command,
            DeployExecutionStep::StopVolumeOwner {
                machine_id: target.machine_id.clone(),
                container_id: target.container_id.clone(),
            },
            async {
                machine_runtime
                    .stop_container(
                        &target.machine_id,
                        MachineContainerStopRpcRequest {
                            operation_id: command.operation_id().clone(),
                            container_id: target.container_id.clone(),
                            expected_identity: target.identity.clone(),
                        },
                    )
                    .await
                    .map_err(DeployExecutionError::RunContainer)
            },
        )
        .await;
        match result {
            Ok(outcome) => {
                let stop_outcome = applied_stop_outcome(outcome);
                finish_owner_stop(state, service_id, target, stop_outcome);
                applied_participants.push(DeployVolumeHandoffAppliedParticipant {
                    target: target.clone(),
                    stop_outcome,
                    shared_volume_names: participant.shared_volume_names.clone(),
                });
            }
            Err(error) => {
                let uncertainty = stop_uncertainty(command, target, &error);
                finish_owner_stop_uncertain(state, service_id, target, uncertainty);
                return Err(error);
            }
        }
    }

    let superseded = NonEmptyAppliedVolumeHandoffParticipants::try_new(applied_participants)
        .expect("volume handoff contains at least one applied participant");
    let handoff =
        DeployVolumeHandoffApplied {
            machine_id: replacement_machine_id.clone(),
            volume_names: NonEmptyVolumeNames::try_new(participants.as_slice().iter().flat_map(
                |participant| participant.shared_volume_names.as_slice().iter().cloned(),
            ))
            .expect("volume handoff participants share at least one volume"),
            superseded,
        };

    record_evidence(
        command,
        recorder,
        DeployEvidence::VolumeHandoffApplied {
            service_id: service_id.clone(),
            handoff,
        },
    )
    .await?;
    Ok(ServiceStartTracking::VolumeHandoff(service_id.clone()))
}

const fn applied_stop_outcome(
    outcome: MachineContainerStopOutcome,
) -> DeployVolumeHandoffStopOutcome {
    match outcome {
        MachineContainerStopOutcome::StoppedRunning => {
            DeployVolumeHandoffStopOutcome::StoppedRunning
        }
        MachineContainerStopOutcome::AlreadyStopped => {
            DeployVolumeHandoffStopOutcome::AlreadyStopped
        }
        MachineContainerStopOutcome::Missing => DeployVolumeHandoffStopOutcome::Missing,
    }
}

fn begin_owner_stop(
    state: &mut VolumeHandoffRollbackState,
    service_id: &ServiceId,
    participant: DeployVolumeHandoffParticipant,
) {
    state
        .services
        .entry(service_id.clone())
        .or_default()
        .owners
        .push(VolumeOwnerState {
            participant,
            stop: VolumeOwnerStopState::Attempting,
        });
}

fn finish_owner_stop(
    state: &mut VolumeHandoffRollbackState,
    service_id: &ServiceId,
    target: &DeployCleanupContainer,
    outcome: DeployVolumeHandoffStopOutcome,
) {
    owner_state_mut(state, service_id, target).stop = VolumeOwnerStopState::Applied(outcome);
}

fn finish_owner_stop_uncertain(
    state: &mut VolumeHandoffRollbackState,
    service_id: &ServiceId,
    target: &DeployCleanupContainer,
    uncertainty: DeployVolumeHandoffStopUncertain,
) {
    owner_state_mut(state, service_id, target).stop = VolumeOwnerStopState::Uncertain(uncertainty);
}

fn owner_state_mut<'a>(
    state: &'a mut VolumeHandoffRollbackState,
    service_id: &ServiceId,
    target: &DeployCleanupContainer,
) -> &'a mut VolumeOwnerState {
    state
        .services
        .get_mut(service_id)
        .and_then(|service| {
            service
                .owners
                .iter_mut()
                .rev()
                .find(|owner| owner.participant.target == *target)
        })
        .expect("owner stop attempt is recorded before its result")
}

pub(super) async fn quiesce<N>(
    command: &DeployExecutionCommand,
    machine_runtime: &mut N,
    state: VolumeHandoffRollbackState,
) -> VolumeHandoffQuiescence
where
    N: MachineContainerRuntime,
{
    let mut services = BTreeMap::new();
    let mut retained_consumers = Vec::new();
    let mut retained_artifacts = Vec::new();
    for (service_id, service) in state.services {
        let mut quiescence_confirmed = service.possible_consumers.is_empty();
        for possible in service.possible_consumers {
            retained_artifacts.push(RetainedArtifact::VolumeConsumerStartUncertain {
                machine_id: possible.machine_id,
                inspect_hint: operation_hint(command),
                expected_identity: possible.expected_identity,
                message: failure_message(
                    "machine runtime did not establish whether the volume consumer started",
                ),
            });
        }
        for consumer in service.consumers {
            if !retained_consumers.contains(&consumer) {
                retained_consumers.push(consumer.clone());
            }
            let result = with_step_timeout(
                command,
                DeployExecutionStep::QuiesceVolumeConsumer {
                    machine_id: consumer.machine_id.clone(),
                    container_id: consumer.container_id.clone(),
                },
                async {
                    machine_runtime
                        .stop_container(
                            &consumer.machine_id,
                            MachineContainerStopRpcRequest {
                                operation_id: command.operation_id().clone(),
                                container_id: consumer.container_id.clone(),
                                expected_identity: consumer.identity.clone(),
                            },
                        )
                        .await
                        .map_err(DeployExecutionError::RunContainer)
                },
            )
            .await;
            match result {
                Ok(MachineContainerStopOutcome::StoppedRunning)
                | Ok(MachineContainerStopOutcome::AlreadyStopped) => {
                    retained_artifacts.push(RetainedArtifact::StartedContainer {
                        machine_id: consumer.machine_id.clone(),
                        container_id: consumer.container_id.clone(),
                        log_hint: log_hint(&consumer.container_id),
                    });
                }
                Ok(MachineContainerStopOutcome::Missing) => {}
                Err(error) => {
                    quiescence_confirmed = false;
                    retained_artifacts.push(RetainedArtifact::VolumeConsumerQuiescenceUncertain {
                        target: consumer.clone(),
                        uncertainty: stop_uncertainty(command, &consumer, &error),
                    });
                }
            }
        }
        for owner in &service.owners {
            if let VolumeOwnerStopState::Uncertain(uncertainty) = &owner.stop {
                retained_artifacts.push(RetainedArtifact::VolumeOwnerStopUncertain {
                    target: owner.participant.target.clone(),
                    prior_state: owner.participant.prior_state,
                    uncertainty: uncertainty.clone(),
                });
            }
        }
        services.insert(
            service_id,
            QuiescedService {
                owners: service.owners,
                quiescence_confirmed,
            },
        );
    }
    VolumeHandoffQuiescence {
        services,
        retained_consumers,
        retained_artifacts,
    }
}

pub(super) async fn restart_eligible<R, N>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
    machine_runtime: &mut N,
    quiescence: VolumeHandoffQuiescence,
    failure: &mut DeployExecutionFailure,
) where
    R: DeployOperationRecorder,
    N: MachineContainerRuntime,
{
    for (service_id, service) in quiescence.services {
        let mut outcomes = Vec::new();
        for owner in service.owners {
            if !restart_eligible_for(&owner) {
                continue;
            }
            let target = owner.participant.target;
            let outcome = if service.quiescence_confirmed {
                restart_owner(command, machine_runtime, &target).await
            } else {
                DeployVolumeHandoffRollbackOutcome::NotRestartedNewConsumerQuiescenceUnconfirmed
            };
            if let Some(reason) = restoration_unconfirmed(&outcome) {
                failure.add_retained_artifacts(vec![
                    RetainedArtifact::VolumeOwnerRestorationUnconfirmed {
                        target: target.clone(),
                        reason,
                    },
                ]);
            }
            outcomes.push(DeployVolumeHandoffRollbackContainerOutcome { target, outcome });
        }
        let mut by_machine: BTreeMap<MachineId, Vec<_>> = BTreeMap::new();
        for outcome in outcomes {
            by_machine
                .entry(outcome.target.machine_id.clone())
                .or_default()
                .push(outcome);
        }
        for (machine_id, outcomes) in by_machine {
            if let Some(error) = record_failure_evidence(
                command,
                recorder,
                DeployEvidence::VolumeHandoffRollbackFinished {
                    service_id: service_id.clone(),
                    machine_id,
                    outcomes,
                },
            )
            .await
            {
                failure.add_evidence_record_error(error);
            }
        }
    }
}

fn restoration_unconfirmed(
    outcome: &DeployVolumeHandoffRollbackOutcome,
) -> Option<DeployVolumeHandoffRestorationUnconfirmed> {
    match outcome {
        DeployVolumeHandoffRollbackOutcome::Restarted => None,
        DeployVolumeHandoffRollbackOutcome::RestartFailed { failure } => {
            Some(DeployVolumeHandoffRestorationUnconfirmed::RestartFailed {
                failure: failure.clone(),
            })
        }
        DeployVolumeHandoffRollbackOutcome::NotRestartedNewConsumerQuiescenceUnconfirmed => {
            Some(DeployVolumeHandoffRestorationUnconfirmed::NewConsumerQuiescenceUnconfirmed)
        }
    }
}

fn restart_eligible_for(owner: &VolumeOwnerState) -> bool {
    match &owner.stop {
        VolumeOwnerStopState::Applied(DeployVolumeHandoffStopOutcome::StoppedRunning) => true,
        VolumeOwnerStopState::Uncertain(_) => {
            owner.participant.prior_state == DeployVolumeHandoffPriorState::Running
        }
        VolumeOwnerStopState::Attempting
        | VolumeOwnerStopState::Applied(
            DeployVolumeHandoffStopOutcome::AlreadyStopped
            | DeployVolumeHandoffStopOutcome::Missing,
        ) => false,
    }
}

async fn restart_owner<N>(
    command: &DeployExecutionCommand,
    machine_runtime: &mut N,
    target: &DeployCleanupContainer,
) -> DeployVolumeHandoffRollbackOutcome
where
    N: MachineContainerRuntime,
{
    match with_step_timeout(
        command,
        DeployExecutionStep::RestartVolumeOwner {
            machine_id: target.machine_id.clone(),
            container_id: target.container_id.clone(),
        },
        async {
            machine_runtime
                .restart_container(
                    &target.machine_id,
                    MachineContainerRestartRpcRequest {
                        operation_id: command.operation_id().clone(),
                        container_id: target.container_id.clone(),
                        expected_identity: target.identity.clone(),
                    },
                )
                .await
                .map_err(DeployExecutionError::RunContainer)
        },
    )
    .await
    {
        Ok(()) => DeployVolumeHandoffRollbackOutcome::Restarted,
        Err(DeployExecutionError::RunContainer(
            MachineContainerRuntimeError::RestartContainerFailed {
                message,
                inspect_hint,
                ..
            },
        )) => DeployVolumeHandoffRollbackOutcome::RestartFailed {
            failure: DeployVolumeHandoffRestartFailure::StartFailed {
                message,
                inspect_hint,
            },
        },
        Err(DeployExecutionError::StepTimedOut {
            step: DeployExecutionStep::RestartVolumeOwner { .. },
            timeout,
        }) => DeployVolumeHandoffRollbackOutcome::RestartFailed {
            failure: DeployVolumeHandoffRestartFailure::TimedOut {
                timeout_seconds: timeout_seconds(timeout),
                inspect_hint: inspect_hint(&target.container_id),
            },
        },
        Err(DeployExecutionError::RunContainer(MachineContainerRuntimeError::Unavailable {
            reason,
            ..
        })) => DeployVolumeHandoffRollbackOutcome::RestartFailed {
            failure: DeployVolumeHandoffRestartFailure::RuntimeUnavailable {
                message: reason.failure_message(),
                inspect_hint: inspect_hint(&target.container_id),
            },
        },
        Err(error) => DeployVolumeHandoffRollbackOutcome::RestartFailed {
            failure: DeployVolumeHandoffRestartFailure::RuntimeUnavailable {
                message: failure_message(error.to_string()),
                inspect_hint: inspect_hint(&target.container_id),
            },
        },
    }
}

fn stop_uncertainty(
    command: &DeployExecutionCommand,
    target: &DeployCleanupContainer,
    error: &DeployExecutionError,
) -> DeployVolumeHandoffStopUncertain {
    match error {
        DeployExecutionError::RunContainer(MachineContainerRuntimeError::StopContainerFailed {
            message,
            inspect_hint,
            ..
        }) => DeployVolumeHandoffStopUncertain::StopFailed {
            message: message.clone(),
            inspect_hint: inspect_hint.clone(),
        },
        DeployExecutionError::RunContainer(MachineContainerRuntimeError::Unavailable {
            reason,
            ..
        }) => DeployVolumeHandoffStopUncertain::RuntimeUnavailable {
            message: reason.failure_message(),
            inspect_hint: inspect_hint(&target.container_id),
        },
        DeployExecutionError::StepTimedOut { timeout, .. } => {
            DeployVolumeHandoffStopUncertain::TimedOut {
                timeout_seconds: timeout_seconds(*timeout),
                inspect_hint: inspect_hint(&target.container_id),
            }
        }
        DeployExecutionError::Plan(_)
        | DeployExecutionError::PlanInconsistent { .. }
        | DeployExecutionError::StepId(_)
        | DeployExecutionError::InvalidImagePull { .. }
        | DeployExecutionError::InternalInvariant { .. }
        | DeployExecutionError::RecordTransition(_)
        | DeployExecutionError::RecordEvidence(_)
        | DeployExecutionError::Image { .. }
        | DeployExecutionError::RunContainer(_)
        | DeployExecutionError::EnsureVolume(_)
        | DeployExecutionError::PreStartHook(_)
        | DeployExecutionError::PreStartHookExited { .. }
        | DeployExecutionError::WaitHealthy(_)
        | DeployExecutionError::ProvisionCertificate { .. }
        | DeployExecutionError::CommitNamespaceState(_)
        | DeployExecutionError::Failed { .. } => {
            DeployVolumeHandoffStopUncertain::RuntimeUnavailable {
                message: failure_message(error.to_string()),
                inspect_hint: operation_hint(command),
            }
        }
    }
}

fn timeout_seconds(timeout: std::time::Duration) -> u32 {
    u32::try_from(timeout.as_secs().max(1)).unwrap_or(u32::MAX)
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message.into()).expect("generated volume handoff failure is non-empty")
}

fn inspect_hint(container_id: &ployz_core::ids::ContainerId) -> OperatorHint {
    OperatorHint::try_new(format!("ployz container inspect {}", container_id.as_str()))
        .expect("generated inspect hint is non-empty")
}

fn log_hint(container_id: &ployz_core::ids::ContainerId) -> OperatorHint {
    OperatorHint::try_new(format!("ployz logs {}", container_id.as_str()))
        .expect("generated log hint is non-empty")
}

fn operation_hint(command: &DeployExecutionCommand) -> OperatorHint {
    OperatorHint::try_new(format!(
        "ployz ops status {}",
        command.operation_id().as_str()
    ))
    .expect("generated operation hint is non-empty")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_test_support::{containers, ids};

    #[test]
    fn ordinary_service_tracking_cannot_enroll_volume_handoff_rollback_state() {
        let tracking = ServiceStartTracking::Ordinary;
        let mut state = VolumeHandoffRollbackState::default();
        let identity = containers::identity("svc_api").build();

        tracking.begin_consumer_start(&mut state, ids::machine_id("machine_a"), identity.clone());
        tracking.consumer_started(
            &mut state,
            DeployCleanupContainer {
                machine_id: ids::machine_id("machine_a"),
                container_id: ids::container_id("ctr_new"),
                identity: identity.clone(),
            },
        );
        tracking.consumer_did_not_start(&mut state, &identity);
        tracking.ambiguous_consumers(&mut state, &identity, []);

        assert!(state.services.is_empty());
    }
}
