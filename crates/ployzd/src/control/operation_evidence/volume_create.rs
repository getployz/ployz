use super::{
    AcceptedVolumeCreateSubmission, OperationRepository, OperationStatusWrite,
    RecordOperationEventError, RecordOperationEventOutcome, SubmitOperationError,
    VolumeCreateOperationSubmission, VolumeCreatePayload,
};
use ployz_core::ids::OperationId;
use ployz_core::operation::VolumeCreateTransition;

impl OperationRepository {
    pub async fn submit_volume_create(
        &self,
        submission: VolumeCreateOperationSubmission,
    ) -> Result<AcceptedVolumeCreateSubmission, SubmitOperationError> {
        let operation_id = submission.request.operation_id.clone();
        let submitted = self
            .submit_operation::<VolumeCreateOperationSubmission>(
                operation_id,
                VolumeCreatePayload {
                    request: submission.request,
                },
            )
            .await?;
        Ok(AcceptedVolumeCreateSubmission {
            request: submitted.payload.request,
            start_sequence: submitted.start_sequence,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn record_volume_create_transition(
        &self,
        operation_id: &OperationId,
        transition: VolumeCreateTransition,
    ) -> Result<OperationStatusWrite, RecordOperationEventError> {
        self.record_operation_event(operation_id, transition.event(operation_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::store::CoreStore;
    use ployz_core::deploy::{VolumeName, VolumeSpec};
    use ployz_core::operation::{
        OperationEvent, OperationEventReplayRequest, OperationInterruptionCause, OperationStatus,
        VolumeCreateOperationState, VolumeCreateRequest, VolumeCreateRunningStage,
    };
    use ployz_test_support::ids::{
        event_replay_limit, event_sequence, machine_id, namespace_id, operation_id,
    };

    fn request() -> VolumeCreateRequest {
        VolumeCreateRequest {
            operation_id: operation_id("op_volume_create_evidence"),
            namespace_id: namespace_id("team_a"),
            volume_name: VolumeName::try_new("data").expect("valid volume name"),
            machine_id: machine_id("machine_a"),
            spec: VolumeSpec::Plain,
        }
    }

    #[tokio::test]
    async fn volume_create_evidence_replays_and_interrupts_from_the_last_durable_stage() {
        let nats = ployz_test_support::nats::TestNats::start().await;
        let store = CoreStore::open_in_memory()
            .await
            .expect("open operation store");
        let repository = OperationRepository::open(store, nats.controller);
        let request = request();
        let operation_id = request.operation_id.clone();

        repository
            .submit_volume_create(VolumeCreateOperationSubmission { request })
            .await
            .expect("submit volume create");
        repository
            .record_volume_create_transition(&operation_id, VolumeCreateTransition::Planning)
            .await
            .expect("record planning");
        repository
            .record_volume_create_transition(
                &operation_id,
                VolumeCreateTransition::Running {
                    stage: VolumeCreateRunningStage::CommittingPin,
                },
            )
            .await
            .expect("record pin commit stage");
        assert!(
            repository
                .record_interrupted_operation(
                    &operation_id,
                    OperationInterruptionCause::PriorCoreProcessLoss,
                )
                .await
                .expect("record interruption")
        );

        let snapshot = repository
            .operation_status_snapshot(&operation_id)
            .await
            .expect("read status")
            .expect("status exists");
        assert!(matches!(
            snapshot.status,
            OperationStatus::VolumeCreate {
                state: VolumeCreateOperationState::Interrupted { .. },
                ..
            }
        ));

        let replay = repository
            .replay_operation_events(OperationEventReplayRequest {
                operation_id,
                start_sequence: event_sequence(1),
                limit: event_replay_limit(10),
            })
            .await
            .expect("replay volume create events");
        assert_eq!(replay.events.len(), 4);
        assert!(matches!(
            replay.events.first().map(|event| &event.event),
            Some(OperationEvent::VolumeCreateSubmitted { .. })
        ));
        assert!(matches!(
            replay.events.last().map(|event| &event.event),
            Some(OperationEvent::OperationInterrupted { .. })
        ));
    }
}
