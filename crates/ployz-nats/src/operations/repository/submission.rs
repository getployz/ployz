use ployz_core::ids::{CertId, MachineId, OperationId};
use ployz_core::install::InstallArtifactVersion;
use ployz_core::install::MachineJoinBundle;
use ployz_core::machine::{IssuedJoinToken, MachineName, RawJoinToken};
use ployz_core::ops::{
    EventSequence, OperationEvent, OperationIdempotencyKey, OperationKind, OperationStatus,
};
use ployz_core::roles::InstallRolePolicy;

use super::AsyncNatsOperationRepository;
use crate::operations::events::{OperationEventAppend, OperationEventLogError};
use crate::operations::status_store::{
    OperationStatusStoreError, StatusStoreWrite, StoredDeployClaim, StoredMachineAddClaim,
    StoredMachineAddJoinToken, StoredMachineAddSubmission,
};

/// Per-kind adapter for the shared submit flow: how to build the submitted
/// transcript event and accepted status.
trait SubmitKind: Sized {
    type Payload: Clone;

    const KIND: OperationKind;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEventAppend;

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)>;

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus;
}

struct SubmittedOperation<P> {
    operation_id: OperationId,
    start_sequence: EventSequence,
    payload: P,
    should_start_execution: bool,
}

impl SubmitKind for DeployOperationSubmission {
    type Payload = ployz_core::deploy::DeployRequest;
    const KIND: OperationKind = OperationKind::Deploy;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEventAppend {
        OperationEventAppend::from_event(OperationEvent::DeploySubmitted {
            operation_id,
            target: payload,
        })
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

impl SubmitKind for CertOperationSubmission {
    type Payload = CertId;
    const KIND: OperationKind = OperationKind::Cert;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEventAppend {
        OperationEventAppend::from_event(OperationEvent::CertRenewalSubmitted {
            operation_id,
            cert_id: payload,
        })
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::CertRenewalSubmitted {
            operation_id,
            cert_id,
        } = event
        else {
            return None;
        };
        Some((operation_id, cert_id))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::cert_accepted(operation_id, payload.clone(), sequence)
    }
}

impl SubmitKind for MachineUpdateOperationSubmission {
    type Payload = MachineUpdatePayload;
    const KIND: OperationKind = OperationKind::MachineUpdate;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEventAppend {
        OperationEventAppend::from_event(OperationEvent::MachineUpdateSubmitted {
            operation_id,
            machine_id: payload.machine_id,
            target_version: payload.target_version,
        })
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

impl AsyncNatsOperationRepository {
    pub async fn claim_deploy(
        &self,
        submission: DeployOperationSubmission,
    ) -> Result<DeployOperationSubmission, SubmitOperationError> {
        let DeployOperationSubmission {
            operation_id,
            idempotency_key,
            target,
        } = submission;
        let claim = self
            .status_store
            .put_deploy_claim_if_absent(
                &idempotency_key,
                &StoredDeployClaim {
                    operation_id,
                    target,
                },
            )
            .await
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
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn submit_cert(
        &self,
        submission: CertOperationSubmission,
    ) -> Result<AcceptedCertSubmission, SubmitOperationError> {
        let CertOperationSubmission {
            operation_id,
            cert_id,
        } = submission;
        let submitted = self
            .submit_operation::<CertOperationSubmission>(operation_id, cert_id)
            .await?;

        Ok(AcceptedCertSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            cert_id: submitted.payload,
        })
    }

    pub async fn submit_machine_update(
        &self,
        submission: MachineUpdateOperationSubmission,
    ) -> Result<AcceptedMachineUpdateSubmission, SubmitOperationError> {
        let MachineUpdateOperationSubmission {
            operation_id,
            machine_id,
            target_version,
        } = submission;
        let payload = MachineUpdatePayload {
            machine_id,
            target_version,
        };
        let submitted = self
            .submit_operation::<MachineUpdateOperationSubmission>(operation_id, payload)
            .await?;

        Ok(AcceptedMachineUpdateSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            machine_id: submitted.payload.machine_id,
            target_version: submitted.payload.target_version,
            should_start_execution: submitted.should_start_execution,
        })
    }

    /// The shared accept flow: append the submitted transcript event, create
    /// accepted status, and return quickly.
    async fn submit_operation<K: SubmitKind>(
        &self,
        operation_id: OperationId,
        payload: K::Payload,
    ) -> Result<SubmittedOperation<K::Payload>, SubmitOperationError> {
        if let Some(current) = self
            .status_store
            .get(&operation_id)
            .await
            .map_err(OperationStatusStoreError::from_status_read)
            .map_err(SubmitOperationError::StoreStatus)?
            && current.kind() != K::KIND
        {
            return Err(SubmitOperationError::DuplicateSequenceMismatch {
                sequence: current.last_event_sequence(),
            });
        }

        let stored = self
            .event_log
            .append(K::submitted_event(operation_id.clone(), payload.clone()))
            .await
            .map_err(SubmitOperationError::AppendEvent)?;
        if stored.duplicate {
            let (payload, should_start_execution) = self
                .recover_submitted_operation::<K>(&operation_id, stored.sequence)
                .await?;
            return Ok(SubmittedOperation {
                operation_id,
                start_sequence: stored.sequence,
                payload,
                should_start_execution,
            });
        }
        let status = K::accepted_status(operation_id.clone(), &payload, stored.sequence);
        let write = self
            .status_store
            .put_if_newer(&status)
            .await
            .map_err(SubmitOperationError::StoreStatus)?;
        let should_start_execution = submit_status_should_start(write, stored.sequence)?;

        Ok(SubmittedOperation {
            operation_id,
            start_sequence: stored.sequence,
            payload,
            should_start_execution,
        })
    }

    async fn recover_submitted_operation<K: SubmitKind>(
        &self,
        expected_operation_id: &OperationId,
        sequence: EventSequence,
    ) -> Result<(K::Payload, bool), SubmitOperationError> {
        let event = self
            .event_log
            .event_at_sequence(sequence)
            .await
            .map_err(SubmitOperationError::AppendEvent)?;
        let Some((operation_id, payload)) = K::submitted_event_parts(event) else {
            return Err(SubmitOperationError::DuplicateSequenceMismatch { sequence });
        };
        if &operation_id != expected_operation_id {
            return Err(SubmitOperationError::DuplicateSequenceMismatch { sequence });
        }

        let status = K::accepted_status(operation_id, &payload, sequence);
        let write = self
            .status_store
            .put_if_newer(&status)
            .await
            .map_err(SubmitOperationError::StoreStatus)?;
        let should_start_execution = submit_status_should_start(write, sequence)?;

        Ok((payload, should_start_execution))
    }

    pub async fn submit_machine_add(
        &self,
        submission: MachineAddOperationSubmission,
    ) -> Result<AcceptedMachineAddSubmission, SubmitMachineAddError> {
        validate_machine_add_join_material(&submission.raw_join_token, &submission.join_token)?;
        let requested_operation_id = submission.operation_id.clone();
        if let Some(current) = self
            .status_store
            .get(&submission.operation_id)
            .await
            .map_err(OperationStatusStoreError::from_status_read)
            .map_err(submit_machine_add_store_status)?
            && current.kind() != OperationKind::MachineAdd
        {
            return Err(submit_machine_add_duplicate_mismatch(
                current.last_event_sequence(),
            ));
        }

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
        let claim = self
            .status_store
            .put_machine_add_claim_if_absent(&idempotency_key, &claim)
            .await
            .map_err(submit_machine_add_store_status)?;
        if claim.operation_id != requested_operation_id {
            return Err(SubmitMachineAddError::DuplicateIdempotencyKey);
        }

        let operation_id = claim.operation_id;
        let machine_id = claim.machine_id;
        let name = claim.name;
        let roles = claim.roles;
        let join_bundle = claim.join_bundle;
        let join_token = claim.join_token;
        let raw_join_token = claim.raw_join_token;
        let fingerprint = validate_machine_add_join_material(&raw_join_token, &join_token)?;

        self.status_store
            .put_machine_add_join_token_if_absent(
                &fingerprint,
                &StoredMachineAddJoinToken {
                    operation_id: operation_id.clone(),
                    idempotency_key: idempotency_key.clone(),
                },
            )
            .await
            .map_err(submit_machine_add_store_status)?;
        let stored = self
            .event_log
            .append(OperationEventAppend::from_event(
                OperationEvent::MachineAddSubmitted {
                    operation_id: operation_id.clone(),
                    machine_id: machine_id.clone(),
                    name: name.clone(),
                    roles,
                    join_token: join_token.clone(),
                },
            ))
            .await
            .map_err(submit_machine_add_append_event)?;
        if stored.duplicate {
            let stored_event = self
                .event_log
                .event_at_sequence(stored.sequence)
                .await
                .map_err(submit_machine_add_append_event)?;
            if !machine_add_submitted_event_matches(
                stored_event,
                &operation_id,
                &machine_id,
                &name,
                roles,
                &join_token,
            ) {
                return Err(submit_machine_add_duplicate_mismatch(stored.sequence));
            }
        }
        let submitted = StoredMachineAddSubmission {
            operation_id: operation_id.clone(),
            idempotency_key: idempotency_key.clone(),
            start_sequence: stored.sequence,
            machine_id,
            name,
            roles,
            join_bundle,
            join_token,
            raw_join_token,
        };
        let submitted = self
            .status_store
            .put_machine_add_submission_if_absent(&idempotency_key, &submitted)
            .await
            .map_err(submit_machine_add_store_status)?;
        if submitted.operation_id != operation_id {
            return Err(submit_machine_add_duplicate_mismatch(stored.sequence));
        }
        let status = OperationStatus::machine_add_pending(
            operation_id.clone(),
            submitted.machine_id.clone(),
            submitted.name.clone(),
            submitted.roles,
            submitted.join_token.clone(),
            stored.sequence,
        );
        self.status_store
            .put_if_newer(&status)
            .await
            .map_err(submit_machine_add_store_status)
            .and_then(|write| match write {
                StatusStoreWrite::Stored { .. } => Ok(()),
                StatusStoreWrite::Stale {
                    current_sequence,
                    attempted_sequence,
                } if current_sequence >= attempted_sequence => Ok(()),
                StatusStoreWrite::Stale {
                    current_sequence, ..
                }
                | StatusStoreWrite::Contended {
                    current_sequence, ..
                } => Err(submit_machine_add_duplicate_mismatch(current_sequence)),
            })?;

        Ok(AcceptedMachineAddSubmission {
            operation_id: submitted.operation_id,
            start_sequence: stored.sequence,
            machine_id: submitted.machine_id,
            name: submitted.name,
            roles: submitted.roles,
            join_bundle: submitted.join_bundle,
            join_token: submitted.join_token,
            raw_join_token: submitted.raw_join_token,
        })
    }
}

fn machine_add_submitted_event_matches(
    event: OperationEvent,
    expected_operation_id: &OperationId,
    expected_machine_id: &MachineId,
    expected_name: &MachineName,
    expected_roles: InstallRolePolicy,
    expected_join_token: &IssuedJoinToken,
) -> bool {
    let OperationEvent::MachineAddSubmitted {
        operation_id,
        machine_id,
        name,
        roles,
        join_token,
    } = event
    else {
        return false;
    };
    operation_id == *expected_operation_id
        && machine_id == *expected_machine_id
        && name == *expected_name
        && roles == expected_roles
        && join_token == *expected_join_token
}

fn submit_machine_add_store_status(error: OperationStatusStoreError) -> SubmitMachineAddError {
    match error {
        OperationStatusStoreError::RecordExists { .. } => {
            SubmitMachineAddError::DuplicateIdempotencyKey
        }
        error @ (OperationStatusStoreError::OpenBucket { .. }
        | OperationStatusStoreError::EncodeStatus(_)
        | OperationStatusStoreError::DecodeStatus(_)
        | OperationStatusStoreError::EncodeSubmission(_)
        | OperationStatusStoreError::DecodeSubmission(_)
        | OperationStatusStoreError::CasConflict { .. }
        | OperationStatusStoreError::GetStatus { .. }
        | OperationStatusStoreError::Timeout { .. }) => {
            SubmitMachineAddError::Operation(SubmitOperationError::StoreStatus(error))
        }
    }
}

fn submit_machine_add_append_event(error: OperationEventLogError) -> SubmitMachineAddError {
    SubmitMachineAddError::Operation(SubmitOperationError::AppendEvent(error))
}

const fn submit_machine_add_duplicate_mismatch(sequence: EventSequence) -> SubmitMachineAddError {
    SubmitMachineAddError::Operation(SubmitOperationError::DuplicateSequenceMismatch { sequence })
}

fn submit_status_should_start(
    write: StatusStoreWrite,
    sequence: EventSequence,
) -> Result<bool, SubmitOperationError> {
    match write {
        StatusStoreWrite::Stored { .. } => Ok(true),
        StatusStoreWrite::Stale {
            current_sequence,
            attempted_sequence,
        } if current_sequence == attempted_sequence => Ok(true),
        StatusStoreWrite::Stale {
            current_sequence,
            attempted_sequence,
        } if current_sequence > attempted_sequence => Ok(false),
        StatusStoreWrite::Stale { .. } | StatusStoreWrite::Contended { .. } => {
            Err(SubmitOperationError::DuplicateSequenceMismatch { sequence })
        }
    }
}

fn validate_machine_add_join_material(
    raw_join_token: &RawJoinToken,
    join_token: &IssuedJoinToken,
) -> Result<ployz_core::machine::JoinTokenFingerprint, SubmitMachineAddError> {
    let fingerprint = raw_join_token
        .fingerprint()
        .map_err(|_| SubmitMachineAddError::JoinTokenMismatch)?;
    if join_token.matches(&fingerprint) {
        Ok(fingerprint)
    } else {
        Err(SubmitMachineAddError::JoinTokenMismatch)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployOperationSubmission {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub target: ployz_core::deploy::DeployRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertOperationSubmission {
    pub operation_id: OperationId,
    pub cert_id: CertId,
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
pub struct AcceptedDeploySubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub target: ployz_core::deploy::DeployRequest,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedCertSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub cert_id: CertId,
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

/// How a deploy or cert submission fails inside the repository.
#[derive(Debug)]
pub enum SubmitOperationError {
    InvalidDeployTarget,
    AppendEvent(OperationEventLogError),
    StoreStatus(OperationStatusStoreError),
    DuplicateSequenceMismatch { sequence: EventSequence },
}

/// Machine-add extends the shared submit failure with join-token
/// validation.
#[derive(Debug)]
pub enum SubmitMachineAddError {
    Operation(SubmitOperationError),
    JoinTokenMismatch,
    DuplicateIdempotencyKey,
}
