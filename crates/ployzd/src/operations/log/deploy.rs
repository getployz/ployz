use super::{
    AcceptedDeploySubmission, DeployOperationPayload, DeployOperationSubmission,
    IssueDeployReservationError, OperationRepository, OperationStatusWrite,
    RecordDeployEvidenceError, RecordDeployTransitionError, RecordOperationEventOutcome,
    StoredDeployClaim, SubmitOperationError, create_or_adopt_deploy_claim,
    deploy_reservation_id_from_text, index_error, store_status,
};
use ployz_core::deploy::DeployReservationId;
use ployz_core::ids::{NamespaceId, OperationId};
use ployz_core::ops::{DeployEvidence, DeployTransition, EventSequence};
use rusqlite::{OptionalExtension, params};

impl OperationRepository {
    pub async fn claim_deploy(
        &self,
        submission: DeployOperationSubmission,
    ) -> Result<DeployOperationSubmission, SubmitOperationError> {
        let DeployOperationSubmission {
            operation_id,
            idempotency_key,
            reservation_id,
            target,
        } = submission;
        let claim = StoredDeployClaim {
            operation_id,
            reservation_id,
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
            reservation_id: claim.reservation_id,
            target: claim.target,
        })
    }

    pub async fn submit_deploy(
        &self,
        submission: DeployOperationSubmission,
    ) -> Result<AcceptedDeploySubmission, SubmitOperationError> {
        let claim = self.claim_deploy(submission).await?;
        let payload = DeployOperationPayload {
            reservation_id: Some(claim.reservation_id),
            target: claim.target,
        };
        let submitted = self
            .submit_deploy_operation(claim.operation_id, payload)
            .await?;
        Ok(AcceptedDeploySubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            target: submitted.payload.target,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn issue_deploy_reservation(
        &self,
        namespace_id: &NamespaceId,
    ) -> Result<DeployReservationId, IssueDeployReservationError> {
        let namespace_id = namespace_id.clone();
        self.store
            .call(move |conn| {
                let transaction = conn.transaction()?;
                let last_issued = transaction
                    .query_row(
                        "SELECT last_issued FROM deploy_reservations WHERE namespace_id = ?1",
                        [namespace_id.as_str()],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .map(deploy_reservation_id_from_text)
                    .transpose()?;
                let next = match last_issued {
                    Some(last_issued) => last_issued
                        .get()
                        .checked_add(1)
                        .and_then(|value| DeployReservationId::try_new(value).ok())
                        .ok_or(rusqlite::Error::IntegralValueOutOfRange(0, i64::MAX))?,
                    None => DeployReservationId::first(),
                };
                transaction.execute(
                    "INSERT INTO deploy_reservations (namespace_id, last_issued)
                     VALUES (?1, ?2)
                     ON CONFLICT(namespace_id) DO UPDATE SET last_issued = excluded.last_issued",
                    params![namespace_id.as_str(), next.get().to_string()],
                )?;
                transaction.commit()?;
                Ok(next)
            })
            .await
            .map_err(|error| IssueDeployReservationError::StoreStatus(index_error(&error)))
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
