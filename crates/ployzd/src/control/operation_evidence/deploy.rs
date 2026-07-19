use super::{
    AcceptedDeploySubmission, DeployOperationPayload, DeployOperationSubmission,
    OperationRepository, OperationStatusStoreError, OperationStatusWrite,
    RecordDeployEvidenceError, RecordDeployTransitionError, RecordOperationEventOutcome,
    StoredDeployClaim, SubmitOperationError, SubmitTxn, SubmittedOperation,
    create_or_adopt_deploy_claim, create_submit, existing_submit, index_error, store_status,
    subject_token_conversion,
};
use ployz_core::deploy::{DeployRequestEvidence, DeployReservationId};
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
        let target_evidence = DeployRequestEvidence::from_request(&target);
        let claim = StoredDeployClaim {
            operation_id,
            reservation_id,
            target: target_evidence.clone(),
        };
        let key = idempotency_key.clone();
        let adopted = self
            .store
            .call(move |conn| create_or_adopt_deploy_claim(conn, key.as_str(), &claim))
            .await
            .map_err(store_status)?;
        let stored = adopted
            .into_value()
            .map_err(SubmitOperationError::StoreStatus)?;
        Ok(DeployOperationSubmission {
            operation_id: stored.operation_id,
            idempotency_key,
            reservation_id: stored.reservation_id,
            target,
        })
    }

    pub async fn submit_claimed_deploy(
        &self,
        claim: DeployOperationSubmission,
    ) -> Result<AcceptedDeploySubmission, SubmitOperationError> {
        let target = claim.target;
        let payload = DeployOperationPayload {
            reservation_id: Some(claim.reservation_id),
            target: DeployRequestEvidence::from_request(&target),
        };
        let submitted = self
            .submit_deploy_operation(claim.operation_id, payload)
            .await?;
        Ok(AcceptedDeploySubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            target,
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
        let existing = match existing {
            SubmitTxn::Existing {
                start_sequence,
                payload,
                should_start_execution: _,
            } => SubmitTxn::Existing {
                start_sequence,
                payload,
                should_start_execution: false,
            },
            other => other,
        };
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
    let namespace_id = &payload.target.request().namespace_id;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::store::CoreStore;
    use futures_util::StreamExt;
    use ployz_core::deploy::{
        ContainerRuntimeSpec, DeployRequest, DeployServiceSpec, EnvName, EnvValue, ImageReference,
        ImageSource, ReplicaCount, ServiceEnvironment, ServiceMode,
    };
    use ployz_core::operation::OperationEventReplayRequest;
    use ployz_test_support::ids::{
        event_replay_limit, event_sequence, idempotency_key, namespace_id, operation_id, service_id,
    };
    use std::collections::BTreeMap;
    use std::time::Duration;

    const SECRET: &str = "sentinel-deploy-environment-secret";

    #[tokio::test]
    async fn deploy_repository_persists_and_publishes_environment_evidence_only() {
        let nats = ployz_test_support::nats::TestNats::start().await;
        let store = CoreStore::open_in_memory().await.expect("core store opens");
        let repository = OperationRepository::open(store.clone(), nats.controller.clone());
        let mut progress = nats
            .user
            .subscribe("plz.v1.progress.>")
            .await
            .expect("operator subscribes to progress");
        nats.user
            .flush()
            .await
            .expect("progress subscription flushes");
        let target = secret_deploy_request(SECRET);
        let reservation_id = repository
            .issue_deploy_reservation(&target.namespace_id)
            .await
            .expect("reservation issues");
        let claim = repository
            .claim_deploy(DeployOperationSubmission {
                operation_id: operation_id("op_secret_evidence"),
                idempotency_key: idempotency_key("idem_secret_evidence"),
                reservation_id,
                target: target.clone(),
            })
            .await
            .expect("fresh deploy claim succeeds");
        assert_eq!(claim.target, target, "fresh claim keeps the live request");

        let accepted = repository
            .submit_claimed_deploy(claim)
            .await
            .expect("claimed deploy submits");
        assert_eq!(
            accepted.target, target,
            "accepted execution keeps the live request"
        );
        assert!(accepted.should_start_execution);

        let (claim_json, event_json) = store
            .call(|conn| {
                let claim_json = conn.query_row(
                    "SELECT json FROM deploy_claims WHERE key = ?1",
                    ["idem_secret_evidence"],
                    |row| row.get::<_, String>(0),
                )?;
                let event_json = conn.query_row(
                    "SELECT event_json FROM operation_events WHERE operation_id = ?1 AND sequence = 1",
                    ["op_secret_evidence"],
                    |row| row.get::<_, String>(0),
                )?;
                Ok((claim_json, event_json))
            })
            .await
            .expect("stored evidence reads");
        assert_redacted_evidence(&claim_json);
        assert_redacted_evidence(&event_json);

        let message = tokio::time::timeout(Duration::from_secs(2), progress.next())
            .await
            .expect("submission progress arrives")
            .expect("progress subscription stays open");
        let progress_json = std::str::from_utf8(&message.payload).expect("progress is JSON");
        assert_redacted_evidence(progress_json);

        let replay = repository
            .replay_operation_events(OperationEventReplayRequest {
                operation_id: accepted.operation_id,
                start_sequence: event_sequence(1),
                limit: event_replay_limit(10),
            })
            .await
            .expect("operation event replays");
        let replay_json = serde_json::to_string(&replay).expect("replay serializes");
        assert_redacted_evidence(&replay_json);
    }

    #[tokio::test]
    async fn duplicate_deploy_claim_uses_matching_presented_live_request_without_restart() {
        let nats = ployz_test_support::nats::TestNats::start().await;
        let store = CoreStore::open_in_memory().await.expect("core store opens");
        let repository = OperationRepository::open(store, nats.controller);
        let target = secret_deploy_request(SECRET);
        let reservation_id = repository
            .issue_deploy_reservation(&target.namespace_id)
            .await
            .expect("reservation issues");
        let first = repository
            .claim_deploy(DeployOperationSubmission {
                operation_id: operation_id("op_original"),
                idempotency_key: idempotency_key("idem_duplicate"),
                reservation_id,
                target: target.clone(),
            })
            .await
            .expect("first claim succeeds");
        repository
            .submit_claimed_deploy(first)
            .await
            .expect("first submit succeeds");

        let duplicate = repository
            .claim_deploy(DeployOperationSubmission {
                operation_id: operation_id("op_discarded"),
                idempotency_key: idempotency_key("idem_duplicate"),
                reservation_id: DeployReservationId::try_new(reservation_id.get() + 1)
                    .expect("next reservation id shape is valid"),
                target: target.clone(),
            })
            .await
            .expect("matching evidence adopts the stored claim");
        assert_eq!(duplicate.operation_id, operation_id("op_original"));
        assert_eq!(duplicate.reservation_id, reservation_id);
        assert_eq!(
            duplicate.target, target,
            "caller live values stay in memory"
        );
        let accepted = repository
            .submit_claimed_deploy(duplicate)
            .await
            .expect("existing operation is accepted idempotently");
        assert!(!accepted.should_start_execution);
        assert_eq!(accepted.target, target);

        let mismatch = repository
            .claim_deploy(DeployOperationSubmission {
                operation_id: operation_id("op_mismatch"),
                idempotency_key: idempotency_key("idem_duplicate"),
                reservation_id,
                target: secret_deploy_request("different-secret"),
            })
            .await;
        assert!(matches!(
            mismatch,
            Err(SubmitOperationError::StoreStatus(
                OperationStatusStoreError::CasConflict { .. }
            ))
        ));
    }

    fn assert_redacted_evidence(json: &str) {
        assert!(
            !json.contains(SECRET),
            "serialized evidence leaked the sentinel"
        );
        assert!(
            json.contains("API_TOKEN"),
            "environment name remains visible"
        );
        assert!(
            json.contains("v1:sha256:"),
            "environment fingerprint remains visible"
        );
    }

    fn secret_deploy_request(secret: &str) -> DeployRequest {
        let environment = BTreeMap::from([(
            EnvName::try_new("API_TOKEN").expect("environment name"),
            EnvValue::try_new(secret).expect("environment value"),
        )]);
        DeployRequest {
            namespace_id: namespace_id("secret-app"),
            origin: None,
            volumes: BTreeMap::new(),
            services: vec![DeployServiceSpec {
                keep: None,
                service_id: service_id("api"),
                image: ImageReference::try_new("registry.example/api:latest")
                    .expect("image reference"),
                image_source: ImageSource::Registry,
                mode: ServiceMode::Replicated {
                    replicas: ReplicaCount::try_new(1).expect("replicas"),
                },
                runtime: ContainerRuntimeSpec {
                    environment: ServiceEnvironment::from(environment),
                    ..ContainerRuntimeSpec::image_defaults()
                },
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            }],
        }
    }
}
