use super::{
    AcceptedMachineAddSubmission, AdoptResult, MachineAddOperationSubmission,
    MachineJoinRedemption, MachineJoinReportTarget, OperationRepository, OperationStatusStoreError,
    OperationStatusWrite, RecordMachineAddEventError, RecordMachineJoinReportError,
    RecordOperationEventOutcome, RecordedMachineJoinReport, RedeemMachineJoinTokenError,
    RedeemedMachineJoin, StoredMachineAddClaim, StoredMachineAddJoinToken,
    StoredMachineAddMintClaim, StoredMachineAddSecretDelivery, StoredMachineAddSubmission,
    SubmitMachineAddError, SubmitOperationError, index_error, insert_event, publish_progress,
    select_status, subject_token_conversion, upsert_status,
};
use crate::core_store::{CoreStoreError, query_json, query_json_list, to_json};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine::{
    IssuedJoinToken, JoinTokenFingerprint, JoinTokenRedeemedAt, MachineAddFailure, RawJoinToken,
    redeem_pending_join_token,
};
use ployz_core::ops::{
    EventSequence, MachineAddOperationState, OperationEvent, OperationIdempotencyKey,
    OperationKind, OperationStatus,
};
use rusqlite::{Connection, OptionalExtension, params};

impl OperationRepository {
    pub async fn submit_machine_add(
        &self,
        submission: MachineAddOperationSubmission,
    ) -> Result<AcceptedMachineAddSubmission, SubmitMachineAddError> {
        validate_machine_add_join_material(
            &submission.identity.raw_join_token,
            &submission.identity.join_token,
        )?;
        let MachineAddOperationSubmission {
            operation_id,
            identity,
            idempotency_key,
        } = submission;
        let claim = StoredMachineAddClaim {
            operation_id: operation_id.clone(),
            identity,
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
            MachineAddTxn::Conflict(message) => Err(SubmitMachineAddError::Operation(
                SubmitOperationError::StoreStatus(OperationStatusStoreError::CasConflict {
                    message: message.to_owned(),
                }),
            )),
            MachineAddTxn::Accepted {
                submitted,
                progress,
            } => {
                if let Some((event, status)) = progress {
                    publish_progress(&self.progress, event, &status).await;
                }
                let submitted = *submitted;
                Ok(AcceptedMachineAddSubmission {
                    operation_id: submitted.operation_id,
                    start_sequence: submitted.start_sequence,
                    identity: submitted.identity,
                })
            }
        }
    }

    pub async fn record_machine_add_joined(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        joined_at: JoinTokenRedeemedAt,
    ) -> Result<OperationStatusWrite, RecordMachineAddEventError> {
        self.record_machine_add_joined_outcome(operation_id, machine_id, joined_at)
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }

