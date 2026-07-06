//! Core-local operation evidence and working records, stored in SQLite.
//!
//! The operation projection (`operations`), the append-only event log
//! (`operation_events`), and the machine-add working records (the six index
//! tables) all live in the one core database. Every read is a query and every
//! mutation a transaction — there is no in-memory copy of this state. Business
//! outcomes (a duplicate submit, a stale projection, a fingerprint conflict)
//! travel back inside `Ok(...)`; only a genuine database failure is `Err`.

use crate::core_store::{CoreStore, CoreStoreError, from_json, query_json_list, to_json};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::install::{InstallArtifactVersion, MachineJoinBundle, MachineJoinSecretDelivery};
use ployz_core::machine::{
    IssuedJoinToken, JoinTokenFingerprint, JoinTokenRedeemedAt, MachineAddFailure, MachineName,
    RawJoinToken, redeem_pending_join_token,
};
use ployz_core::ops::{
    DeployEvidence, DeployTransition, EventSequence, MachineAddOperationState,
    MachineAddOperationStateName, MachineLifecycleTransition, MachineUpdateTransition,
    OperationEvent, OperationEventReplayCursor, OperationEventReplayLimit, OperationEventReplayPage,
    OperationEventReplayRequest, OperationIdempotencyKey, OperationKind, OperationProjection,
    OperationStatus, OperationStatusSnapshot, StatusProjectionError, project_operation_event,
    validate_fresh_deploy_evidence,
};
use ployz_core::roles::InstallRolePolicy;
use ployz_core::state::MachineLifecycle;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// One index table and the column its primary key lives in.
type IndexTable = (&'static str, &'static str);

const DEPLOY_CLAIMS: IndexTable = ("deploy_claims", "idempotency_key");
const MACHINE_ADD_CLAIMS: IndexTable = ("machine_add_claims", "idempotency_key");
const MACHINE_ADD_SUBMISSIONS: IndexTable = ("machine_add_submissions", "idempotency_key");
const MACHINE_ADD_SECRET_DELIVERIES: IndexTable =
    ("machine_add_secret_deliveries", "idempotency_key");
const MACHINE_ADD_MINT_CLAIMS: IndexTable = ("machine_add_mint_claims", "idempotency_key");
const MACHINE_ADD_JOIN_TOKENS: IndexTable = ("machine_add_join_tokens", "fingerprint");

#[derive(Debug, Clone)]
pub struct OperationRepository {
    store: CoreStore,
    progress: async_nats::Client,
}

