use super::{
    AcceptedBuildSubmission, BuildOperationPayload, BuildOperationSubmission, OperationRepository,
    OperationStatusWrite, RecordBuildEvidenceError, RecordBuildTransitionError,
    RecordOperationEventOutcome, SubmitOperationError,
};
use ployz_core::ids::OperationId;
use ployz_core::operation::{BuildEvidence, BuildTransition, EventSequence, OperationEvent};

impl OperationRepository {
    pub async fn submit_build(
        &self,
        submission: BuildOperationSubmission,
    ) -> Result<AcceptedBuildSubmission, SubmitOperationError> {
        let payload = BuildOperationPayload {
            source: submission.source,
            adapter: submission.adapter,
            platforms: submission.platforms,
        };
        let requested_payload = payload.clone();
        let submitted = self
            .submit_operation::<BuildOperationSubmission>(submission.operation_id, payload)
            .await?;
        if submitted.payload != requested_payload {
            return Err(SubmitOperationError::DuplicateSequenceMismatch {
                sequence: submitted.start_sequence,
            });
        }
        Ok(AcceptedBuildSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            source: submitted.payload.source,
            adapter: submitted.payload.adapter,
            platforms: submitted.payload.platforms,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn record_build_transition(
        &self,
        operation_id: &OperationId,
        transition: BuildTransition,
    ) -> Result<OperationStatusWrite, RecordBuildTransitionError> {
        self.record_operation_event(operation_id, transition.event(operation_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_build_evidence(
        &self,
        operation_id: &OperationId,
        evidence: BuildEvidence,
    ) -> Result<EventSequence, RecordBuildEvidenceError> {
        self.record_operation_event(operation_id, build_evidence_event(operation_id, evidence))
            .await
            .map(RecordOperationEventOutcome::sequence)
    }
}

fn build_evidence_event(operation_id: &OperationId, evidence: BuildEvidence) -> OperationEvent {
    match evidence {
        BuildEvidence::VerifiedCommit {
            platform,
            machine_id,
            commit,
        } => OperationEvent::BuildCommitVerified {
            operation_id: operation_id.clone(),
            platform,
            machine_id,
            commit,
        },
        BuildEvidence::PlatformPlaced {
            platform,
            machine_id,
        } => OperationEvent::BuildPlatformPlaced {
            operation_id: operation_id.clone(),
            platform,
            machine_id,
        },
        BuildEvidence::ToolchainVerified {
            platform,
            machine_id,
            toolchain,
        } => OperationEvent::BuildPlatformToolchainVerified {
            operation_id: operation_id.clone(),
            platform,
            machine_id,
            toolchain,
        },
        BuildEvidence::PlatformLog {
            platform,
            machine_id,
            chunk,
        } => OperationEvent::BuildPlatformLog {
            operation_id: operation_id.clone(),
            platform,
            machine_id,
            chunk,
        },
        BuildEvidence::PlatformLogTruncated {
            platform,
            machine_id,
            omitted_bytes,
        } => OperationEvent::BuildPlatformLogTruncated {
            operation_id: operation_id.clone(),
            platform,
            machine_id,
            omitted_bytes,
        },
        BuildEvidence::PlatformLogGap {
            platform,
            machine_id,
            expected_sequence,
            final_sequence,
        } => OperationEvent::BuildPlatformLogGap {
            operation_id: operation_id.clone(),
            platform,
            machine_id,
            expected_sequence,
            final_sequence,
        },
        BuildEvidence::PlatformCompleted {
            platform,
            machine_id,
            image,
        } => OperationEvent::BuildPlatformCompleted {
            operation_id: operation_id.clone(),
            platform,
            machine_id,
            image,
        },
        BuildEvidence::PlatformFailed {
            platform,
            machine_id,
            failure,
        } => OperationEvent::BuildPlatformFailed {
            operation_id: operation_id.clone(),
            platform,
            machine_id,
            failure,
        },
    }
}
