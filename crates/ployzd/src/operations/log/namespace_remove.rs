use super::{
    AcceptedNamespaceRemoveSubmission, NamespaceRemoveOperationSubmission, NamespaceRemovePayload,
    OperationRepository, OperationStatusWrite, RecordNamespaceRemoveTransitionError,
    RecordOperationEventOutcome, SubmitOperationError,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{NamespaceRemoveTransition, OperationEvent, RouteTarget};

impl OperationRepository {
    pub async fn submit_namespace_remove(
        &self,
        submission: NamespaceRemoveOperationSubmission,
    ) -> Result<AcceptedNamespaceRemoveSubmission, SubmitOperationError> {
        let submitted = self
            .submit_operation::<NamespaceRemoveOperationSubmission>(
                submission.operation_id,
                NamespaceRemovePayload {
                    namespace_id: submission.namespace_id,
                },
            )
            .await?;
        Ok(AcceptedNamespaceRemoveSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            namespace_id: submitted.payload.namespace_id,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn record_namespace_remove_transition(
        &self,
        operation_id: &OperationId,
        transition: NamespaceRemoveTransition,
    ) -> Result<OperationStatusWrite, RecordNamespaceRemoveTransitionError> {
        self.record_operation_event(operation_id, transition.event(operation_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_namespace_remove_route_binding(
        &self,
        operation_id: &OperationId,
        target: RouteTarget,
    ) -> Result<ployz_core::ops::EventSequence, RecordNamespaceRemoveTransitionError> {
        self.record_operation_event(
            operation_id,
            OperationEvent::NamespaceRemoveRouteBindingRemoved {
                operation_id: operation_id.clone(),
                target,
            },
        )
        .await
        .map(RecordOperationEventOutcome::sequence)
    }

    pub async fn record_namespace_remove_container(
        &self,
        operation_id: &OperationId,
        machine_id: ployz_core::ids::MachineId,
        container_id: ployz_core::ids::ContainerId,
    ) -> Result<ployz_core::ops::EventSequence, RecordNamespaceRemoveTransitionError> {
        self.record_operation_event(
            operation_id,
            OperationEvent::NamespaceRemoveContainerRemoved {
                operation_id: operation_id.clone(),
                machine_id,
                container_id,
            },
        )
        .await
        .map(RecordOperationEventOutcome::sequence)
    }
}