impl OperationRepository {
    #[must_use]
    pub fn open(store: CoreStore, progress: async_nats::Client) -> Self {
        Self { store, progress }
    }

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
            .call(move |conn| {
                create_or_adopt(conn, DEPLOY_CLAIMS, key.as_str(), &claim, AdoptPolicy::FirstWriterWins)
            })
            .await
            .map_err(store_status)?;
        let claim = adopted.into_value().map_err(SubmitOperationError::StoreStatus)?;
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
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn submit_machine_update(
        &self,
        submission: MachineUpdateOperationSubmission,
    ) -> Result<AcceptedMachineUpdateSubmission, SubmitOperationError> {
        let payload = MachineUpdatePayload {
            machine_id: submission.machine_id,
            target_version: submission.target_version,
        };
        let submitted = self
            .submit_operation::<MachineUpdateOperationSubmission>(submission.operation_id, payload)
            .await?;
        Ok(AcceptedMachineUpdateSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            machine_id: submitted.payload.machine_id,
            target_version: submitted.payload.target_version,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn submit_machine_lifecycle(
        &self,
        submission: MachineLifecycleOperationSubmission,
    ) -> Result<AcceptedMachineLifecycleSubmission, SubmitOperationError> {
        let payload = MachineLifecyclePayload {
            machine_id: submission.machine_id,
            target: submission.target,
        };
        let submitted = self
            .submit_operation::<MachineLifecycleOperationSubmission>(
                submission.operation_id,
                payload,
            )
            .await?;
        Ok(AcceptedMachineLifecycleSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            machine_id: submitted.payload.machine_id,
            target: submitted.payload.target,
            should_start_execution: submitted.should_start_execution,
        })
    }

    async fn submit_operation<K: SubmitKind>(
        &self,
        operation_id: OperationId,
        payload: K::Payload,
    ) -> Result<SubmittedOperation<K::Payload>, SubmitOperationError> {
        let closure_payload = payload.clone();
        let closure_id = operation_id.clone();
        let outcome = self
            .store
            .call(move |conn| submit_operation_txn::<K>(conn, closure_id, closure_payload))
            .await
            .map_err(store_status)?;
        match outcome {
            SubmitTxn::DuplicateSequenceMismatch { sequence } => {
                Err(SubmitOperationError::DuplicateSequenceMismatch { sequence })
            }
            SubmitTxn::Existing {
                start_sequence,
                payload,
                should_start_execution,
            } => Ok(SubmittedOperation {
                operation_id,
                start_sequence,
                payload,
                should_start_execution,
            }),
            SubmitTxn::Created {
                start_sequence,
                event,
            } => {
                publish_progress(&self.progress, event).await;
                Ok(SubmittedOperation {
                    operation_id,
                    start_sequence,
                    payload,
                    should_start_execution: true,
                })
            }
        }
    }

    pub async fn submit_machine_add(
        &self,
        submission: MachineAddOperationSubmission,
    ) -> Result<AcceptedMachineAddSubmission, SubmitMachineAddError> {
        validate_machine_add_join_material(&submission.raw_join_token, &submission.join_token)?;
        let MachineAddOperationSubmission {
            operation_id,
            machine_id,
            name,
            roles,
            join_bundle,
            join_token,
            raw_join_token,
            idempotency_key,
        } = submission;
        let claim = StoredMachineAddClaim {
            operation_id: operation_id.clone(),
            machine_id,
            name,
            roles,
            join_bundle,
            join_token,
            raw_join_token,
        };
        let outcome = self
            .store
            .call(move |conn| submit_machine_add_txn(conn, operation_id, idempotency_key, claim))
            .await
            .map_err(submit_machine_add_store_status)?;
        match outcome {
            MachineAddTxn::KindMismatch { sequence } => {
                Err(submit_machine_add_duplicate_mismatch(sequence))
            }
            MachineAddTxn::DuplicateIdempotencyKey => {
                Err(SubmitMachineAddError::DuplicateIdempotencyKey)
            }
            MachineAddTxn::JoinTokenMismatch => Err(SubmitMachineAddError::JoinTokenMismatch),
            MachineAddTxn::Conflict(message) => {
                Err(SubmitMachineAddError::Operation(SubmitOperationError::StoreStatus(
                    OperationStatusStoreError::CasConflict {
                        message: message.to_owned(),
                    },
                )))
            }
            MachineAddTxn::Accepted { submitted, event } => {
                if let Some(event) = event {
                    publish_progress(&self.progress, event).await;
                }
                let submitted = *submitted;
                Ok(AcceptedMachineAddSubmission {
                    operation_id: submitted.operation_id,
                    start_sequence: submitted.start_sequence,
                    machine_id: submitted.machine_id,
                    name: submitted.name,
                    roles: submitted.roles,
                    join_bundle: submitted.join_bundle,
                    join_token: submitted.join_token,
                    raw_join_token: submitted.raw_join_token,
                })
            }
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

    pub async fn record_machine_add_joined(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        joined_at: JoinTokenRedeemedAt,
    ) -> Result<OperationStatusWrite, RecordMachineAddEventError> {
        self.record_operation_event(
            operation_id,
            OperationEvent::MachineAddJoined {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                joined_at,
            },
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_machine_add_credential_provisioned(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        step: ployz_core::machine::MachineCredentialProvisioningStep,
    ) -> Result<OperationStatusWrite, RecordMachineAddEventError> {
        self.record_operation_event(
            operation_id,
            OperationEvent::MachineAddCredentialProvisioned {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                step,
            },
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_machine_add_failed(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        failure: MachineAddFailure,
    ) -> Result<OperationStatusWrite, RecordMachineAddEventError> {
        self.record_operation_event(
            operation_id,
            OperationEvent::MachineAddFailed {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                failure,
            },
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_machine_add_completed(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
    ) -> Result<OperationStatusWrite, RecordMachineAddEventError> {
        self.record_operation_event(
            operation_id,
            OperationEvent::MachineAddCompleted {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
            },
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_machine_lifecycle_transition(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        transition: MachineLifecycleTransition,
    ) -> Result<OperationStatusWrite, RecordOperationEventError> {
        self.record_operation_event(operation_id, transition.event(operation_id, machine_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_machine_update_transition(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        transition: MachineUpdateTransition,
    ) -> Result<OperationStatusWrite, RecordOperationEventError> {
        self.record_operation_event(operation_id, transition.event(operation_id, machine_id))
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }

    async fn record_operation_event(
        &self,
        operation_id: &OperationId,
        event: OperationEvent,
    ) -> Result<RecordOperationEventOutcome, RecordOperationEventError> {
        let closure_id = operation_id.clone();
        let outcome = self
            .store
            .call(move |conn| record_operation_event_txn(conn, &closure_id, event))
            .await
            .map_err(|error| RecordOperationEventError::StoreStatus(index_error(&error)))?;
        match outcome {
            RecordTxn::Missing => Err(RecordOperationEventError::MissingOperation {
                operation_id: operation_id.clone(),
            }),
            RecordTxn::Projection(error) => Err(RecordOperationEventError::ProjectStatus(error)),
            RecordTxn::AlreadySatisfied { current_sequence } => {
                Ok(RecordOperationEventOutcome::AlreadySatisfied { current_sequence })
            }
            RecordTxn::Stored { sequence, event } => {
                publish_progress(&self.progress, event).await;
                Ok(RecordOperationEventOutcome::Stored { sequence })
            }
        }
    }

    pub async fn operation_status_snapshot(
        &self,
        operation_id: &OperationId,
    ) -> Option<OperationStatusSnapshot> {
        let operation_id = operation_id.clone();
        // ponytail: a SELECT on the open core DB does not fail in practice; a
        // hard database fault surfaces here as an absent operation.
        self.store
            .call(move |conn| select_status(conn, &operation_id))
            .await
            .ok()
            .flatten()
            .map(OperationStatusSnapshot::new)
    }

    pub async fn operation_statuses(&self) -> Vec<OperationStatus> {
        self.store
            .call(select_all_statuses)
            .await
            .unwrap_or_default()
    }

    pub async fn replay_operation_events(
        &self,
        request: OperationEventReplayRequest,
    ) -> Result<OperationEventReplayPage, ReplayOperationEventsError> {
        let OperationEventReplayRequest {
            operation_id,
            start_sequence,
            limit,
        } = request;
        let closure_id = operation_id.clone();
        let outcome = self
            .store
            .call(move |conn| replay_operation_events_txn(conn, &closure_id, start_sequence, limit))
            .await
            .map_err(|error| ReplayOperationEventsError::ReadEvents(read_event_error(&error)))?;
        match outcome {
            ReplayTxn::Missing => Err(ReplayOperationEventsError::MissingOperation { operation_id }),
            ReplayTxn::Invalid(error) => Err(ReplayOperationEventsError::ReadEvents(error)),
            ReplayTxn::Page(page) => Ok(page),
        }
    }

    pub async fn machine_add_submissions(
        &self,
    ) -> Result<Vec<StoredMachineAddSubmission>, OperationStatusStoreError> {
        self.store
            .call(|conn| select_all_json(conn, MACHINE_ADD_SUBMISSIONS.0))
            .await
            .map_err(|error| index_error(&error))
    }

    pub async fn machine_add_secret_delivery(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredMachineAddSecretDelivery>, OperationStatusStoreError> {
        let key = idempotency_key.clone();
        self.store
            .call(move |conn| select_json(conn, MACHINE_ADD_SECRET_DELIVERIES, key.as_str()))
            .await
            .map_err(|error| index_error(&error))
    }

    pub async fn put_machine_add_mint_claim_if_absent(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        claim: &StoredMachineAddMintClaim,
    ) -> Result<StoredMachineAddMintClaim, OperationStatusStoreError> {
        let key = idempotency_key.clone();
        let claim = claim.clone();
        let adopted = self
            .store
            .call(move |conn| {
                create_or_adopt(
                    conn,
                    MACHINE_ADD_MINT_CLAIMS,
                    key.as_str(),
                    &claim,
                    AdoptPolicy::FirstWriterWins,
                )
            })
            .await
            .map_err(|error| index_error(&error))?;
        adopted.into_value()
    }

    pub async fn put_machine_add_secret_delivery_if_absent(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        secret_delivery: &StoredMachineAddSecretDelivery,
    ) -> Result<StoredMachineAddSecretDelivery, OperationStatusStoreError> {
        let key = idempotency_key.clone();
        let delivery = secret_delivery.clone();
        let adopted = self
            .store
            .call(move |conn| {
                create_or_adopt(
                    conn,
                    MACHINE_ADD_SECRET_DELIVERIES,
                    key.as_str(),
                    &delivery,
                    AdoptPolicy::RequireEqual {
                        conflict_message: "machine add secret delivery is already assigned",
                    },
                )
            })
            .await
            .map_err(|error| index_error(&error))?;
        adopted.into_value()
    }

    pub async fn delete_machine_add_secret_delivery(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<(), OperationStatusStoreError> {
        let key = idempotency_key.clone();
        self.store
            .call(move |conn| delete_json(conn, MACHINE_ADD_SECRET_DELIVERIES, key.as_str()))
            .await
            .map_err(|error| index_error(&error))
    }

    pub async fn redeem_machine_join_token(
        &self,
        token: &RawJoinToken,
        joined_at: JoinTokenRedeemedAt,
    ) -> Result<MachineJoinRedemption, RedeemMachineJoinTokenError> {
        let submission = self
            .machine_add_submission_for_token(token)
            .await
            .map_err(RedeemMachineJoinTokenError::StoreStatus)?
            .ok_or(RedeemMachineJoinTokenError::UnknownJoinToken)?;
        let Some(status) = self.get(&submission.operation_id).await else {
            return Err(RedeemMachineJoinTokenError::MissingOperation {
                operation_id: submission.operation_id,
            });
        };
        let OperationStatus::MachineAdd {
            id,
            machine_id,
            name,
            roles,
            state,
            last_event_sequence,
        } = status
        else {
            return Err(RedeemMachineJoinTokenError::WrongOperationKind {
                operation_id: submission.operation_id,
            });
        };

        match state {
            MachineAddOperationState::Pending { join_token } => {
                let fingerprint = token
                    .fingerprint()
                    .map_err(|_| RedeemMachineJoinTokenError::InvalidJoinToken)?;
                match redeem_pending_join_token(&join_token, &fingerprint, joined_at) {
                    Ok(joined_at) => {
                        let secret_delivery = self
                            .machine_add_secret_delivery(&submission.idempotency_key)
                            .await
                            .map_err(RedeemMachineJoinTokenError::StoreStatus)?
                            .ok_or_else(|| RedeemMachineJoinTokenError::MissingSecretDelivery {
                                operation_id: id.clone(),
                            })?
                            .secret_delivery;
                        self.record_machine_add_joined(&id, &machine_id, joined_at)
                            .await
                            .map_err(RedeemMachineJoinTokenError::RecordMachineAddEvent)?;
                        let Some(OperationStatus::MachineAdd {
                            last_event_sequence,
                            ..
                        }) = self.get(&id).await
                        else {
                            return Err(RedeemMachineJoinTokenError::MissingOperation {
                                operation_id: id,
                            });
                        };
                        Ok(MachineJoinRedemption::Joined(RedeemedMachineJoin {
                            operation_id: id,
                            machine_id,
                            name,
                            roles,
                            join_bundle: submission.join_bundle,
                            secret_delivery,
                            joined_at,
                            last_event_sequence,
                        }))
                    }
                    Err(failure) => {
                        self.record_machine_add_failed(&id, &machine_id, failure.clone())
                            .await
                            .map_err(RedeemMachineJoinTokenError::RecordMachineAddEvent)?;
                        Err(RedeemMachineJoinTokenError::JoinRejected {
                            operation_id: id,
                            failure,
                        })
                    }
                }
            }
            MachineAddOperationState::Joining { joined_at } => {
                let secret_delivery = self
                    .machine_add_secret_delivery(&submission.idempotency_key)
                    .await
                    .map_err(RedeemMachineJoinTokenError::StoreStatus)?
                    .ok_or_else(|| RedeemMachineJoinTokenError::MissingSecretDelivery {
                        operation_id: id.clone(),
                    })?
                    .secret_delivery;
                Ok(MachineJoinRedemption::AlreadyJoined(RedeemedMachineJoin {
                    operation_id: id,
                    machine_id,
                    name,
                    roles,
                    join_bundle: submission.join_bundle,
                    secret_delivery,
                    joined_at,
                    last_event_sequence,
                }))
            }
            state @ (MachineAddOperationState::Completed
            | MachineAddOperationState::Failed { .. }
            | MachineAddOperationState::Cancelled { .. }) => {
                Err(RedeemMachineJoinTokenError::OperationNotPending {
                    operation_id: id,
                    current: state.name(),
                })
            }
        }
    }

    pub async fn record_machine_join_completed(
        &self,
        token: &RawJoinToken,
    ) -> Result<RecordedMachineJoinReport, RecordMachineJoinReportError> {
        let target = self.machine_join_report_target(token).await?;
        let status_write = self
            .record_machine_add_completed(&target.operation_id, &target.machine_id)
            .await
            .map_err(RecordMachineJoinReportError::RecordMachineAddEvent)?;
        self.delete_machine_add_secret_delivery(&target.idempotency_key)
            .await
            .map_err(RecordMachineJoinReportError::StoreStatus)?;
        Ok(RecordedMachineJoinReport {
            operation_id: target.operation_id,
            machine_id: target.machine_id,
            status_write,
        })
    }

    pub async fn record_machine_join_failed(
        &self,
        token: &RawJoinToken,
        failure: MachineAddFailure,
    ) -> Result<RecordedMachineJoinReport, RecordMachineJoinReportError> {
        let target = self.machine_join_report_target(token).await?;
        let status_write = self
            .record_machine_add_failed(&target.operation_id, &target.machine_id, failure)
            .await
            .map_err(RecordMachineJoinReportError::RecordMachineAddEvent)?;
        self.delete_machine_add_secret_delivery(&target.idempotency_key)
            .await
            .map_err(RecordMachineJoinReportError::StoreStatus)?;
        Ok(RecordedMachineJoinReport {
            operation_id: target.operation_id,
            machine_id: target.machine_id,
            status_write,
        })
    }

    async fn machine_join_report_target(
        &self,
        token: &RawJoinToken,
    ) -> Result<MachineJoinReportTarget, RecordMachineJoinReportError> {
        let submission = self
            .machine_add_submission_for_token(token)
            .await
            .map_err(RecordMachineJoinReportError::StoreStatus)?
            .ok_or(RecordMachineJoinReportError::UnknownJoinToken)?;
        Ok(MachineJoinReportTarget {
            operation_id: submission.operation_id,
            idempotency_key: submission.idempotency_key,
            machine_id: submission.machine_id,
        })
    }

    async fn machine_add_submission_for_token(
        &self,
        token: &RawJoinToken,
    ) -> Result<Option<StoredMachineAddSubmission>, OperationStatusStoreError> {
        let fingerprint =
            token
                .fingerprint()
                .map_err(|_| OperationStatusStoreError::CorruptRecord {
                    message: "invalid join token".to_owned(),
                })?;
        let token = token.clone();
        let outcome = self
            .store
            .call(move |conn| submission_for_token_txn(conn, &fingerprint, &token))
            .await
            .map_err(|error| index_error(&error))?;
        outcome.into_result()
    }

    pub async fn get(&self, operation_id: &OperationId) -> Option<OperationStatus> {
        let operation_id = operation_id.clone();
        // ponytail: see operation_status_snapshot — an open-DB read is
        // effectively infallible; a hard fault reads as absence.
        self.store
            .call(move |conn| select_status(conn, &operation_id))
            .await
            .ok()
            .flatten()
    }
}

// -------- transaction bodies (run inside CoreStore::call) --------

enum RecordTxn {
    Missing,
    Projection(StatusProjectionError),
    AlreadySatisfied { current_sequence: EventSequence },
    Stored { sequence: EventSequence, event: OperationEvent },
}

fn record_operation_event_txn(
    conn: &mut Connection,
    operation_id: &OperationId,
    event: OperationEvent,
) -> Result<RecordTxn, rusqlite::Error> {
    let transaction = conn.transaction()?;
    let Some(current) = select_status(&transaction, operation_id)? else {
        return Ok(RecordTxn::Missing);
    };
    // The next sequence is the projection's own: status is authoritative and
    // the transaction holds the write, so the insert lands at this sequence.
    let sequence = current.next_event_sequence();
    // Deploy evidence recorded after a terminal deploy must be fresh.
    if let Some(evidence) = deploy_evidence_from_event(&event)
        && current.is_terminal()
        && let Err(error) = validate_fresh_deploy_evidence(&current, &evidence)
    {
        return Ok(RecordTxn::Projection(error));
    }
    let projection = match project_operation_event(&current, event.clone(), sequence) {
        Ok(projection) => projection,
        Err(error) => return Ok(RecordTxn::Projection(error)),
    };
    let OperationProjection::StatusChanged { status } = projection else {
        return Ok(RecordTxn::AlreadySatisfied {
            current_sequence: current.last_event_sequence(),
        });
    };
    insert_event(&transaction, &event, sequence)?;
    upsert_status(&transaction, operation_id, &status, sequence)?;
    transaction.commit()?;
    Ok(RecordTxn::Stored { sequence, event })
}

enum SubmitTxn<P> {
    DuplicateSequenceMismatch { sequence: EventSequence },
    Existing {
        start_sequence: EventSequence,
        payload: P,
        should_start_execution: bool,
    },
    Created {
        start_sequence: EventSequence,
        event: OperationEvent,
    },
}

fn submit_operation_txn<K: SubmitKind>(
    conn: &mut Connection,
    operation_id: OperationId,
    payload: K::Payload,
) -> Result<SubmitTxn<K::Payload>, rusqlite::Error> {
    let transaction = conn.transaction()?;
    if let Some(current) = select_status(&transaction, &operation_id)? {
        if current.kind() != K::KIND {
            return Ok(SubmitTxn::DuplicateSequenceMismatch {
                sequence: current.last_event_sequence(),
            });
        }
        let Some((first_event, first_sequence)) = select_first_event(&transaction, &operation_id)?
        else {
            return Ok(SubmitTxn::DuplicateSequenceMismatch {
                sequence: first_event_sequence(),
            });
        };
        let Some((stored_operation_id, payload)) = K::submitted_event_parts(first_event) else {
            return Ok(SubmitTxn::DuplicateSequenceMismatch {
                sequence: first_sequence,
            });
        };
        if stored_operation_id != operation_id {
            return Ok(SubmitTxn::DuplicateSequenceMismatch {
                sequence: first_sequence,
            });
        }
        return Ok(SubmitTxn::Existing {
            start_sequence: first_sequence,
            payload,
            should_start_execution: current.last_event_sequence() == first_sequence,
        });
    }
    let sequence = first_event_sequence();
    let event = K::submitted_event(operation_id.clone(), payload.clone());
    let status = K::accepted_status(operation_id.clone(), &payload, sequence);
    insert_event(&transaction, &event, sequence)?;
    upsert_status(&transaction, &operation_id, &status, sequence)?;
    transaction.commit()?;
    Ok(SubmitTxn::Created {
        start_sequence: sequence,
        event,
    })
}

enum MachineAddTxn {
    KindMismatch { sequence: EventSequence },
    DuplicateIdempotencyKey,
    JoinTokenMismatch,
    Conflict(&'static str),
    Accepted {
        submitted: Box<StoredMachineAddSubmission>,
        event: Option<OperationEvent>,
    },
}

fn submit_machine_add_txn(
    conn: &mut Connection,
    operation_id: OperationId,
    idempotency_key: OperationIdempotencyKey,
    claim: StoredMachineAddClaim,
) -> Result<MachineAddTxn, rusqlite::Error> {
    let transaction = conn.transaction()?;
    if let Some(current) = select_status(&transaction, &operation_id)?
        && current.kind() != OperationKind::MachineAdd
    {
        return Ok(MachineAddTxn::KindMismatch {
            sequence: current.last_event_sequence(),
        });
    }
    let submissions: Vec<StoredMachineAddSubmission> =
        select_all_json(&transaction, MACHINE_ADD_SUBMISSIONS.0)?;
    if submissions
        .iter()
        .any(|submitted| submitted.operation_id == operation_id && submitted.idempotency_key != idempotency_key)
    {
        return Ok(MachineAddTxn::DuplicateIdempotencyKey);
    }
    let claim = match create_or_adopt(
        &transaction,
        MACHINE_ADD_CLAIMS,
        idempotency_key.as_str(),
        &claim,
        AdoptPolicy::FirstWriterWins,
    )? {
        AdoptResult::Value(claim) => claim,
        AdoptResult::Conflict(message) => return Ok(MachineAddTxn::Conflict(message)),
    };
    if claim.operation_id != operation_id {
        return Ok(MachineAddTxn::DuplicateIdempotencyKey);
    }
    let Ok(fingerprint) = claim.raw_join_token.fingerprint() else {
        return Ok(MachineAddTxn::JoinTokenMismatch);
    };
    if !claim.join_token.matches(&fingerprint) {
        return Ok(MachineAddTxn::JoinTokenMismatch);
    }
    if let AdoptResult::Conflict(message) = create_or_adopt(
        &transaction,
        MACHINE_ADD_JOIN_TOKENS,
        fingerprint.as_str(),
        &StoredMachineAddJoinToken {
            operation_id: claim.operation_id.clone(),
            idempotency_key: idempotency_key.clone(),
        },
        AdoptPolicy::RequireEqual {
            conflict_message: "join token fingerprint is already assigned",
        },
    )? {
        return Ok(MachineAddTxn::Conflict(message));
    }

    // A prior submission for this operation means the accept already happened;
    // adopt it and skip re-emitting the submitted event.
    if let Some(existing) =
        select_json::<StoredMachineAddSubmission>(&transaction, MACHINE_ADD_SUBMISSIONS, idempotency_key.as_str())?
    {
        transaction.commit()?;
        return Ok(MachineAddTxn::Accepted {
            submitted: Box::new(existing),
            event: None,
        });
    }

    let sequence = first_event_sequence();
    let event = OperationEvent::MachineAddSubmitted {
        operation_id: claim.operation_id.clone(),
        machine_id: claim.machine_id.clone(),
        name: claim.name.clone(),
        roles: claim.roles,
        join_token: claim.join_token.clone(),
    };
    let submitted = StoredMachineAddSubmission {
        operation_id: claim.operation_id.clone(),
        idempotency_key: idempotency_key.clone(),
        start_sequence: sequence,
        machine_id: claim.machine_id.clone(),
        name: claim.name.clone(),
        roles: claim.roles,
        join_bundle: claim.join_bundle.clone(),
        join_token: claim.join_token.clone(),
        raw_join_token: claim.raw_join_token.clone(),
    };
    if let AdoptResult::Conflict(message) = create_or_adopt(
        &transaction,
        MACHINE_ADD_SUBMISSIONS,
        idempotency_key.as_str(),
        &submitted,
        AdoptPolicy::RequireEqual {
            conflict_message: "machine add submission is already assigned",
        },
    )? {
        return Ok(MachineAddTxn::Conflict(message));
    }
    let status = OperationStatus::machine_add_pending(
        submitted.operation_id.clone(),
        submitted.machine_id.clone(),
        submitted.name.clone(),
        submitted.roles,
        submitted.join_token.clone(),
        sequence,
    );
    insert_event(&transaction, &event, sequence)?;
    upsert_status(&transaction, &submitted.operation_id, &status, sequence)?;
    transaction.commit()?;
    Ok(MachineAddTxn::Accepted {
        submitted: Box::new(submitted),
        event: Some(event),
    })
}

enum ReplayTxn {
    Missing,
    Invalid(OperationEventLogError),
    Page(OperationEventReplayPage),
}

fn replay_operation_events_txn(
    conn: &mut Connection,
    operation_id: &OperationId,
    start_sequence: EventSequence,
    limit: OperationEventReplayLimit,
) -> Result<ReplayTxn, rusqlite::Error> {
    let Some(status) = select_status(conn, operation_id)? else {
        return Ok(ReplayTxn::Missing);
    };
    let page_limit = limit.as_usize();
    let mut statement = conn.prepare(
        "SELECT sequence, event_json FROM operation_events
         WHERE operation_id = ?1 AND sequence >= ?2 ORDER BY sequence LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![
            operation_id.as_str(),
            start_sequence.get(),
            page_limit as i64
        ],
        |row| Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?)),
    )?;
    let mut events = Vec::new();
    for row in rows {
        let (sequence, event_json) = row?;
        let sequence = match EventSequence::try_new(sequence) {
            Ok(sequence) => sequence,
            Err(error) => {
                return Ok(ReplayTxn::Invalid(
                    OperationEventLogError::InvalidEventSequence { sequence, error },
                ));
            }
        };
        let event = match serde_json::from_str(&event_json) {
            Ok(event) => event,
            Err(error) => {
                return Ok(ReplayTxn::Invalid(OperationEventLogError::DecodeEvent(error)));
            }
        };
        events.push(ployz_core::ops::ReplayedOperationEvent { sequence, event });
    }
    if events.len() < page_limit {
        return finish_replay_page(OperationEventReplayPage::caught_up(events), status.is_terminal());
    }
    let Some(next) = events.last().and_then(|event| event.sequence.get().checked_add(1)) else {
        return Ok(ReplayTxn::Invalid(
            OperationEventLogError::InvalidNextReplaySequence { sequence: u64::MAX },
        ));
    };
    let next = match EventSequence::try_new(next) {
        Ok(next) => next,
        Err(error) => {
            return Ok(ReplayTxn::Invalid(
                OperationEventLogError::InvalidEventSequence {
                    sequence: next,
                    error,
                },
            ));
        }
    };
    finish_replay_page(
        OperationEventReplayPage::more(events, next),
        status.is_terminal(),
    )
}

fn finish_replay_page(
    page: OperationEventReplayPage,
    terminal: bool,
) -> Result<ReplayTxn, rusqlite::Error> {
    let page = match (page.cursor, terminal) {
        (OperationEventReplayCursor::CaughtUp, true) => {
            OperationEventReplayPage::terminal(page.events)
        }
        (cursor, _) => OperationEventReplayPage {
            events: page.events,
            cursor,
        },
    };
    Ok(ReplayTxn::Page(page))
}

enum TokenSubmission {
    None,
    Corrupt,
    Found(Box<StoredMachineAddSubmission>),
}

impl TokenSubmission {
    fn into_result(
        self,
    ) -> Result<Option<StoredMachineAddSubmission>, OperationStatusStoreError> {
        match self {
            Self::None => Ok(None),
            Self::Found(submission) => Ok(Some(*submission)),
            Self::Corrupt => Err(OperationStatusStoreError::CorruptRecord {
                message: "join token index does not match submission".to_owned(),
            }),
        }
    }
}

fn submission_for_token_txn(
    conn: &mut Connection,
    fingerprint: &JoinTokenFingerprint,
    token: &RawJoinToken,
) -> Result<TokenSubmission, rusqlite::Error> {
    let Some(index): Option<StoredMachineAddJoinToken> =
        select_json(conn, MACHINE_ADD_JOIN_TOKENS, fingerprint.as_str())?
    else {
        return Ok(TokenSubmission::None);
    };
    let Some(submission): Option<StoredMachineAddSubmission> =
        select_json(conn, MACHINE_ADD_SUBMISSIONS, index.idempotency_key.as_str())?
    else {
        return Ok(TokenSubmission::None);
    };
    if submission.operation_id != index.operation_id
        || submission.raw_join_token != *token
        || !submission.join_token.matches(fingerprint)
    {
        return Ok(TokenSubmission::Corrupt);
    }
    Ok(TokenSubmission::Found(Box::new(submission)))
}

// -------- SQLite row helpers --------

fn select_status(
    conn: &Connection,
    operation_id: &OperationId,
) -> Result<Option<OperationStatus>, rusqlite::Error> {
    conn.query_row(
        "SELECT status_json FROM operations WHERE operation_id = ?1",
        [operation_id.as_str()],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|json| from_json(&json))
    .transpose()
}

fn select_all_statuses(conn: &mut Connection) -> Result<Vec<OperationStatus>, rusqlite::Error> {
    let mut statement =
        conn.prepare("SELECT status_json FROM operations ORDER BY operation_id")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut statuses = Vec::new();
    for row in rows {
        statuses.push(from_json(&row?)?);
    }
    Ok(statuses)
}

fn upsert_status(
    conn: &Connection,
    operation_id: &OperationId,
    status: &OperationStatus,
    last_sequence: EventSequence,
) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO operations (operation_id, status_json, last_sequence) VALUES (?1, ?2, ?3)
         ON CONFLICT(operation_id) DO UPDATE SET
             status_json = excluded.status_json, last_sequence = excluded.last_sequence",
        params![operation_id.as_str(), to_json(status)?, last_sequence.get()],
    )?;
    Ok(())
}

fn insert_event(
    conn: &Connection,
    event: &OperationEvent,
    sequence: EventSequence,
) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO operation_events (operation_id, sequence, event_json) VALUES (?1, ?2, ?3)",
        params![event.operation_id().as_str(), sequence.get(), to_json(event)?],
    )?;
    Ok(())
}

fn select_first_event(
    conn: &Connection,
    operation_id: &OperationId,
) -> Result<Option<(OperationEvent, EventSequence)>, rusqlite::Error> {
    let Some((sequence, event_json)) = conn
        .query_row(
            "SELECT sequence, event_json FROM operation_events
             WHERE operation_id = ?1 ORDER BY sequence LIMIT 1",
            [operation_id.as_str()],
            |row| Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let sequence = EventSequence::try_new(sequence).map_err(sequence_conversion)?;
    Ok(Some((from_json(&event_json)?, sequence)))
}

fn select_json<V: DeserializeOwned>(
    conn: &Connection,
    table: IndexTable,
    key: &str,
) -> Result<Option<V>, rusqlite::Error> {
    let (name, key_column) = table;
    conn.query_row(
        &format!("SELECT json FROM {name} WHERE {key_column} = ?1"),
        [key],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|json| from_json(&json))
    .transpose()
}

fn select_all_json<V: DeserializeOwned>(
    conn: &Connection,
    table: &str,
) -> Result<Vec<V>, rusqlite::Error> {
    query_json_list(conn, &format!("SELECT json FROM {table} ORDER BY 1"))
}

fn insert_json<V: Serialize>(
    conn: &Connection,
    table: IndexTable,
    key: &str,
    value: &V,
) -> Result<(), rusqlite::Error> {
    let (name, key_column) = table;
    conn.execute(
        &format!("INSERT INTO {name} ({key_column}, json) VALUES (?1, ?2)"),
        params![key, to_json(value)?],
    )?;
    Ok(())
}

fn delete_json(conn: &Connection, table: IndexTable, key: &str) -> Result<(), rusqlite::Error> {
    let (name, key_column) = table;
    conn.execute(
        &format!("DELETE FROM {name} WHERE {key_column} = ?1"),
        [key],
    )?;
    Ok(())
}

enum AdoptResult<V> {
    Value(V),
    Conflict(&'static str),
}

impl<V> AdoptResult<V> {
    fn into_value(self) -> Result<V, OperationStatusStoreError> {
        match self {
            Self::Value(value) => Ok(value),
            Self::Conflict(message) => Err(OperationStatusStoreError::CasConflict {
                message: message.to_owned(),
            }),
        }
    }
}

fn create_or_adopt<V>(
    conn: &Connection,
    table: IndexTable,
    key: &str,
    value: &V,
    policy: AdoptPolicy,
) -> Result<AdoptResult<V>, rusqlite::Error>
where
    V: Serialize + DeserializeOwned + Clone + PartialEq,
{
    if let Some(existing) = select_json::<V>(conn, table, key)? {
        return Ok(match policy {
            AdoptPolicy::FirstWriterWins => AdoptResult::Value(existing),
            AdoptPolicy::RequireEqual { .. } if existing == *value => AdoptResult::Value(existing),
            AdoptPolicy::RequireEqual { conflict_message } => AdoptResult::Conflict(conflict_message),
        });
    }
    insert_json(conn, table, key, value)?;
    Ok(AdoptResult::Value(value.clone()))
}

fn first_event_sequence() -> EventSequence {
    EventSequence::try_new(1).expect("one is a valid event sequence")
}

fn sequence_conversion(error: ployz_core::ops::EventSequenceError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Integer,
        format!("invalid event sequence: {error}").into(),
    )
}

fn index_error(error: &CoreStoreError) -> OperationStatusStoreError {
    OperationStatusStoreError::Index {
        message: error.to_string(),
    }
}

fn read_event_error(error: &CoreStoreError) -> OperationEventLogError {
    OperationEventLogError::ReadEvent {
        message: error.to_string(),
    }
}

fn store_status(error: CoreStoreError) -> SubmitOperationError {
    SubmitOperationError::StoreStatus(index_error(&error))
}

// -------- submit kinds (pure) --------

trait SubmitKind: Sized {
    type Payload: Clone + Send + 'static;
    const KIND: OperationKind;
    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent;
    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)>;
    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus;
}

impl SubmitKind for DeployOperationSubmission {
    type Payload = ployz_core::deploy::DeployRequest;
    const KIND: OperationKind = OperationKind::Deploy;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::DeploySubmitted {
            operation_id,
            target: payload,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::DeploySubmitted {
            operation_id,
            target,
        } = event
        else {
            return None;
        };
        Some((operation_id, target))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::deploy_accepted(operation_id, payload.status_service_id(), sequence)
    }
}

impl SubmitKind for MachineUpdateOperationSubmission {
    type Payload = MachineUpdatePayload;
    const KIND: OperationKind = OperationKind::MachineUpdate;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::MachineUpdateSubmitted {
            operation_id,
            machine_id: payload.machine_id,
            target_version: payload.target_version,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::MachineUpdateSubmitted {
            operation_id,
            machine_id,
            target_version,
        } = event
        else {
            return None;
        };
        Some((
            operation_id,
            MachineUpdatePayload {
                machine_id,
                target_version,
            },
        ))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::machine_update_accepted(
            operation_id,
            payload.machine_id.clone(),
            payload.target_version.clone(),
            sequence,
        )
    }
}

impl SubmitKind for MachineLifecycleOperationSubmission {
    type Payload = MachineLifecyclePayload;
    const KIND: OperationKind = OperationKind::MachineLifecycle;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::MachineLifecycleSubmitted {
            operation_id,
            machine_id: payload.machine_id,
            target: payload.target,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::MachineLifecycleSubmitted {
            operation_id,
            machine_id,
            target,
        } = event
        else {
            return None;
        };
        Some((operation_id, MachineLifecyclePayload { machine_id, target }))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::machine_lifecycle_accepted(
            operation_id,
            payload.machine_id.clone(),
            payload.target,
            sequence,
        )
    }
}

struct SubmittedOperation<P> {
    operation_id: OperationId,
    start_sequence: EventSequence,
    payload: P,
    should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredDeployClaim {
    pub operation_id: OperationId,
    pub target: ployz_core::deploy::DeployRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredMachineAddSubmission {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub start_sequence: EventSequence,
    pub machine_id: MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    pub join_bundle: MachineJoinBundle,
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredMachineAddClaim {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    pub join_bundle: MachineJoinBundle,
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredMachineAddSecretDelivery {
    pub operation_id: OperationId,
    pub secret_delivery: MachineJoinSecretDelivery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredMachineAddMintClaim {
    pub operation_id: OperationId,
    pub nkey_public: ployz_core::nats_config::NatsUserPublicKey,
    pub nkey_seed: ployz_core::nats_config::NatsUserSeed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredMachineAddJoinToken {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployOperationSubmission {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub target: ployz_core::deploy::DeployRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddOperationSubmission {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    pub join_bundle: MachineJoinBundle,
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
    pub idempotency_key: OperationIdempotencyKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineUpdateOperationSubmission {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub target_version: InstallArtifactVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MachineUpdatePayload {
    machine_id: MachineId,
    target_version: InstallArtifactVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineLifecycleOperationSubmission {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub target: MachineLifecycle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MachineLifecyclePayload {
    machine_id: MachineId,
    target: MachineLifecycle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedDeploySubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub target: ployz_core::deploy::DeployRequest,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedMachineAddSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub machine_id: MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    pub join_bundle: MachineJoinBundle,
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedMachineUpdateSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub machine_id: MachineId,
    pub target_version: InstallArtifactVersion,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedMachineLifecycleSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub machine_id: MachineId,
    pub target: MachineLifecycle,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatusWrite {
    Stored,
    AlreadySatisfied { current_sequence: EventSequence },
}

enum RecordOperationEventOutcome {
    AlreadySatisfied { current_sequence: EventSequence },
    Stored { sequence: EventSequence },
}

impl RecordOperationEventOutcome {
    fn into_status_write(self) -> OperationStatusWrite {
        match self {
            Self::AlreadySatisfied { current_sequence } => {
                OperationStatusWrite::AlreadySatisfied { current_sequence }
            }
            Self::Stored { .. } => OperationStatusWrite::Stored,
        }
    }

    fn sequence(self) -> EventSequence {
        match self {
            Self::AlreadySatisfied { current_sequence } => current_sequence,
            Self::Stored { sequence, .. } => sequence,
        }
    }
}

#[derive(Debug)]
pub enum SubmitOperationError {
    InvalidDeployTarget,
    StoreStatus(OperationStatusStoreError),
    DuplicateSequenceMismatch { sequence: EventSequence },
}

#[derive(Debug)]
pub enum SubmitMachineAddError {
    Operation(SubmitOperationError),
    JoinTokenMismatch,
    DuplicateIdempotencyKey,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordOperationEventError {
    #[error("{0}")]
    StoreStatus(OperationStatusStoreError),
    #[error("operation record corrupt: missing operation {}", .operation_id.as_str())]
    MissingOperation { operation_id: OperationId },
    #[error("operation status projection failed: {0}")]
    ProjectStatus(StatusProjectionError),
}

pub type RecordDeployTransitionError = RecordOperationEventError;
pub type RecordDeployEvidenceError = RecordOperationEventError;
pub type RecordLifecycleEventError = RecordOperationEventError;
pub type RecordMachineAddEventError = RecordLifecycleEventError;

#[derive(Debug, thiserror::Error)]
pub enum OperationStatusStoreError {
    #[error("operation working-record conflict: {message}")]
    CasConflict { message: String },
    #[error("operation working records are corrupt: {message}")]
    CorruptRecord { message: String },
    #[error("operation working records: {message}")]
    Index { message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum OperationEventLogError {
    #[error("decode operation event: {0}")]
    DecodeEvent(serde_json::Error),
    #[error("read operation event: {message}")]
    ReadEvent { message: String },
    #[error("operation event sequence {sequence} is invalid: {error}")]
    InvalidEventSequence {
        sequence: u64,
        error: ployz_core::ops::EventSequenceError,
    },
    #[error("operation replay next sequence {sequence} is invalid")]
    InvalidNextReplaySequence { sequence: u64 },
}

#[derive(Debug)]
pub enum ReplayOperationEventsError {
    ReadEvents(OperationEventLogError),
    MissingOperation { operation_id: OperationId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineJoinRedemption {
    Joined(RedeemedMachineJoin),
    AlreadyJoined(RedeemedMachineJoin),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemedMachineJoin {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    pub join_bundle: MachineJoinBundle,
    pub secret_delivery: MachineJoinSecretDelivery,
    pub joined_at: JoinTokenRedeemedAt,
    pub last_event_sequence: EventSequence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedMachineJoinReport {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub status_write: OperationStatusWrite,
}

struct MachineJoinReportTarget {
    operation_id: OperationId,
    idempotency_key: OperationIdempotencyKey,
    machine_id: MachineId,
}

#[derive(Debug)]
pub enum RedeemMachineJoinTokenError {
    Clock {
        message: String,
    },
    InvalidJoinToken,
    UnknownJoinToken,
    StoreStatus(OperationStatusStoreError),
    RecordMachineAddEvent(RecordMachineAddEventError),
    MissingOperation {
        operation_id: OperationId,
    },
    MissingSecretDelivery {
        operation_id: OperationId,
    },
    WrongOperationKind {
        operation_id: OperationId,
    },
    JoinTokenMismatch {
        operation_id: OperationId,
    },
    OperationNotPending {
        operation_id: OperationId,
        current: MachineAddOperationStateName,
    },
    JoinRejected {
        operation_id: OperationId,
        failure: MachineAddFailure,
    },
}

#[derive(Debug)]
pub enum RecordMachineJoinReportError {
    InvalidJoinToken,
    UnknownJoinToken,
    StoreStatus(OperationStatusStoreError),
    RecordMachineAddEvent(RecordMachineAddEventError),
    JoinTokenMismatch { operation_id: OperationId },
}

#[derive(Clone, Copy)]
enum AdoptPolicy {
    FirstWriterWins,
    RequireEqual { conflict_message: &'static str },
}

fn deploy_evidence_from_event(event: &OperationEvent) -> Option<DeployEvidence> {
    match event {
        OperationEvent::DeployPlanCreated { plan, .. } => {
            Some(DeployEvidence::PlanCreated { plan: plan.clone() })
        }
        OperationEvent::DeployDataplanePrepared { report, .. } => {
            Some(DeployEvidence::DataplanePrepared {
                report: report.clone(),
            })
        }
        OperationEvent::DeployContainerStarted {
            machine_id,
            container_id,
            ..
        } => Some(DeployEvidence::ContainerStarted {
            machine_id: machine_id.clone(),
            container_id: container_id.clone(),
        }),
        OperationEvent::DeployHealthCheckStarted { .. } => Some(DeployEvidence::HealthCheckStarted),
        OperationEvent::DeployCleanupFinished {
            removed, failed, ..
        } => Some(DeployEvidence::CleanupFinished {
            removed: removed.clone(),
            failed: failed.clone(),
        }),
        // Non-evidence deploy transitions and every other operation kind carry
        // no deploy evidence. Enumerated so a new event variant forces a
        // decision here rather than silently falling through.
        OperationEvent::DeploySubmitted { .. }
        | OperationEvent::DeployPlanningStarted { .. }
        | OperationEvent::DeployRunning { .. }
        | OperationEvent::DeployCompleted { .. }
        | OperationEvent::DeployFailed { .. }
        | OperationEvent::CertRenewalSubmitted { .. }
        | OperationEvent::CertChallengePublished { .. }
        | OperationEvent::CertValidationStarted { .. }
        | OperationEvent::CertCompleted { .. }
        | OperationEvent::CertFailed { .. }
        | OperationEvent::MachineAddSubmitted { .. }
        | OperationEvent::MachineAddJoined { .. }
        | OperationEvent::MachineAddCredentialProvisioned { .. }
        | OperationEvent::MachineAddCompleted { .. }
        | OperationEvent::MachineAddFailed { .. }
        | OperationEvent::MachineUpdateSubmitted { .. }
        | OperationEvent::MachineUpdateRunning { .. }
        | OperationEvent::MachineUpdateCompleted { .. }
        | OperationEvent::MachineUpdateFailed { .. }
        | OperationEvent::MachineLifecycleSubmitted { .. }
        | OperationEvent::MachineLifecycleCompleted { .. }
        | OperationEvent::MachineLifecycleFailed { .. }
        | OperationEvent::Cancelled { .. } => None,
    }
}

async fn publish_progress(client: &async_nats::Client, event: OperationEvent) {
    let Ok(payload) = serde_json::to_vec(&event) else {
        return;
    };
    let _ = client.publish(event.subject(), payload.into()).await;
}

fn submit_machine_add_store_status(error: CoreStoreError) -> SubmitMachineAddError {
    SubmitMachineAddError::Operation(SubmitOperationError::StoreStatus(index_error(&error)))
}

const fn submit_machine_add_duplicate_mismatch(sequence: EventSequence) -> SubmitMachineAddError {
    SubmitMachineAddError::Operation(SubmitOperationError::DuplicateSequenceMismatch { sequence })
}

fn validate_machine_add_join_material(
    raw_join_token: &RawJoinToken,
    join_token: &IssuedJoinToken,
) -> Result<JoinTokenFingerprint, SubmitMachineAddError> {
    let fingerprint = raw_join_token
        .fingerprint()
        .map_err(|_| SubmitMachineAddError::JoinTokenMismatch)?;
    if join_token.matches(&fingerprint) {
        Ok(fingerprint)
    } else {
        Err(SubmitMachineAddError::JoinTokenMismatch)
    }
}
