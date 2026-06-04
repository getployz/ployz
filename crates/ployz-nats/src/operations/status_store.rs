use async_nats::jetstream;
use ployz_core::ops::{EventSequence, OperationIdempotencyKey, OperationStatus};
use serde::{Deserialize, Serialize};
use std::future::Future;

use super::keys::{deploy_submission_key, operation_status_key};
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
    ) -> Result<Option<OperationStatus>, OperationStatusStoreError> {
        let Some(payload) = with_status_timeout(
            "operation status get",
            self.bucket.get(operation_status_key(operation_id)),
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
            .map_err(OperationStatusStoreError::DecodeStatus)
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
    CasConflict {
        message: String,
    },
    GetStatus {
        message: String,
    },
    Timeout {
        operation: &'static str,
    },
}

async fn with_status_timeout<T>(
    operation: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, OperationStatusStoreError> {
    tokio::time::timeout(NATS_OPERATION_TIMEOUT, future)
        .await
        .map_err(|_| OperationStatusStoreError::Timeout { operation })
}
