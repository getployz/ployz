use super::{
    AcceptedDeploySubmission, DeployOperationSubmission, OperationRepository, OperationStatusWrite,
    RecordDeployEvidenceError, RecordDeployTransitionError, RecordOperationEventOutcome,
    StoredDeployClaim, SubmitOperationError, create_or_adopt_deploy_claim, store_status,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{DeployEvidence, DeployTransition, EventSequence};

impl OperationRepository {
    pub async fn claim_deploy(
        &self,
        submission: DeployOperationSubmission,
    ) -> Result<DeployOperationSubmission, SubmitOperationError> {
        let DeployOperationSubmission {
            operation_id,
            idempotency_key,
            target,
        } = submission;
        let claim = StoredDeployClaim {
            operation_id,
            target,
        };
        let key = idempotency_key.clone();
        let adopted = self
            .store
            .call(move |conn| create_or_adopt_deploy_claim(conn, key.as_str(), &claim))
            .await
            .map_err(store_status)?;
        let claim = adopted
            .into_value()
            .map_err(SubmitOperationError::StoreStatus)?;
        Ok(DeployOperationSubmission {
            operation_id: claim.operation_id,
            idempotency_key,
            target: claim.target,
        })
    }

    pub async fn submit_deploy(
        &self,
        submission: DeployOperationSubmission,
    ) -> Result<AcceptedDeploySubmission, SubmitOperationError> {
        let claim = self.claim_deploy(submission).await?;
        let submitted = self
            .submit_operation::<DeployOperationSubmission>(claim.operation_id, claim.target)
            .await?;
        Ok(AcceptedDeploySubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            target: submitted.payload,
            registry_credentials: Vec::new(),
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn record_deploy_transition(
        &self,
        operation_id: &OperationId,
        transition: DeployTransition,
    ) -> Result<OperationStatusWrite, RecordDeployTransitionError> {
        self.record_operation_event(operation_id, transition.event(operation_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_deploy_evidence(
        &self,
        operation_id: &OperationId,
        evidence: DeployEvidence,
    ) -> Result<EventSequence, RecordDeployEvidenceError> {
        self.record_operation_event(operation_id, evidence.event(operation_id))
            .await
            .map(RecordOperationEventOutcome::sequence)
    }
}