    pub(super) async fn record_machine_add_joined_outcome(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        joined_at: JoinTokenRedeemedAt,
    ) -> Result<RecordOperationEventOutcome, RecordMachineAddEventError> {
        self.record_operation_event(
            operation_id,
            OperationEvent::MachineAddJoined {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                joined_at,
            },
        )
        .await
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

    pub async fn machine_add_submissions(
        &self,
    ) -> Result<Vec<StoredMachineAddSubmission>, OperationStatusStoreError> {
        self.store
            .call(select_machine_add_submissions)
            .await
            .map_err(|error| index_error(&error))
    }

    pub async fn machine_add_secret_delivery(
        &self,
        idempotency_key: &ployz_core::ops::OperationIdempotencyKey,
    ) -> Result<Option<StoredMachineAddSecretDelivery>, OperationStatusStoreError> {
        let idempotency_key = idempotency_key.clone();
        self.store
            .call(move |conn| select_machine_add_secret_delivery(conn, &idempotency_key))
            .await
            .map_err(|error| index_error(&error))
    }

    pub async fn put_machine_add_mint_claim_if_absent(
        &self,
        idempotency_key: &ployz_core::ops::OperationIdempotencyKey,
        claim: &StoredMachineAddMintClaim,
    ) -> Result<StoredMachineAddMintClaim, OperationStatusStoreError> {
        let idempotency_key = idempotency_key.clone();
        let claim = claim.clone();
        self.store
            .call(move |conn| put_machine_add_mint_claim_txn(conn, &idempotency_key, &claim))
            .await
            .map_err(|error| index_error(&error))?
            .into_value()
    }

    pub async fn put_machine_add_secret_delivery_if_absent(
        &self,
        idempotency_key: &ployz_core::ops::OperationIdempotencyKey,
        secret_delivery: &StoredMachineAddSecretDelivery,
    ) -> Result<StoredMachineAddSecretDelivery, OperationStatusStoreError> {
        let idempotency_key = idempotency_key.clone();
        let secret_delivery = secret_delivery.clone();
        self.store
            .call(move |conn| {
                put_machine_add_secret_delivery_txn(conn, &idempotency_key, &secret_delivery)
            })
            .await
            .map_err(|error| index_error(&error))?
            .into_value()
    }

    pub async fn machine_add_mint_claim(
        &self,
        idempotency_key: &ployz_core::ops::OperationIdempotencyKey,
    ) -> Result<Option<StoredMachineAddMintClaim>, OperationStatusStoreError> {
        let idempotency_key = idempotency_key.clone();
        self.store
            .call(move |conn| select_machine_add_mint_claim(conn, &idempotency_key))
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
        let Some(OperationStatus::MachineAdd {
            id,
            machine_id,
            name,
            roles,
            state,
            last_event_sequence,
        }) = self
            .get(&submission.operation_id)
            .await
            .map_err(RedeemMachineJoinTokenError::StoreStatus)?
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
                        let outcome = self
                            .record_machine_add_joined_outcome(&id, &machine_id, joined_at)
                            .await
                            .map_err(RedeemMachineJoinTokenError::RecordMachineAddEvent)?;
                        let already_satisfied = matches!(
                            outcome,
                            RecordOperationEventOutcome::AlreadySatisfied { .. }
                        );
                        let OperationStatus::MachineAdd {
                            state,
                            last_event_sequence,
                            ..
                        } = outcome.status()
                        else {
                            return Err(RedeemMachineJoinTokenError::MissingOperation {
                                operation_id: id,
                            });
                        };
                        let MachineAddOperationState::Joining { joined_at } = state else {
                            return Err(RedeemMachineJoinTokenError::MissingOperation {
                                operation_id: id,
                            });
                        };
                        let joined = RedeemedMachineJoin {
                            operation_id: id,
                            machine_id,
                            name,
                            roles,
                            join_bundle: submission.identity.join_bundle,
                            secret_delivery,
                            joined_at: *joined_at,
                            last_event_sequence: *last_event_sequence,
                        };
                        if already_satisfied {
                            Ok(MachineJoinRedemption::AlreadyJoined(joined))
                        } else {
                            Ok(MachineJoinRedemption::Joined(joined))
                        }
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
                    join_bundle: submission.identity.join_bundle,
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

    pub async fn scrub_machine_add_secrets(
        &self,
        operation_id: &OperationId,
    ) -> Result<(), OperationStatusStoreError> {
        let operation_id = operation_id.clone();
        self.store
            .call(move |conn| scrub_machine_add_secret_rows(conn, &operation_id))
            .await
            .map_err(|error| index_error(&error))
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
            machine_id: submission.identity.machine_id,
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
}

// -------- machine-add transaction and row helpers --------

// A transient result enum: built then immediately matched inside one transaction,
// never held in bulk, so the variant-size spread the lint optimizes for is moot here.
#[allow(clippy::large_enum_variant)]
enum MachineAddTxn {
    KindMismatch {
        sequence: EventSequence,
    },
    DuplicateIdempotencyKey,
    JoinTokenMismatch,
    Conflict(&'static str),
    Accepted {
        submitted: Box<StoredMachineAddSubmission>,
        progress: Option<(OperationEvent, OperationStatus)>,
    },
}

fn submit_machine_add_txn(
    conn: &mut Connection,
    operation_id: OperationId,
    idempotency_key: OperationIdempotencyKey,
    claim: StoredMachineAddClaim,
) -> Result<MachineAddTxn, rusqlite::Error> {
    let transaction = conn.transaction()?;
    let current = select_status(&transaction, &operation_id)?;
    if let Some(current) = &current {
        if current.kind() != OperationKind::MachineAdd {
            return Ok(MachineAddTxn::KindMismatch {
                sequence: current.last_event_sequence(),
            });
        }
        if let Some(existing) = select_machine_add_submission(&transaction, &idempotency_key)? {
            // The idempotency key must name the same operation the caller did.
            // A key already bound to a different operation is a duplicate key,
            // not an adopt of this one — returning that operation's join
            // material would answer the wrong request.
            if existing.operation_id != operation_id {
                return Ok(MachineAddTxn::DuplicateIdempotencyKey);
            }
            transaction.commit()?;
            return Ok(MachineAddTxn::Accepted {
                submitted: Box::new(existing),
                progress: None,
            });
        }
        return Ok(MachineAddTxn::DuplicateIdempotencyKey);
    }
    if !claim_machine_add_idempotency(&transaction, &operation_id, &idempotency_key)? {
        return Ok(MachineAddTxn::DuplicateIdempotencyKey);
    }
    let claim = match create_or_adopt_machine_add_claim(&transaction, &idempotency_key, &claim)? {
        AdoptResult::Value(claim) => claim,
        AdoptResult::Conflict(message) => return Ok(MachineAddTxn::Conflict(message)),
    };
    if claim.operation_id != operation_id {
        return Ok(MachineAddTxn::DuplicateIdempotencyKey);
    }
    let Ok(fingerprint) = claim.identity.raw_join_token.fingerprint() else {
        return Ok(MachineAddTxn::JoinTokenMismatch);
    };
    if !claim.identity.join_token.matches(&fingerprint) {
        return Ok(MachineAddTxn::JoinTokenMismatch);
    }
    if let AdoptResult::Conflict(message) = create_or_adopt_machine_add_join_token(
        &transaction,
        &fingerprint,
        &StoredMachineAddJoinToken {
            operation_id: claim.operation_id.clone(),
            idempotency_key: idempotency_key.clone(),
        },
    )? {
        return Ok(MachineAddTxn::Conflict(message));
    }

    let sequence = EventSequence::first();
    let event = OperationEvent::MachineAddSubmitted {
        operation_id: claim.operation_id.clone(),
        machine_id: claim.identity.machine_id.clone(),
        name: claim.identity.name.clone(),
        roles: claim.identity.roles,
        join_token: claim.identity.join_token.clone(),
    };
    let submitted = StoredMachineAddSubmission {
        operation_id: claim.operation_id.clone(),
        idempotency_key: idempotency_key.clone(),
        start_sequence: sequence,
        identity: claim.identity.clone(),
    };
    if let AdoptResult::Conflict(message) =
        create_or_adopt_machine_add_submission(&transaction, &idempotency_key, &submitted)?
    {
        return Ok(MachineAddTxn::Conflict(message));
    }
    let status = OperationStatus::machine_add_pending(
        submitted.operation_id.clone(),
        submitted.identity.machine_id.clone(),
        submitted.identity.name.clone(),
        submitted.identity.roles,
        submitted.identity.join_token.clone(),
        sequence,
    );
    insert_event(&transaction, &event, sequence)?;
    upsert_status(&transaction, &submitted.operation_id, &status)?;
    transaction.commit()?;
    Ok(MachineAddTxn::Accepted {
        submitted: Box::new(submitted),
        progress: Some((event, status)),
    })
}

enum TokenSubmission {
    None,
    Corrupt,
    Found(Box<StoredMachineAddSubmission>),
}

impl TokenSubmission {
    fn into_result(self) -> Result<Option<StoredMachineAddSubmission>, OperationStatusStoreError> {
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
    let Some(index) = select_machine_add_join_token(conn, fingerprint)? else {
        return Ok(TokenSubmission::None);
    };
    let Some(submission) = select_machine_add_submission(conn, &index.idempotency_key)? else {
        return Ok(TokenSubmission::None);
    };
    if submission.operation_id != index.operation_id
        || submission.identity.raw_join_token != *token
        || !submission.identity.join_token.matches(fingerprint)
    {
        return Ok(TokenSubmission::Corrupt);
    }
    Ok(TokenSubmission::Found(Box::new(submission)))
}

fn claim_machine_add_idempotency(
    conn: &Connection,
    operation_id: &OperationId,
    idempotency_key: &OperationIdempotencyKey,
) -> Result<bool, rusqlite::Error> {
    if let Some(owner) = conn
        .query_row(
            "SELECT operation_id FROM machine_add_idempotency WHERE idempotency_key = ?1",
            [idempotency_key.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(owner == operation_id.as_str());
    }
    let operation_claimed_by_other_key: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM machine_add_idempotency
            WHERE operation_id = ?1 AND idempotency_key <> ?2
        )",
        params![operation_id.as_str(), idempotency_key.as_str()],
        |row| row.get(0),
    )?;
    if operation_claimed_by_other_key {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO machine_add_idempotency (idempotency_key, operation_id) VALUES (?1, ?2)",
        params![idempotency_key.as_str(), operation_id.as_str()],
    )?;
    Ok(true)
}

fn create_or_adopt_machine_add_claim(
    conn: &Connection,
    idempotency_key: &OperationIdempotencyKey,
    claim: &StoredMachineAddClaim,
) -> Result<AdoptResult<StoredMachineAddClaim>, rusqlite::Error> {
    if let Some(existing) = select_machine_add_claim(conn, idempotency_key)? {
        return Ok(AdoptResult::Value(existing));
    }
    conn.execute(
        "INSERT INTO machine_add_claims
         (idempotency_key, operation_id, machine_id, raw_join_token, claim_json)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            idempotency_key.as_str(),
            claim.operation_id.as_str(),
            claim.identity.machine_id.as_str(),
            claim.identity.raw_join_token.as_str(),
            to_json(claim)?,
        ],
    )?;
    Ok(AdoptResult::Value(claim.clone()))
}

fn select_machine_add_claim(
    conn: &Connection,
    idempotency_key: &OperationIdempotencyKey,
) -> Result<Option<StoredMachineAddClaim>, rusqlite::Error> {
    query_json(
        conn,
        "SELECT claim_json FROM machine_add_claims WHERE idempotency_key = ?1",
        idempotency_key.as_str(),
    )
}

fn create_or_adopt_machine_add_join_token(
    conn: &Connection,
    fingerprint: &JoinTokenFingerprint,
    index: &StoredMachineAddJoinToken,
) -> Result<AdoptResult<StoredMachineAddJoinToken>, rusqlite::Error> {
    if let Some(existing) = select_machine_add_join_token(conn, fingerprint)? {
        return Ok(if existing == *index {
            AdoptResult::Value(existing)
        } else {
            AdoptResult::Conflict("join token fingerprint is already assigned")
        });
    }
    conn.execute(
        "INSERT INTO machine_add_join_tokens
         (fingerprint, operation_id, idempotency_key)
         VALUES (?1, ?2, ?3)",
        params![
            fingerprint.as_str(),
            index.operation_id.as_str(),
            index.idempotency_key.as_str(),
        ],
    )?;
    Ok(AdoptResult::Value(index.clone()))
}

fn select_machine_add_join_token(
    conn: &Connection,
    fingerprint: &JoinTokenFingerprint,
) -> Result<Option<StoredMachineAddJoinToken>, rusqlite::Error> {
    conn.query_row(
        "SELECT operation_id, idempotency_key
         FROM machine_add_join_tokens WHERE fingerprint = ?1",
        [fingerprint.as_str()],
        |row| {
            Ok(StoredMachineAddJoinToken {
                operation_id: OperationId::try_new(row.get::<_, String>(0)?)
                    .map_err(subject_token_conversion)?,
                idempotency_key: OperationIdempotencyKey::try_new(row.get::<_, String>(1)?)
                    .map_err(subject_token_conversion)?,
            })
        },
    )
    .optional()
}

fn create_or_adopt_machine_add_submission(
    conn: &Connection,
    idempotency_key: &OperationIdempotencyKey,
    submission: &StoredMachineAddSubmission,
) -> Result<AdoptResult<StoredMachineAddSubmission>, rusqlite::Error> {
    if let Some(existing) = select_machine_add_submission(conn, idempotency_key)? {
        return Ok(if existing == *submission {
            AdoptResult::Value(existing)
        } else {
            AdoptResult::Conflict("machine add submission is already assigned")
        });
    }
    conn.execute(
        "INSERT INTO machine_add_submissions
         (idempotency_key, operation_id, machine_id, raw_join_token, submission_json)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            idempotency_key.as_str(),
            submission.operation_id.as_str(),
            submission.identity.machine_id.as_str(),
            submission.identity.raw_join_token.as_str(),
            to_json(submission)?,
        ],
    )?;
    Ok(AdoptResult::Value(submission.clone()))
}

fn select_machine_add_submission(
    conn: &Connection,
    idempotency_key: &OperationIdempotencyKey,
) -> Result<Option<StoredMachineAddSubmission>, rusqlite::Error> {
    query_json(
        conn,
        "SELECT submission_json FROM machine_add_submissions WHERE idempotency_key = ?1",
        idempotency_key.as_str(),
    )
}

fn select_machine_add_submissions(
    conn: &mut Connection,
) -> Result<Vec<StoredMachineAddSubmission>, rusqlite::Error> {
    query_json_list(
        conn,
        "SELECT submission_json FROM machine_add_submissions ORDER BY operation_id",
    )
}

fn put_machine_add_secret_delivery_txn(
    conn: &Connection,
    idempotency_key: &OperationIdempotencyKey,
    delivery: &StoredMachineAddSecretDelivery,
) -> Result<AdoptResult<StoredMachineAddSecretDelivery>, rusqlite::Error> {
    if let Some(existing) = select_machine_add_secret_delivery(conn, idempotency_key)? {
        return Ok(if existing == *delivery {
            AdoptResult::Value(existing)
        } else {
            AdoptResult::Conflict("machine add secret delivery is already assigned")
        });
    }
    conn.execute(
        "INSERT INTO machine_add_secret_deliveries
         (idempotency_key, operation_id, secret_delivery_json)
         VALUES (?1, ?2, ?3)",
        params![
            idempotency_key.as_str(),
            delivery.operation_id.as_str(),
            to_json(delivery)?,
        ],
    )?;
    Ok(AdoptResult::Value(delivery.clone()))
}

fn select_machine_add_secret_delivery(
    conn: &Connection,
    idempotency_key: &OperationIdempotencyKey,
) -> Result<Option<StoredMachineAddSecretDelivery>, rusqlite::Error> {
    query_json(
        conn,
        "SELECT secret_delivery_json FROM machine_add_secret_deliveries WHERE idempotency_key = ?1",
        idempotency_key.as_str(),
    )
}

fn put_machine_add_mint_claim_txn(
    conn: &Connection,
    idempotency_key: &OperationIdempotencyKey,
    claim: &StoredMachineAddMintClaim,
) -> Result<AdoptResult<StoredMachineAddMintClaim>, rusqlite::Error> {
    if let Some(existing) = select_machine_add_mint_claim(conn, idempotency_key)? {
        return Ok(AdoptResult::Value(existing));
    }
    conn.execute(
        "INSERT INTO machine_add_mint_claims
         (idempotency_key, operation_id, nkey_public, mint_claim_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            idempotency_key.as_str(),
            claim.operation_id.as_str(),
            claim.nkey_public.as_str(),
            to_json(claim)?,
        ],
    )?;
    Ok(AdoptResult::Value(claim.clone()))
}

fn select_machine_add_mint_claim(
    conn: &Connection,
    idempotency_key: &OperationIdempotencyKey,
) -> Result<Option<StoredMachineAddMintClaim>, rusqlite::Error> {
    query_json(
        conn,
        "SELECT mint_claim_json FROM machine_add_mint_claims WHERE idempotency_key = ?1",
        idempotency_key.as_str(),
    )
}

pub(super) fn scrub_failed_machine_add_secrets(
    conn: &Connection,
    status: &OperationStatus,
) -> Result<(), rusqlite::Error> {
    let OperationStatus::MachineAdd { id, state, .. } = status else {
        return Ok(());
    };
    if !matches!(
        state,
        MachineAddOperationState::Failed { .. } | MachineAddOperationState::Cancelled { .. }
    ) {
        return Ok(());
    }
    scrub_machine_add_secret_rows(conn, id)
}

fn scrub_machine_add_secret_rows(
    conn: &Connection,
    operation_id: &OperationId,
) -> Result<(), rusqlite::Error> {
    for table in [
        "machine_add_claims",
        "machine_add_submissions",
        "machine_add_join_tokens",
        "machine_add_secret_deliveries",
        "machine_add_mint_claims",
    ] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE operation_id = ?1"),
            [operation_id.as_str()],
        )?;
    }
    Ok(())
}

pub(super) fn duplicate_machine_add_joined(
    current: &OperationStatus,
    event: &OperationEvent,
) -> bool {
    let OperationStatus::MachineAdd {
        id,
        machine_id,
        state: MachineAddOperationState::Joining { .. },
        ..
    } = current
    else {
        return false;
    };
    let OperationEvent::MachineAddJoined {
        operation_id,
        machine_id: joined_machine_id,
        ..
    } = event
    else {
        return false;
    };
    id == operation_id && machine_id == joined_machine_id
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
