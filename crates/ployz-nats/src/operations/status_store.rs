use async_nats::jetstream;
use ployz_core::ids::{OperationId, OperationOwnerId};
use ployz_core::machine::{IssuedJoinToken, JoinTokenFingerprint, MachineName, RawJoinToken};
use ployz_core::ops::{
    EventSequence, OperationIdempotencyKey, OperationLeaseExpiresAt, OperationOwnerLease,
    OperationOwnershipStatus, OperationStatus,
};
use ployz_core::roles::FirstNodeGateway;
use serde::{Deserialize, Serialize};
use std::future::Future;

use super::keys::{
    cert_submission_key, deploy_submission_key, machine_add_join_token_key,
    machine_add_submission_key, operation_owner_lease_key, operation_status_key,
};
use super::projection::{status_id, status_sequence};
use super::{KV_OPS_BUCKET, NATS_OPERATION_TIMEOUT};

#[derive(Debug, Clone)]
pub struct AsyncNatsOperationStatusStore {
    bucket: jetstream::kv::Store,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDeploySubmission {
    pub operation_id: ployz_core::ids::OperationId,
    pub start_sequence: EventSequence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCertSubmission {
    pub operation_id: ployz_core::ids::OperationId,
    pub start_sequence: EventSequence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMachineAddSubmission {
    pub operation_id: ployz_core::ids::OperationId,
    pub start_sequence: Option<EventSequence>,
    pub node_id: ployz_core::ids::NodeId,
    pub name: MachineName,
    pub gateway: FirstNodeGateway,
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMachineAddJoinToken {
    pub operation_id: ployz_core::ids::OperationId,
    pub idempotency_key: OperationIdempotencyKey,
}

impl AsyncNatsOperationStatusStore {
    pub async fn from_jetstream(
        jetstream: &jetstream::Context,
    ) -> Result<Self, OperationStatusStoreError> {
        let bucket = with_status_timeout(
            "operation status bucket open",
            jetstream.get_key_value(KV_OPS_BUCKET),
        )
        .await?
        .map_err(|error| OperationStatusStoreError::OpenBucket {
            bucket: KV_OPS_BUCKET,
            message: error.to_string(),
        })?;

        Ok(Self { bucket })
    }

    #[must_use]
    pub fn new(bucket: jetstream::kv::Store) -> Self {
        Self { bucket }
    }

    pub async fn put_if_newer(
        &self,
        status: &OperationStatus,
    ) -> Result<OperationStatusWrite, OperationStatusStoreError> {
        let key = operation_status_key(status_id(status));
        let incoming_sequence = status_sequence(status);
        let payload =
            serde_json::to_vec(status).map_err(OperationStatusStoreError::EncodeStatus)?;
        let Some(existing) = with_status_timeout(
            "operation status entry read",
            self.bucket.entry(key.clone()),
        )
        .await?
        .map_err(|error| OperationStatusStoreError::GetStatus {
            message: error.to_string(),
        })?
        else {
            let revision = match with_status_timeout(
                "operation status create",
                self.bucket.create(&key, payload.into()),
            )
            .await?
            {
                Ok(revision) => revision,
                Err(error) => {
                    return self
                        .classify_write_conflict(&key, incoming_sequence, error)
                        .await;
                }
            };
            return Ok(OperationStatusWrite::Stored {
                revision: KvRevision(revision),
            });
        };

        let current: OperationStatus = serde_json::from_slice(&existing.value)
            .map_err(OperationStatusStoreError::DecodeStatus)?;
        let current_sequence = status_sequence(&current);
        if current_sequence >= incoming_sequence {
            return Ok(OperationStatusWrite::Stale {
                current_sequence,
                attempted_sequence: incoming_sequence,
            });
        }

        let revision = match with_status_timeout(
            "operation status update",
            self.bucket.update(&key, payload.into(), existing.revision),
        )
        .await?
        {
            Ok(revision) => revision,
            Err(error) => {
                return self
                    .classify_write_conflict(&key, incoming_sequence, error)
                    .await;
            }
        };

        Ok(OperationStatusWrite::Stored {
            revision: KvRevision(revision),
        })
    }

    pub async fn deploy_submission(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredDeploySubmission>, OperationStatusStoreError> {
        let Some(payload) = with_status_timeout(
            "deploy submission get",
            self.bucket.get(deploy_submission_key(idempotency_key)),
        )
        .await?
        .map_err(|error| OperationStatusStoreError::GetStatus {
            message: error.to_string(),
        })?
        else {
            return Ok(None);
        };

        serde_json::from_slice(&payload)
            .map(Some)
            .map_err(OperationStatusStoreError::DecodeSubmission)
    }

    pub async fn put_deploy_submission_if_absent(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        submission: &StoredDeploySubmission,
    ) -> Result<StoredDeploySubmission, OperationStatusStoreError> {
        if let Some(existing) = self.deploy_submission(idempotency_key).await? {
            return Ok(existing);
        }

        let key = deploy_submission_key(idempotency_key);
        let payload =
            serde_json::to_vec(submission).map_err(OperationStatusStoreError::EncodeSubmission)?;
        match with_status_timeout(
            "deploy submission create",
            self.bucket.create(&key, payload.into()),
        )
        .await?
        {
            Ok(_) => Ok(submission.clone()),
            Err(error) => {
                if let Some(existing) = self.deploy_submission(idempotency_key).await? {
                    Ok(existing)
                } else {
                    Err(OperationStatusStoreError::CasConflict {
                        message: error.to_string(),
                    })
                }
            }
        }
    }

    pub async fn cert_submission(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredCertSubmission>, OperationStatusStoreError> {
        let Some(payload) = with_status_timeout(
            "cert submission get",
            self.bucket.get(cert_submission_key(idempotency_key)),
        )
        .await?
        .map_err(|error| OperationStatusStoreError::GetStatus {
            message: error.to_string(),
        })?
        else {
            return Ok(None);
        };

        serde_json::from_slice(&payload)
            .map(Some)
            .map_err(OperationStatusStoreError::DecodeSubmission)
    }

    pub async fn put_cert_submission_if_absent(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        submission: &StoredCertSubmission,
    ) -> Result<StoredCertSubmission, OperationStatusStoreError> {
        if let Some(existing) = self.cert_submission(idempotency_key).await? {
            return Ok(existing);
        }

        let key = cert_submission_key(idempotency_key);
        let payload =
            serde_json::to_vec(submission).map_err(OperationStatusStoreError::EncodeSubmission)?;
        match with_status_timeout(
            "cert submission create",
            self.bucket.create(&key, payload.into()),
        )
        .await?
        {
            Ok(_) => Ok(submission.clone()),
            Err(error) => {
                if let Some(existing) = self.cert_submission(idempotency_key).await? {
                    Ok(existing)
                } else {
                    Err(OperationStatusStoreError::CasConflict {
                        message: error.to_string(),
                    })
                }
            }
        }
    }

    pub async fn machine_add_submission(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredMachineAddSubmission>, OperationStatusStoreError> {
        let Some(payload) = with_status_timeout(
            "machine add submission get",
            self.bucket.get(machine_add_submission_key(idempotency_key)),
        )
        .await?
        .map_err(|error| OperationStatusStoreError::GetStatus {
            message: error.to_string(),
        })?
        else {
            return Ok(None);
        };

        serde_json::from_slice(&payload)
            .map(Some)
            .map_err(OperationStatusStoreError::DecodeSubmission)
    }

    pub async fn put_machine_add_submission_if_absent(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        submission: &StoredMachineAddSubmission,
    ) -> Result<StoredMachineAddSubmission, OperationStatusStoreError> {
        if let Some(existing) = self.machine_add_submission(idempotency_key).await? {
            return Ok(existing);
        }

        let key = machine_add_submission_key(idempotency_key);
        let payload =
            serde_json::to_vec(submission).map_err(OperationStatusStoreError::EncodeSubmission)?;
        match with_status_timeout(
            "machine add submission create",
            self.bucket.create(&key, payload.into()),
        )
        .await?
        {
            Ok(_) => Ok(submission.clone()),
            Err(error) => {
                if let Some(existing) = self.machine_add_submission(idempotency_key).await? {
                    Ok(existing)
                } else {
                    Err(OperationStatusStoreError::CasConflict {
                        message: error.to_string(),
                    })
                }
            }
        }
    }

    pub async fn put_machine_add_join_token_if_absent(
        &self,
        fingerprint: &JoinTokenFingerprint,
        token: &StoredMachineAddJoinToken,
    ) -> Result<StoredMachineAddJoinToken, OperationStatusStoreError> {
        if let Some(existing) = self.machine_add_join_token(fingerprint).await? {
            if existing == *token {
                return Ok(existing);
            }
            return Err(OperationStatusStoreError::CasConflict {
                message: "join token fingerprint is already assigned".to_owned(),
            });
        }

        let key = machine_add_join_token_key(fingerprint);
        let payload =
            serde_json::to_vec(token).map_err(OperationStatusStoreError::EncodeSubmission)?;
        match with_status_timeout(
            "machine add join token index create",
            self.bucket.create(&key, payload.into()),
        )
        .await?
        {
            Ok(_) => Ok(token.clone()),
            Err(error) => {
                let Some(existing) = self.machine_add_join_token(fingerprint).await? else {
                    return Err(OperationStatusStoreError::CasConflict {
                        message: error.to_string(),
                    });
                };
                if existing == *token {
                    Ok(existing)
                } else {
                    Err(OperationStatusStoreError::CasConflict {
                        message: "join token fingerprint is already assigned".to_owned(),
                    })
                }
            }
        }
    }

    pub async fn machine_add_submission_for_join_token(
        &self,
        fingerprint: &JoinTokenFingerprint,
    ) -> Result<Option<StoredMachineAddSubmission>, OperationStatusStoreError> {
        let Some(index) = self.machine_add_join_token(fingerprint).await? else {
            return Ok(None);
        };
        let Some(submission) = self.machine_add_submission(&index.idempotency_key).await? else {
            return Err(OperationStatusStoreError::CasConflict {
                message: "join token index points at a missing machine add submission".to_owned(),
            });
        };
        if submission.operation_id != index.operation_id {
            return Err(OperationStatusStoreError::CasConflict {
                message: "join token index points at a different operation".to_owned(),
            });
        }
        if submission.start_sequence.is_none() {
            return Ok(None);
        }

        Ok(Some(submission))
    }

    async fn machine_add_join_token(
        &self,
        fingerprint: &JoinTokenFingerprint,
    ) -> Result<Option<StoredMachineAddJoinToken>, OperationStatusStoreError> {
        let Some(payload) = with_status_timeout(
            "machine add join token index get",
            self.bucket.get(machine_add_join_token_key(fingerprint)),
        )
        .await?
        .map_err(|error| OperationStatusStoreError::GetStatus {
            message: error.to_string(),
        })?
        else {
            return Ok(None);
        };

        serde_json::from_slice(&payload)
            .map(Some)
            .map_err(OperationStatusStoreError::DecodeSubmission)
    }

    pub async fn record_machine_add_submission_sequence(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        sequence: EventSequence,
    ) -> Result<StoredMachineAddSubmission, OperationStatusStoreError> {
        let key = machine_add_submission_key(idempotency_key);
        let Some(existing) = with_status_timeout(
            "machine add submission entry read",
            self.bucket.entry(key.clone()),
        )
        .await?
        .map_err(|error| OperationStatusStoreError::GetStatus {
            message: error.to_string(),
        })?
        else {
            return Err(OperationStatusStoreError::CasConflict {
                message: "machine add submission record is missing".to_owned(),
            });
        };
        let mut submission: StoredMachineAddSubmission = serde_json::from_slice(&existing.value)
            .map_err(OperationStatusStoreError::DecodeSubmission)?;
        if submission.start_sequence == Some(sequence) {
            return Ok(submission);
        }
        if let Some(start_sequence) = submission.start_sequence {
            return Err(OperationStatusStoreError::CasConflict {
                message: format!(
                    "machine add submission sequence is {} but attempted {}",
                    start_sequence.get(),
                    sequence.get()
                ),
            });
        }

        submission.start_sequence = Some(sequence);
        let payload =
            serde_json::to_vec(&submission).map_err(OperationStatusStoreError::EncodeSubmission)?;
        match with_status_timeout(
            "machine add submission sequence update",
            self.bucket.update(&key, payload.into(), existing.revision),
        )
        .await?
        {
            Ok(_) => Ok(submission),
            Err(error) => {
                let Some(current) = self.machine_add_submission(idempotency_key).await? else {
                    return Err(OperationStatusStoreError::CasConflict {
                        message: error.to_string(),
                    });
                };
                if current.start_sequence == Some(sequence) {
                    Ok(current)
                } else {
                    Err(OperationStatusStoreError::CasConflict {
                        message: error.to_string(),
                    })
                }
            }
        }
    }

    pub async fn claim_owner_lease(
        &self,
        operation_id: &OperationId,
        owner_id: &OperationOwnerId,
        now: OperationLeaseExpiresAt,
        expires_at: OperationLeaseExpiresAt,
    ) -> Result<OperationOwnerLease, OperationStatusStoreError> {
        let key = operation_owner_lease_key(operation_id);
        let candidate =
            OperationOwnerLease::new(operation_id.clone(), owner_id.clone(), expires_at);
        let payload =
            serde_json::to_vec(&candidate).map_err(OperationStatusStoreError::EncodeLease)?;

        let Some(existing) =
            with_status_timeout("operation owner lease read", self.bucket.entry(key.clone()))
                .await?
                .map_err(|error| OperationStatusStoreError::GetStatus {
                    message: error.to_string(),
                })?
        else {
            return match with_status_timeout(
                "operation owner lease create",
                self.bucket.create(&key, payload.into()),
            )
            .await?
            {
                Ok(_) => Ok(candidate),
                Err(error) => {
                    self.claim_owner_lease_after_conflict(operation_id, now, error)
                        .await
                }
            };
        };

        let current: OperationOwnerLease = serde_json::from_slice(&existing.value)
            .map_err(OperationStatusStoreError::DecodeLease)?;
        if !current.is_expired_at(now) {
            return Ok(current);
        }

        match with_status_timeout(
            "operation owner lease update",
            self.bucket.update(&key, payload.into(), existing.revision),
        )
        .await?
        {
            Ok(_) => Ok(candidate),
            Err(error) => {
                self.claim_owner_lease_after_conflict(operation_id, now, error)
                    .await
            }
        }
    }

    pub async fn renew_owner_lease(
        &self,
        operation_id: &OperationId,
        owner_id: &OperationOwnerId,
        now: OperationLeaseExpiresAt,
        expires_at: OperationLeaseExpiresAt,
    ) -> Result<Option<OperationOwnerLease>, OperationStatusStoreError> {
        let key = operation_owner_lease_key(operation_id);
        let Some(existing) =
            with_status_timeout("operation owner lease read", self.bucket.entry(key.clone()))
                .await?
                .map_err(|error| OperationStatusStoreError::GetStatus {
                    message: error.to_string(),
                })?
        else {
            return Ok(None);
        };

        let current: OperationOwnerLease = serde_json::from_slice(&existing.value)
            .map_err(OperationStatusStoreError::DecodeLease)?;
        if current.owner_id != *owner_id || current.is_expired_at(now) {
            return Ok(None);
        }
        if expires_at <= current.expires_at {
            return Ok(Some(current));
        }

        let renewed = current.renew_until(expires_at);
        let payload =
            serde_json::to_vec(&renewed).map_err(OperationStatusStoreError::EncodeLease)?;
        match with_status_timeout(
            "operation owner lease update",
            self.bucket.update(&key, payload.into(), existing.revision),
        )
        .await?
        {
            Ok(_) => Ok(Some(renewed)),
            Err(error) => {
                if let Some(current) = self.operation_owner_lease(operation_id).await? {
                    if current.owner_id == *owner_id && !current.is_expired_at(now) {
                        Ok(Some(current))
                    } else {
                        Ok(None)
                    }
                } else {
                    Err(OperationStatusStoreError::CasConflict {
                        message: error.to_string(),
                    })
                }
            }
        }
    }

    pub async fn operation_ownership(
        &self,
        operation_id: &OperationId,
        now: OperationLeaseExpiresAt,
    ) -> Result<OperationOwnershipStatus, OperationStatusStoreError> {
        let Some(lease) = self.operation_owner_lease(operation_id).await? else {
            return Ok(OperationOwnershipStatus::Unclaimed);
        };

        Ok(OperationOwnershipStatus::from_lease_at(lease, now))
    }

    async fn operation_owner_lease(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationOwnerLease>, OperationStatusStoreError> {
        let Some(payload) = with_status_timeout(
            "operation owner lease get",
            self.bucket.get(operation_owner_lease_key(operation_id)),
        )
        .await?
        .map_err(|error| OperationStatusStoreError::GetStatus {
            message: error.to_string(),
        })?
        else {
            return Ok(None);
        };

        serde_json::from_slice(&payload)
            .map(Some)
            .map_err(OperationStatusStoreError::DecodeLease)
    }

    async fn claim_owner_lease_after_conflict(
        &self,
        operation_id: &OperationId,
        now: OperationLeaseExpiresAt,
        error: impl ToString,
    ) -> Result<OperationOwnerLease, OperationStatusStoreError> {
        let Some(current) = self.operation_owner_lease(operation_id).await? else {
            return Err(OperationStatusStoreError::CasConflict {
                message: error.to_string(),
            });
        };
        if current.is_expired_at(now) {
            return Err(OperationStatusStoreError::CasConflict {
                message: error.to_string(),
            });
        }
        Ok(current)
    }

    async fn classify_write_conflict(
        &self,
        key: &str,
        attempted_sequence: EventSequence,
        error: impl ToString,
    ) -> Result<OperationStatusWrite, OperationStatusStoreError> {
        let Some(existing) =
            with_status_timeout("operation status conflict read", self.bucket.entry(key))
                .await?
                .map_err(|error| OperationStatusStoreError::GetStatus {
                    message: error.to_string(),
                })?
        else {
            return Err(OperationStatusStoreError::CasConflict {
                message: error.to_string(),
            });
        };

        let current: OperationStatus = serde_json::from_slice(&existing.value)
            .map_err(OperationStatusStoreError::DecodeStatus)?;
        let current_sequence = status_sequence(&current);
        if current_sequence >= attempted_sequence {
            return Ok(OperationStatusWrite::Stale {
                current_sequence,
                attempted_sequence,
            });
        }

        Ok(OperationStatusWrite::Contended {
            current_sequence,
            attempted_sequence,
        })
    }

    pub async fn get(
        &self,
        operation_id: &ployz_core::ids::OperationId,
    ) -> Result<Option<OperationStatus>, OperationStatusReadError> {
        let Some(payload) = with_status_read_timeout(
            "operation status get",
            self.bucket.get(operation_status_key(operation_id)),
        )
        .await?
        .map_err(|error| OperationStatusReadError::GetStatus {
            message: error.to_string(),
        })?
        else {
            return Ok(None);
        };

        serde_json::from_slice(&payload)
            .map(Some)
            .map_err(OperationStatusReadError::DecodeStatus)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KvRevision(u64);

impl KvRevision {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatusWrite {
    Stored {
        revision: KvRevision,
    },
    AlreadySatisfied {
        current_sequence: EventSequence,
    },
    Stale {
        current_sequence: EventSequence,
        attempted_sequence: EventSequence,
    },
    Contended {
        current_sequence: EventSequence,
        attempted_sequence: EventSequence,
    },
}

#[derive(Debug)]
pub enum OperationStatusStoreError {
    OpenBucket {
        bucket: &'static str,
        message: String,
    },
    EncodeStatus(serde_json::Error),
    DecodeStatus(serde_json::Error),
    EncodeSubmission(serde_json::Error),
    DecodeSubmission(serde_json::Error),
    EncodeLease(serde_json::Error),
    DecodeLease(serde_json::Error),
    CasConflict {
        message: String,
    },
    GetStatus {
        message: String,
    },
    Timeout {
        operation: &'static str,
    },
    Clock {
        message: String,
    },
}

impl OperationStatusStoreError {
    #[must_use]
    pub fn from_status_read(error: OperationStatusReadError) -> Self {
        match error {
            OperationStatusReadError::DecodeStatus(error) => Self::DecodeStatus(error),
            OperationStatusReadError::GetStatus { message } => Self::GetStatus { message },
            OperationStatusReadError::Timeout { operation } => Self::Timeout { operation },
        }
    }
}

#[derive(Debug)]
pub enum OperationStatusReadError {
    DecodeStatus(serde_json::Error),
    GetStatus { message: String },
    Timeout { operation: &'static str },
}

async fn with_status_timeout<T>(
    operation: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, OperationStatusStoreError> {
    tokio::time::timeout(NATS_OPERATION_TIMEOUT, future)
        .await
        .map_err(|_| OperationStatusStoreError::Timeout { operation })
}

async fn with_status_read_timeout<T>(
    operation: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, OperationStatusReadError> {
    tokio::time::timeout(NATS_OPERATION_TIMEOUT, future)
        .await
        .map_err(|_| OperationStatusReadError::Timeout { operation })
}
