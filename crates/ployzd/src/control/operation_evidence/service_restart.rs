use super::{
    AcceptedServiceRestartSubmission, OperationRepository, OperationStatusWrite,
    RecordOperationEventOutcome, RecordServiceRestartTransitionError,
    ServiceRestartOperationSubmission, ServiceRestartPayload, SubmitOperationError,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::ServiceRestartTransition;

impl OperationRepository {
    pub async fn submit_service_restart(
        &self,
        submission: ServiceRestartOperationSubmission,
    ) -> Result<AcceptedServiceRestartSubmission, SubmitOperationError> {
        let submitted = self
            .submit_operation::<ServiceRestartOperationSubmission>(
                submission.operation_id,
                ServiceRestartPayload {
                    namespace_id: submission.namespace_id,
                    service_id: submission.service_id,
                },
            )
            .await?;
        Ok(AcceptedServiceRestartSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            namespace_id: submitted.payload.namespace_id,
            service_id: submitted.payload.service_id,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn record_service_restart_transition(
        &self,
        operation_id: &OperationId,
        transition: ServiceRestartTransition,
    ) -> Result<OperationStatusWrite, RecordServiceRestartTransitionError> {
        self.record_operation_event(operation_id, transition.event(operation_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_service_restart_container(
        &self,
        operation_id: &OperationId,
        machine_id: ployz_core::ids::MachineId,
        container_id: ployz_core::ids::ContainerId,
    ) -> Result<ployz_core::ops::EventSequence, RecordServiceRestartTransitionError> {
        self.record_operation_event(
            operation_id,
            ployz_core::ops::OperationEvent::ServiceRestartContainerRestarted {
                operation_id: operation_id.clone(),
                machine_id,
                container_id,
            },
        )
        .await
        .map(RecordOperationEventOutcome::sequence)
    }
}
