//! Core-local operation evidence and working records.

use crate::evidence_file::{read_json_or_default, write_json};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::install::{InstallArtifactVersion, MachineJoinBundle, MachineJoinSecretDelivery};
use ployz_core::machine::{
    IssuedJoinToken, JoinTokenFingerprint, JoinTokenRedeemedAt, MachineAddFailure, MachineName,
    RawJoinToken, redeem_pending_join_token,
};
use ployz_core::ops::{
    DeployEvidence, DeployTransition, EventSequence, EventSequenceError, MachineAddOperationState,
    MachineAddOperationStateName, MachineLifecycleTransition, MachineUpdateTransition,
    OperationEvent, OperationEventReplayCursor, OperationEventReplayPage,
    OperationEventReplayRequest, OperationIdempotencyKey, OperationKind, OperationProjection,
    OperationStatus, OperationStatusSnapshot, ReplayedOperationEvent, StatusProjectionError,
    project_operation_event, validate_fresh_deploy_evidence,
};
use ployz_core::roles::InstallRolePolicy;
use ployz_core::state::MachineLifecycle;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct OperationRepository {
    inner: Arc<Mutex<OperationRepositoryInner>>,
    progress: async_nats::Client,
}

#[derive(Debug)]
struct OperationRepositoryInner {
    dir: PathBuf,
    index: OperationIndex,
    statuses: BTreeMap<OperationId, OperationStatus>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationIndex {
    deploy_claims: BTreeMap<OperationIdempotencyKey, StoredDeployClaim>,
    machine_add_claims: BTreeMap<OperationIdempotencyKey, StoredMachineAddClaim>,
    machine_add_submissions: BTreeMap<OperationIdempotencyKey, StoredMachineAddSubmission>,
    machine_add_secret_deliveries:
        BTreeMap<OperationIdempotencyKey, StoredMachineAddSecretDelivery>,
    machine_add_mint_claims: BTreeMap<OperationIdempotencyKey, StoredMachineAddMintClaim>,
    machine_add_join_tokens: BTreeMap<JoinTokenFingerprint, StoredMachineAddJoinToken>,
}

impl OperationRepository {
    pub fn open(
        dir: impl Into<PathBuf>,
        progress: async_nats::Client,
    ) -> Result<Self, OperationStoreError> {
        let dir = dir.into();
        prepare_operation_dir(&dir)?;
        let mut index = read_json_or_default(&index_path(&dir)).map_err(|error| {
            OperationStoreError::Index {
                message: format!("{error:?}"),
            }
        })?;
        if recover_machine_add_submissions(&dir, &mut index)? {
            write_operation_index(&dir, &index).map_err(|error| OperationStoreError::Index {
                message: format!("{error:?}"),
            })?;
        }
        let statuses = load_statuses(&dir)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(OperationRepositoryInner {
                dir,
                index,
                statuses,
            })),
            progress,
        })
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
        let mut inner = self.inner.lock().await;
        let claim = StoredDeployClaim {
            operation_id,
            target,
        };
        let claim = create_or_adopt(
            &mut inner.index.deploy_claims,
            idempotency_key.clone(),
            claim,
            AdoptPolicy::FirstWriterWins,
        )
        .map_err(SubmitOperationError::StoreStatus)?;
        write_index(&inner).map_err(SubmitOperationError::StoreStatus)?;
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
        let event = K::submitted_event(operation_id.clone(), payload.clone());
        let mut inner = self.inner.lock().await;
        if let Some(current) = inner.statuses.get(&operation_id) {
            if current.kind() != K::KIND {
                return Err(SubmitOperationError::DuplicateSequenceMismatch {
                    sequence: current.last_event_sequence(),
                });
            }
            let (stored_payload, start_sequence) =
                submitted_payload::<K>(&inner.dir, &operation_id)?;
            return Ok(SubmittedOperation {
                operation_id,
                start_sequence,
                payload: stored_payload,
                should_start_execution: current.last_event_sequence() == start_sequence,
            });
        }

        let sequence =
            append_event(&inner.dir, &event).map_err(SubmitOperationError::AppendEvent)?;
        let status = K::accepted_status(operation_id.clone(), &payload, sequence);
        inner.statuses.insert(operation_id.clone(), status);
        drop(inner);
        publish_progress(&self.progress, event).await;
        Ok(SubmittedOperation {
            operation_id,
            start_sequence: sequence,
            payload,
            should_start_execution: true,
        })
    }

    pub async fn submit_machine_add(
        &self,
        submission: MachineAddOperationSubmission,
    ) -> Result<AcceptedMachineAddSubmission, SubmitMachineAddError> {
        validate_machine_add_join_material(&submission.raw_join_token, &submission.join_token)?;
        let requested_operation_id = submission.operation_id.clone();
        let idempotency_key = submission.idempotency_key;
        let claim = StoredMachineAddClaim {
            operation_id: submission.operation_id,
            machine_id: submission.machine_id,
            name: submission.name,
            roles: submission.roles,
            join_bundle: submission.join_bundle,
            join_token: submission.join_token,
            raw_join_token: submission.raw_join_token,
        };
        let mut inner = self.inner.lock().await;
        if let Some(current) = inner.statuses.get(&requested_operation_id)
            && current.kind() != OperationKind::MachineAdd
        {
            return Err(submit_machine_add_duplicate_mismatch(
                current.last_event_sequence(),
            ));
        }
        let claim = create_or_adopt(
            &mut inner.index.machine_add_claims,
            idempotency_key.clone(),
            claim,
            AdoptPolicy::FirstWriterWins,
        )
        .map_err(submit_machine_add_store_status)?;
        if claim.operation_id != requested_operation_id {
            return Err(SubmitMachineAddError::DuplicateIdempotencyKey);
        }
        let fingerprint =
            validate_machine_add_join_material(&claim.raw_join_token, &claim.join_token)?;
        create_or_adopt(
            &mut inner.index.machine_add_join_tokens,
            fingerprint,
            StoredMachineAddJoinToken {
                operation_id: claim.operation_id.clone(),
                idempotency_key: idempotency_key.clone(),
            },
            AdoptPolicy::RequireEqual {
                conflict_message: "join token fingerprint is already assigned",
            },
        )
        .map_err(submit_machine_add_store_status)?;
        write_index(&inner).map_err(submit_machine_add_store_status)?;

        if let Some(current) = inner.statuses.get(&claim.operation_id) {
            let submitted = inner
                .index
                .machine_add_submissions
                .get(&idempotency_key)
                .cloned()
                .ok_or_else(|| {
                    submit_machine_add_duplicate_mismatch(current.last_event_sequence())
                })?;
            return Ok(AcceptedMachineAddSubmission {
                operation_id: submitted.operation_id,
                start_sequence: submitted.start_sequence,
                machine_id: submitted.machine_id,
                name: submitted.name,
                roles: submitted.roles,
                join_bundle: submitted.join_bundle,
                join_token: submitted.join_token,
                raw_join_token: submitted.raw_join_token,
            });
        }

        let event = OperationEvent::MachineAddSubmitted {
            operation_id: claim.operation_id.clone(),
            machine_id: claim.machine_id.clone(),
            name: claim.name.clone(),
            roles: claim.roles,
            join_token: claim.join_token.clone(),
        };
        let sequence = append_event(&inner.dir, &event).map_err(submit_machine_add_append_event)?;
        let submitted = StoredMachineAddSubmission {
            operation_id: claim.operation_id.clone(),
            idempotency_key: idempotency_key.clone(),
            start_sequence: sequence,
            machine_id: claim.machine_id,
            name: claim.name,
            roles: claim.roles,
            join_bundle: claim.join_bundle,
            join_token: claim.join_token,
            raw_join_token: claim.raw_join_token,
        };
        create_or_adopt(
            &mut inner.index.machine_add_submissions,
            idempotency_key,
            submitted.clone(),
            AdoptPolicy::RequireEqual {
                conflict_message: "machine add submission is already assigned",
            },
        )
        .map_err(submit_machine_add_store_status)?;
        inner.statuses.insert(
            submitted.operation_id.clone(),
            OperationStatus::machine_add_pending(
                submitted.operation_id.clone(),
                submitted.machine_id.clone(),
                submitted.name.clone(),
                submitted.roles,
                submitted.join_token.clone(),
                sequence,
            ),
        );
        write_index(&inner).map_err(submit_machine_add_store_status)?;
        drop(inner);
        publish_progress(&self.progress, event).await;
        Ok(AcceptedMachineAddSubmission {
            operation_id: submitted.operation_id,
            start_sequence: sequence,
            machine_id: submitted.machine_id,
            name: submitted.name,
            roles: submitted.roles,
            join_bundle: submitted.join_bundle,
            join_token: submitted.join_token,
            raw_join_token: submitted.raw_join_token,
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
        self.record_deploy_event(operation_id, evidence.event(operation_id))
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

    async fn record_deploy_event(
        &self,
        operation_id: &OperationId,
        event: OperationEvent,
    ) -> Result<RecordOperationEventOutcome, RecordOperationEventError> {
        let evidence = deploy_evidence_from_event(&event);
        if let Some(evidence) = &evidence {
            let inner = self.inner.lock().await;
            let Some(current) = inner.statuses.get(operation_id) else {
                return Err(RecordOperationEventError::MissingOperation {
                    operation_id: operation_id.clone(),
                });
            };
            if current.is_terminal() {
                validate_fresh_deploy_evidence(current, evidence)
                    .map_err(RecordOperationEventError::ProjectStatus)?;
            }
        }
        self.record_operation_event(operation_id, event).await
    }

    async fn record_operation_event(
        &self,
        operation_id: &OperationId,
        event: OperationEvent,
    ) -> Result<RecordOperationEventOutcome, RecordOperationEventError> {
        let mut inner = self.inner.lock().await;
        let Some(current) = inner.statuses.get(operation_id).cloned() else {
            return Err(RecordOperationEventError::MissingOperation {
                operation_id: operation_id.clone(),
            });
        };
        let sequence = next_sequence(&inner.dir, operation_id)
            .map_err(RecordOperationEventError::AppendEvent)?;
        let projection = project_operation_event(&current, event.clone(), sequence)
            .map_err(RecordOperationEventError::ProjectStatus)?;
        let OperationProjection::StatusChanged { status } = projection else {
            return Ok(RecordOperationEventOutcome::AlreadySatisfied {
                current_sequence: current.last_event_sequence(),
            });
        };
        let sequence =
            append_event(&inner.dir, &event).map_err(RecordOperationEventError::AppendEvent)?;
        inner.statuses.insert(operation_id.clone(), *status);
        drop(inner);
        publish_progress(&self.progress, event).await;
        Ok(RecordOperationEventOutcome::Stored {
            sequence,
            status_write: OperationStatusWrite::Stored,
        })
    }

    pub async fn operation_status_snapshot(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationStatusSnapshot>, OperationStatusReadError> {
        let inner = self.inner.lock().await;
        Ok(inner
            .statuses
            .get(operation_id)
            .cloned()
            .map(OperationStatusSnapshot::new))
    }

    pub async fn operation_statuses(
        &self,
    ) -> Result<Vec<OperationStatus>, OperationStatusReadError> {
        let inner = self.inner.lock().await;
        Ok(inner.statuses.values().cloned().collect())
    }

    pub async fn replay_operation_events(
        &self,
        request: OperationEventReplayRequest,
    ) -> Result<OperationEventReplayPage, ReplayOperationEventsError> {
        let inner = self.inner.lock().await;
        let Some(status) = inner.statuses.get(&request.operation_id) else {
            return Err(ReplayOperationEventsError::MissingOperation {
                operation_id: request.operation_id,
            });
        };
        let page = replay_operation_events_from_file(
            &inner.dir,
            &request.operation_id,
            request.start_sequence,
            request.limit,
        )
        .map_err(ReplayOperationEventsError::ReadEvents)?;
        match (page.cursor, status.is_terminal()) {
            (OperationEventReplayCursor::CaughtUp, true) => {
                Ok(OperationEventReplayPage::terminal(page.events))
            }
            (cursor, _) => Ok(OperationEventReplayPage {
                events: page.events,
                cursor,
            }),
        }
    }

    pub async fn machine_add_submissions(
        &self,
    ) -> Result<Vec<StoredMachineAddSubmission>, OperationStatusStoreError> {
        let inner = self.inner.lock().await;
        Ok(inner
            .index
            .machine_add_submissions
            .values()
            .cloned()
            .collect())
    }

    pub async fn machine_add_secret_delivery(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredMachineAddSecretDelivery>, OperationStatusStoreError> {
        let inner = self.inner.lock().await;
        Ok(inner
            .index
            .machine_add_secret_deliveries
            .get(idempotency_key)
            .cloned())
    }

    pub async fn put_machine_add_mint_claim_if_absent(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        claim: &StoredMachineAddMintClaim,
    ) -> Result<StoredMachineAddMintClaim, OperationStatusStoreError> {
        let mut inner = self.inner.lock().await;
        let claim = create_or_adopt(
            &mut inner.index.machine_add_mint_claims,
            idempotency_key.clone(),
            claim.clone(),
            AdoptPolicy::FirstWriterWins,
        )?;
        write_index(&inner)?;
        Ok(claim)
    }

    pub async fn put_machine_add_secret_delivery_if_absent(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        secret_delivery: &StoredMachineAddSecretDelivery,
    ) -> Result<StoredMachineAddSecretDelivery, OperationStatusStoreError> {
        let mut inner = self.inner.lock().await;
        let delivery = create_or_adopt(
            &mut inner.index.machine_add_secret_deliveries,
            idempotency_key.clone(),
            secret_delivery.clone(),
            AdoptPolicy::RequireEqual {
                conflict_message: "machine add secret delivery is already assigned",
            },
        )?;
        write_index(&inner)?;
        Ok(delivery)
    }

    pub async fn delete_machine_add_secret_delivery(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<(), OperationStatusStoreError> {
        let mut inner = self.inner.lock().await;
        inner
            .index
            .machine_add_secret_deliveries
            .remove(idempotency_key);
        write_index(&inner)
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
        let Some(status) = self
            .get(&submission.operation_id)
            .await
            .map_err(RedeemMachineJoinTokenError::LoadStatus)?
        else {
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
            state => Err(RedeemMachineJoinTokenError::OperationNotPending {
                operation_id: id,
                current: state.name(),
            }),
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
        let inner = self.inner.lock().await;
        let Some(index) = inner.index.machine_add_join_tokens.get(&fingerprint) else {
            return Ok(None);
        };
        let Some(submission) = inner
            .index
            .machine_add_submissions
            .get(&index.idempotency_key)
            .cloned()
        else {
            return Ok(None);
        };
        if submission.operation_id != index.operation_id
            || submission.raw_join_token != *token
            || !submission.join_token.matches(&fingerprint)
        {
            return Err(OperationStatusStoreError::CorruptRecord {
                message: "join token index does not match submission".to_owned(),
            });
        }
        Ok(Some(submission))
    }

    pub async fn get(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationStatus>, OperationStatusReadError> {
        let inner = self.inner.lock().await;
        Ok(inner.statuses.get(operation_id).cloned())
    }
}

trait SubmitKind: Sized {
    type Payload: Clone;
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredDeployClaim {
    pub operation_id: OperationId,
    pub target: ployz_core::deploy::DeployRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredMachineAddSecretDelivery {
    pub operation_id: OperationId,
    pub secret_delivery: MachineJoinSecretDelivery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredMachineAddMintClaim {
    pub operation_id: OperationId,
    pub nkey_public: ployz_core::nats_config::NatsUserPublicKey,
    pub nkey_seed: ployz_core::nats_config::NatsUserSeed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    AlreadySatisfied {
        current_sequence: EventSequence,
    },
    Stored {
        sequence: EventSequence,
        status_write: OperationStatusWrite,
    },
}

impl RecordOperationEventOutcome {
    fn into_status_write(self) -> OperationStatusWrite {
        match self {
            Self::AlreadySatisfied { current_sequence } => {
                OperationStatusWrite::AlreadySatisfied { current_sequence }
            }
            Self::Stored { status_write, .. } => status_write,
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
    AppendEvent(OperationEventLogError),
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
    LoadStatus(OperationStatusReadError),
    #[error("{0}")]
    StoreStatus(OperationStatusStoreError),
    #[error("operation record corrupt: missing operation {}", .operation_id.as_str())]
    MissingOperation { operation_id: OperationId },
    #[error("operation status projection failed: {0}")]
    ProjectStatus(StatusProjectionError),
    #[error("{0}")]
    AppendEvent(OperationEventLogError),
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
pub enum OperationStatusReadError {
    #[error("operation status read failed: {message}")]
    Read { message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum OperationStoreError {
    #[error("operation evidence directory: {message}")]
    Directory { message: String },
    #[error("operation working records: {message}")]
    Index { message: String },
    #[error("{0}")]
    EventLog(OperationEventLogError),
    #[error("{0}")]
    ProjectStatus(StatusProjectionError),
}

#[derive(Debug, thiserror::Error)]
pub enum OperationEventLogError {
    #[error("encode operation event: {0}")]
    EncodeEvent(serde_json::Error),
    #[error("decode operation event: {0}")]
    DecodeEvent(serde_json::Error),
    #[error("read operation event: {message}")]
    ReadEvent { message: String },
    #[error("write operation event: {message}")]
    WriteEvent { message: String },
    #[error("operation event sequence {sequence} is invalid: {error}")]
    InvalidEventSequence {
        sequence: u64,
        error: EventSequenceError,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum OperationEventReplayReadError {
    #[error("decode operation event: {0}")]
    DecodeEvent(serde_json::Error),
    #[error("read operation event: {message}")]
    ReadEvent { message: String },
    #[error("operation event sequence {sequence} is invalid: {error}")]
    InvalidEventSequence {
        sequence: u64,
        error: EventSequenceError,
    },
    #[error("operation replay next sequence {sequence} is invalid")]
    InvalidNextReplaySequence { sequence: u64 },
}

#[derive(Debug)]
pub enum ReplayOperationEventsError {
    LoadStatus(OperationStatusReadError),
    ReadEvents(OperationEventReplayReadError),
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
    LoadStatus(OperationStatusReadError),
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

fn create_or_adopt<K, V>(
    records: &mut BTreeMap<K, V>,
    key: K,
    value: V,
    policy: AdoptPolicy,
) -> Result<V, OperationStatusStoreError>
where
    K: Ord,
    V: Clone + PartialEq,
{
    let Some(existing) = records.get(&key) else {
        records.insert(key, value.clone());
        return Ok(value);
    };
    match policy {
        AdoptPolicy::FirstWriterWins => Ok(existing.clone()),
        AdoptPolicy::RequireEqual {
            conflict_message: _,
        } if *existing == value => Ok(existing.clone()),
        AdoptPolicy::RequireEqual { conflict_message } => {
            Err(OperationStatusStoreError::CasConflict {
                message: conflict_message.to_owned(),
            })
        }
    }
}

fn write_index(inner: &OperationRepositoryInner) -> Result<(), OperationStatusStoreError> {
    write_operation_index(&inner.dir, &inner.index).map_err(|error| {
        OperationStatusStoreError::Index {
            message: format!("{error:?}"),
        }
    })
}

fn write_operation_index(
    dir: &Path,
    index: &OperationIndex,
) -> Result<(), crate::evidence_file::EvidenceFileError> {
    write_json(&index_path(dir), index)
}

fn index_path(dir: &Path) -> PathBuf {
    dir.join("working-records.json")
}

fn prepare_operation_dir(dir: &Path) -> Result<(), OperationStoreError> {
    std::fs::create_dir_all(dir).map_err(|error| OperationStoreError::Directory {
        message: error.to_string(),
    })?;
    set_dir_permissions(dir).map_err(|error| OperationStoreError::Directory {
        message: error.to_string(),
    })
}

#[cfg(unix)]
fn set_dir_permissions(dir: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_dir_permissions(_dir: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

fn operation_file(dir: &Path, operation_id: &OperationId) -> PathBuf {
    dir.join(format!("{}.jsonl", operation_id.as_str()))
}

fn recover_machine_add_submissions(
    dir: &Path,
    index: &mut OperationIndex,
) -> Result<bool, OperationStoreError> {
    let mut changed = false;
    let claims_by_operation = index
        .machine_add_claims
        .iter()
        .map(|(idempotency_key, claim)| {
            (
                claim.operation_id.clone(),
                (idempotency_key.clone(), claim.clone()),
            )
        })
        .collect::<BTreeMap<_, _>>();

    for entry in std::fs::read_dir(dir).map_err(|error| OperationStoreError::Directory {
        message: error.to_string(),
    })? {
        let entry = entry.map_err(|error| OperationStoreError::Directory {
            message: error.to_string(),
        })?;
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        for replayed in
            read_operation_events(&entry.path()).map_err(OperationStoreError::EventLog)?
        {
            let OperationEvent::MachineAddSubmitted {
                operation_id,
                machine_id,
                name,
                roles,
                join_token,
            } = replayed.event
            else {
                continue;
            };
            let Some((idempotency_key, claim)) = claims_by_operation.get(&operation_id).cloned()
            else {
                continue;
            };
            if claim.machine_id != machine_id
                || claim.name != name
                || claim.roles != roles
                || claim.join_token != join_token
            {
                return Err(OperationStoreError::Index {
                    message: format!(
                        "machine-add claim does not match submitted event for {}",
                        operation_id.as_str()
                    ),
                });
            }
            if !index.machine_add_submissions.contains_key(&idempotency_key) {
                index.machine_add_submissions.insert(
                    idempotency_key.clone(),
                    StoredMachineAddSubmission {
                        operation_id: operation_id.clone(),
                        idempotency_key: idempotency_key.clone(),
                        start_sequence: replayed.sequence,
                        machine_id: claim.machine_id.clone(),
                        name: claim.name.clone(),
                        roles: claim.roles,
                        join_bundle: claim.join_bundle.clone(),
                        join_token: claim.join_token.clone(),
                        raw_join_token: claim.raw_join_token.clone(),
                    },
                );
                changed = true;
            }
            let fingerprint =
                claim
                    .raw_join_token
                    .fingerprint()
                    .map_err(|error| OperationStoreError::Index {
                        message: format!("machine-add raw join token is invalid: {error}"),
                    })?;
            if !claim.join_token.matches(&fingerprint) {
                return Err(OperationStoreError::Index {
                    message: format!(
                        "machine-add join token does not match raw token for {}",
                        operation_id.as_str()
                    ),
                });
            }
            let join_token_entry = StoredMachineAddJoinToken {
                operation_id,
                idempotency_key: idempotency_key.clone(),
            };
            match index.machine_add_join_tokens.get(&fingerprint) {
                Some(existing) if existing == &join_token_entry => {}
                Some(_) => {
                    return Err(OperationStoreError::Index {
                        message: "join token fingerprint is already assigned".to_owned(),
                    });
                }
                None => {
                    index
                        .machine_add_join_tokens
                        .insert(fingerprint, join_token_entry);
                    changed = true;
                }
            }
        }
    }

    Ok(changed)
}

fn load_statuses(
    dir: &Path,
) -> Result<BTreeMap<OperationId, OperationStatus>, OperationStoreError> {
    let mut statuses = BTreeMap::new();
    for entry in std::fs::read_dir(dir).map_err(|error| OperationStoreError::Directory {
        message: error.to_string(),
    })? {
        let entry = entry.map_err(|error| OperationStoreError::Directory {
            message: error.to_string(),
        })?;
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        for replayed in
            read_operation_events(&entry.path()).map_err(OperationStoreError::EventLog)?
        {
            let operation_id = replayed.event.operation_id().clone();
            match statuses.get(&operation_id) {
                Some(current) => {
                    match project_operation_event(current, replayed.event, replayed.sequence)
                        .map_err(OperationStoreError::ProjectStatus)?
                    {
                        OperationProjection::StatusChanged { status } => {
                            statuses.insert(operation_id, *status);
                        }
                        OperationProjection::AlreadySatisfied => {}
                    }
                }
                None => {
                    statuses.insert(
                        operation_id.clone(),
                        accepted_status_from_submitted_event(replayed.event, replayed.sequence)?,
                    );
                }
            }
        }
    }
    Ok(statuses)
}

fn accepted_status_from_submitted_event(
    event: OperationEvent,
    sequence: EventSequence,
) -> Result<OperationStatus, OperationStoreError> {
    match event {
        OperationEvent::DeploySubmitted {
            operation_id,
            target,
        } => Ok(OperationStatus::deploy_accepted(
            operation_id,
            target.status_service_id(),
            sequence,
        )),
        OperationEvent::MachineAddSubmitted {
            operation_id,
            machine_id,
            name,
            roles,
            join_token,
        } => Ok(OperationStatus::machine_add_pending(
            operation_id,
            machine_id,
            name,
            roles,
            join_token,
            sequence,
        )),
        OperationEvent::MachineUpdateSubmitted {
            operation_id,
            machine_id,
            target_version,
        } => Ok(OperationStatus::machine_update_accepted(
            operation_id,
            machine_id,
            target_version,
            sequence,
        )),
        OperationEvent::MachineLifecycleSubmitted {
            operation_id,
            machine_id,
            target,
        } => Ok(OperationStatus::machine_lifecycle_accepted(
            operation_id,
            machine_id,
            target,
            sequence,
        )),
        event => Err(OperationStoreError::Directory {
            message: format!(
                "operation {} evidence starts with non-submitted event",
                event.operation_id().as_str()
            ),
        }),
    }
}

fn read_operation_events(
    path: &Path,
) -> Result<Vec<ReplayedOperationEvent>, OperationEventLogError> {
    let file = File::open(path).map_err(|error| OperationEventLogError::ReadEvent {
        message: error.to_string(),
    })?;
    std::io::BufReader::new(file)
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let payload = line.map_err(|error| OperationEventLogError::ReadEvent {
                message: error.to_string(),
            })?;
            let sequence = event_sequence_from_index(index)?;
            let event =
                serde_json::from_str(&payload).map_err(OperationEventLogError::DecodeEvent)?;
            Ok(ReplayedOperationEvent { sequence, event })
        })
        .collect()
}

fn event_sequence_from_index(index: usize) -> Result<EventSequence, OperationEventLogError> {
    let sequence = u64::try_from(index)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(OperationEventLogError::InvalidEventSequence {
            sequence: u64::MAX,
            error: EventSequenceError::Zero,
        })?;
    EventSequence::try_new(sequence)
        .map_err(|error| OperationEventLogError::InvalidEventSequence { sequence, error })
}

fn next_sequence(
    dir: &Path,
    operation_id: &OperationId,
) -> Result<EventSequence, OperationEventLogError> {
    let path = operation_file(dir, operation_id);
    let event_count = match read_operation_events(&path) {
        Ok(events) => events.len(),
        Err(OperationEventLogError::ReadEvent { .. }) if !path.exists() => 0,
        Err(error) => return Err(error),
    };
    event_sequence_from_index(event_count)
}

fn append_event(
    dir: &Path,
    event: &OperationEvent,
) -> Result<EventSequence, OperationEventLogError> {
    prepare_event_dir(dir)?;
    let sequence = next_sequence(dir, event.operation_id())?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(operation_file(dir, event.operation_id()))
        .map_err(|error| OperationEventLogError::WriteEvent {
            message: error.to_string(),
        })?;
    set_file_permissions(&file).map_err(|error| OperationEventLogError::WriteEvent {
        message: error.to_string(),
    })?;
    serde_json::to_writer(&mut file, event).map_err(OperationEventLogError::EncodeEvent)?;
    file.write_all(b"\n")
        .and_then(|()| file.sync_all())
        .map_err(|error| OperationEventLogError::WriteEvent {
            message: error.to_string(),
        })?;
    sync_parent_directory(dir).map_err(|error| OperationEventLogError::WriteEvent {
        message: error.to_string(),
    })?;
    Ok(sequence)
}

fn prepare_event_dir(dir: &Path) -> Result<(), OperationEventLogError> {
    std::fs::create_dir_all(dir).map_err(|error| OperationEventLogError::WriteEvent {
        message: error.to_string(),
    })?;
    set_dir_permissions(dir).map_err(|error| OperationEventLogError::WriteEvent {
        message: error.to_string(),
    })
}

#[cfg(unix)]
fn set_file_permissions(file: &File) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_file_permissions(_file: &File) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), std::io::Error> {
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

fn replay_operation_events_from_file(
    dir: &Path,
    operation_id: &OperationId,
    start_sequence: EventSequence,
    limit: ployz_core::ops::OperationEventReplayLimit,
) -> Result<OperationEventReplayPage, OperationEventReplayReadError> {
    let events = read_operation_events_for_replay(&operation_file(dir, operation_id))?;
    let start = usize::try_from(start_sequence.get().saturating_sub(1)).unwrap_or(usize::MAX);
    let limit = limit.as_usize();
    let page = events
        .into_iter()
        .skip(start)
        .take(limit)
        .collect::<Vec<_>>();
    if page.len() < limit {
        return Ok(OperationEventReplayPage::caught_up(page));
    }
    let next = page
        .last()
        .and_then(|event| event.sequence.get().checked_add(1))
        .ok_or(OperationEventReplayReadError::InvalidNextReplaySequence { sequence: u64::MAX })?;
    Ok(OperationEventReplayPage::more(
        page,
        EventSequence::try_new(next).map_err(|error| {
            OperationEventReplayReadError::InvalidEventSequence {
                sequence: next,
                error,
            }
        })?,
    ))
}

fn read_operation_events_for_replay(
    path: &Path,
) -> Result<Vec<ReplayedOperationEvent>, OperationEventReplayReadError> {
    let file = File::open(path).map_err(|error| OperationEventReplayReadError::ReadEvent {
        message: error.to_string(),
    })?;
    std::io::BufReader::new(file)
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let payload = line.map_err(|error| OperationEventReplayReadError::ReadEvent {
                message: error.to_string(),
            })?;
            let sequence = replay_sequence_from_index(index)?;
            let event = serde_json::from_str(&payload)
                .map_err(OperationEventReplayReadError::DecodeEvent)?;
            Ok(ReplayedOperationEvent { sequence, event })
        })
        .collect()
}

fn replay_sequence_from_index(
    index: usize,
) -> Result<EventSequence, OperationEventReplayReadError> {
    let sequence = u64::try_from(index)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(OperationEventReplayReadError::InvalidEventSequence {
            sequence: u64::MAX,
            error: EventSequenceError::Zero,
        })?;
    EventSequence::try_new(sequence)
        .map_err(|error| OperationEventReplayReadError::InvalidEventSequence { sequence, error })
}

fn submitted_payload<K: SubmitKind>(
    dir: &Path,
    operation_id: &OperationId,
) -> Result<(K::Payload, EventSequence), SubmitOperationError> {
    let events = read_operation_events(&operation_file(dir, operation_id))
        .map_err(SubmitOperationError::AppendEvent)?;
    let Some(first) = events.into_iter().next() else {
        return Err(SubmitOperationError::DuplicateSequenceMismatch {
            sequence: EventSequence::try_new(1).expect("one is a valid event sequence"),
        });
    };
    let Some((stored_operation_id, payload)) = K::submitted_event_parts(first.event) else {
        return Err(SubmitOperationError::DuplicateSequenceMismatch {
            sequence: first.sequence,
        });
    };
    if &stored_operation_id != operation_id {
        return Err(SubmitOperationError::DuplicateSequenceMismatch {
            sequence: first.sequence,
        });
    }
    Ok((payload, first.sequence))
}

async fn publish_progress(client: &async_nats::Client, event: OperationEvent) {
    let Ok(payload) = serde_json::to_vec(&event) else {
        return;
    };
    let _ = client.publish(event.subject(), payload.into()).await;
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
        _ => None,
    }
}

fn submit_machine_add_store_status(error: OperationStatusStoreError) -> SubmitMachineAddError {
    SubmitMachineAddError::Operation(SubmitOperationError::StoreStatus(error))
}

fn submit_machine_add_append_event(error: OperationEventLogError) -> SubmitMachineAddError {
    SubmitMachineAddError::Operation(SubmitOperationError::AppendEvent(error))
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

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::machine::JoinTokenExpiresAt;
    use ployz_test_support::fixtures::machine_join_bundle;
    use ployz_test_support::ids::{idempotency_key, machine_id, operation_id, raw_join_token};

    #[test]
    fn machine_add_submission_index_recovers_from_durable_claim_and_event() {
        let dir = tempfile::tempdir().expect("operation dir");
        prepare_operation_dir(dir.path()).expect("operation dir prepares");
        let raw_join_token = raw_join_token("raw_join_token_123");
        let fingerprint = raw_join_token
            .fingerprint()
            .expect("join token fingerprints");
        let join_token = IssuedJoinToken::new(
            fingerprint.clone(),
            JoinTokenExpiresAt::try_new(99_999).expect("valid expiry"),
        );
        let idempotency_key = idempotency_key("idem_machine_add");
        let operation_id = operation_id("op_machine_add");
        let mut index = OperationIndex::default();
        index.machine_add_claims.insert(
            idempotency_key.clone(),
            StoredMachineAddClaim {
                operation_id: operation_id.clone(),
                machine_id: machine_id("machine_a"),
                name: MachineName::try_new("machine_a").expect("valid machine name"),
                roles: InstallRolePolicy::install_all(),
                join_bundle: machine_join_bundle(),
                join_token: join_token.clone(),
                raw_join_token: raw_join_token.clone(),
            },
        );
        index.machine_add_join_tokens.insert(
            fingerprint.clone(),
            StoredMachineAddJoinToken {
                operation_id: operation_id.clone(),
                idempotency_key: idempotency_key.clone(),
            },
        );
        write_operation_index(dir.path(), &index).expect("claim index writes");
        append_event(
            dir.path(),
            &OperationEvent::MachineAddSubmitted {
                operation_id: operation_id.clone(),
                machine_id: machine_id("machine_a"),
                name: MachineName::try_new("machine_a").expect("valid machine name"),
                roles: InstallRolePolicy::install_all(),
                join_token,
            },
        )
        .expect("submitted event appends");

        let mut recovered =
            read_json_or_default::<OperationIndex>(&index_path(dir.path())).expect("index reads");
        assert!(recovered.machine_add_submissions.is_empty());

        assert!(
            recover_machine_add_submissions(dir.path(), &mut recovered).expect("recovery succeeds")
        );
        let submission = recovered
            .machine_add_submissions
            .get(&idempotency_key)
            .expect("submission recovered");
        assert_eq!(submission.operation_id, operation_id);
        assert_eq!(
            submission.start_sequence,
            EventSequence::try_new(1).expect("one is valid")
        );
        assert_eq!(submission.raw_join_token, raw_join_token);
        assert_eq!(
            recovered.machine_add_join_tokens.get(&fingerprint),
            Some(&StoredMachineAddJoinToken {
                operation_id,
                idempotency_key,
            })
        );
    }
}
