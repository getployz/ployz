use super::{
    AcceptedDeploySubmission, DeployOperationPayload, DeployOperationSubmission,
    OperationRepository, OperationStatusStoreError, OperationStatusWrite,
    RecordDeployEvidenceError, RecordDeployTransitionError, RecordOperationEventOutcome,
    StoredDeployClaim, SubmitOperationError, SubmitTxn, SubmittedOperation,
    create_or_adopt_deploy_claim, create_submit, existing_submit, index_error, store_status,
    subject_token_conversion,
};
use ployz_core::deploy::DeployReservationId;
use ployz_core::ids::{NamespaceId, OperationId};
use ployz_core::operation::{DeployEvidence, DeployTransition, EventSequence};
use rusqlite::{Connection, OptionalExtension, params};

pub(super) enum DeployReservationRejection {
    Stale {
        namespace_id: NamespaceId,
        reservation_id: DeployReservationId,
        last_committed_reservation_id: DeployReservationId,
    },
    AlreadyCommitted {
        namespace_id: NamespaceId,
        reservation_id: DeployReservationId,
        owner_operation_id: OperationId,
    },
}

enum DeploySubmitTxn {
    Submission(Box<SubmitTxn<DeployOperationPayload>>),
    Rejected(DeployReservationRejection),
}

impl From<DeployReservationRejection> for SubmitOperationError {
    fn from(value: DeployReservationRejection) -> Self {
        match value {
            DeployReservationRejection::Stale {
                namespace_id,
                reservation_id,
                last_committed_reservation_id,
            } => Self::StaleDeployReservation {
                namespace_id,
                reservation_id,
                last_committed_reservation_id,
            },
            DeployReservationRejection::AlreadyCommitted {
                namespace_id,
                reservation_id,
                owner_operation_id,
            } => Self::DeployReservationAlreadyCommitted {
                namespace_id,
                reservation_id,
                owner_operation_id,
            },
        }
    }
}

impl OperationRepository {
    async fn submit_deploy_operation(
        &self,
        operation_id: OperationId,
        payload: DeployOperationPayload,
    ) -> Result<SubmittedOperation<DeployOperationPayload>, SubmitOperationError> {
        let closure_payload = payload.clone();
        let closure_id = operation_id.clone();
        let outcome = self
            .store
            .call(move |conn| submit_deploy_operation_txn(conn, closure_id, closure_payload))
            .await
            .map_err(store_status)?;
        let outcome = match outcome {
            DeploySubmitTxn::Submission(outcome) => *outcome,
            DeploySubmitTxn::Rejected(rejection) => return Err(rejection.into()),
        };
        self.finish_submit(operation_id, payload, outcome).await
    }

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

    pub async fn submit_claimed_deploy(
        &self,
        claim: DeployOperationSubmission,
    ) -> Result<AcceptedDeploySubmission, SubmitOperationError> {
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
    ) -> Result<DeployReservationId, OperationStatusStoreError> {
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
            .map_err(|error| index_error(&error))
    }

    pub async fn check_deploy_reservation_fence(
        &self,
        namespace_id: &NamespaceId,
        reservation_id: DeployReservationId,
        operation_id: &OperationId,
    ) -> Result<(), SubmitOperationError> {
        let namespace_id = namespace_id.clone();
        let operation_id = operation_id.clone();
        let rejection = self
            .store
            .call(move |conn| {
                deploy_reservation_rejection(conn, &namespace_id, reservation_id, &operation_id)
            })
            .await
            .map_err(store_status)?;
        match rejection {
            Some(rejection) => Err(rejection.into()),
            None => Ok(()),
        }
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

fn submit_deploy_operation_txn(
    conn: &mut Connection,
    operation_id: OperationId,
    payload: DeployOperationPayload,
) -> Result<DeploySubmitTxn, rusqlite::Error> {
    let transaction = conn.transaction()?;
    if let Some(existing) =
        existing_submit::<DeployOperationSubmission>(&transaction, &operation_id)?
    {
        return Ok(DeploySubmitTxn::Submission(Box::new(existing)));
    }
    if let Some(rejection) = commit_deploy_reservation(&transaction, &operation_id, &payload)? {
        return Ok(rejection);
    }
    let created = create_submit::<DeployOperationSubmission>(&transaction, operation_id, payload)?;
    transaction.commit()?;
    Ok(DeploySubmitTxn::Submission(Box::new(created)))
}

fn commit_deploy_reservation(
    conn: &Connection,
    operation_id: &OperationId,
    payload: &DeployOperationPayload,
) -> Result<Option<DeploySubmitTxn>, rusqlite::Error> {
    let namespace_id = &payload.target.namespace_id;
    let reservation_id = payload
        .reservation_id
        .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    if let Some(rejection) =
        deploy_reservation_rejection(conn, namespace_id, reservation_id, operation_id)?
    {
        return Ok(Some(DeploySubmitTxn::Rejected(rejection)));
    }
    conn.execute(
        "UPDATE deploy_reservations
         SET last_committed = ?2, committed_owner_operation_id = ?3
         WHERE namespace_id = ?1",
        params![
            namespace_id.as_str(),
            reservation_id.get().to_string(),
            operation_id.as_str()
        ],
    )?;
    Ok(None)
}

pub(super) fn deploy_reservation_rejection(
    conn: &Connection,
    namespace_id: &NamespaceId,
    reservation_id: DeployReservationId,
    operation_id: &OperationId,
) -> Result<Option<DeployReservationRejection>, rusqlite::Error> {
    let (committed, owner): (Option<String>, Option<String>) = conn.query_row(
        "SELECT last_committed, committed_owner_operation_id
         FROM deploy_reservations WHERE namespace_id = ?1",
        [namespace_id.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let Some(committed) = committed else {
        return Ok(None);
    };
    let committed = deploy_reservation_id_from_text(committed)?;
    if reservation_id < committed {
        return Ok(Some(DeployReservationRejection::Stale {
            namespace_id: namespace_id.clone(),
            reservation_id,
            last_committed_reservation_id: committed,
        }));
    }
    if reservation_id == committed {
        let owner = owner.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let owner_operation_id = OperationId::try_new(owner).map_err(subject_token_conversion)?;
        if owner_operation_id != *operation_id {
            return Ok(Some(DeployReservationRejection::AlreadyCommitted {
                namespace_id: namespace_id.clone(),
                reservation_id,
                owner_operation_id,
            }));
        }
    }
    Ok(None)
}

fn deploy_reservation_id_from_text(value: String) -> Result<DeployReservationId, rusqlite::Error> {
    let number = value.parse::<u64>().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            value.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    DeployReservationId::try_new(number).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            value.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}
