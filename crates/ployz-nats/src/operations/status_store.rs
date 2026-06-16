use async_nats::jetstream;
use ployz_core::ids::{OperationId, OperationOwnerId};
use ployz_core::install::{MachineJoinBundle, MachineJoinSecretDelivery};
use ployz_core::machine::{IssuedJoinToken, JoinTokenFingerprint, MachineName, RawJoinToken};
use ployz_core::nats_config::{NatsUserPublicKey, NatsUserSeed};
use ployz_core::ops::{
    EventSequence, OperationIdempotencyKey, OperationLeaseExpiresAt, OperationOwnerLease,
    OperationOwnershipStatus, OperationStatus,
};
use ployz_core::roles::InstallRolePolicy;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::KV_OPS_BUCKET;
use super::keys::{
    MACHINE_ADD_SUBMISSION_KEY_PREFIX, backup_submission_key, cert_submission_key,
    deploy_submission_key, machine_add_join_token_key, machine_add_mint_claim_key,
    machine_add_secret_delivery_key, machine_add_submission_key, operation_owner_lease_key,
    operation_status_key,
};
use crate::kv::{NatsIoTimeout, bounded_bucket_key_scan_entries_with_prefix, with_io_timeout};

#[derive(Debug, Clone)]
pub struct AsyncNatsOperationStatusStore {
    bucket: jetstream::kv::Store,
}

/// One accepted operation submission keyed by idempotency key. Deploy,
/// cert, and backup submissions all store this shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredOperationSubmission {
    pub operation_id: ployz_core::ids::OperationId,
    pub start_sequence: EventSequence,
}

pub type StoredDeploySubmission = StoredOperationSubmission;
pub type StoredCertSubmission = StoredOperationSubmission;
pub type StoredBackupSubmission = StoredOperationSubmission;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMachineAddSubmission {
    pub operation_id: ployz_core::ids::OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub start_sequence: Option<EventSequence>,
    pub node_id: ployz_core::ids::NodeId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    pub join_bundle: MachineJoinBundle,
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMachineAddSecretDelivery {
    pub operation_id: ployz_core::ids::OperationId,
    pub secret_delivery: MachineJoinSecretDelivery,
}

/// The write-once mint claim for one machine-add idempotency key
/// (ADR-0015 atomic resource claim). The first mint run stores its freshly
/// generated material here before any render; concurrent or resumed runs
/// adopt the claimed material and converge on the same secret delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMachineAddMintClaim {
    pub operation_id: ployz_core::ids::OperationId,
    pub nkey_public: NatsUserPublicKey,
    pub nkey_seed: NatsUserSeed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMachineAddJoinToken {
    pub operation_id: ployz_core::ids::OperationId,
    pub idempotency_key: OperationIdempotencyKey,
}

/// What to do when a create-only record write finds an existing record
/// under the same key.
#[derive(Debug, Clone, Copy)]
enum AdoptPolicy {
    /// First writer wins; later writers adopt the stored record as-is.
    AdoptExisting,
    /// The stored record must equal the candidate; anything else is a
    /// conflict with this message.
    RequireEqual { conflict_message: &'static str },
}

impl AsyncNatsOperationStatusStore {
    pub async fn from_jetstream(
        jetstream: &jetstream::Context,
    ) -> Result<Self, OperationStatusStoreError> {
        let bucket = with_io_timeout(
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

    async fn get_record<T>(
        &self,
        operation: &'static str,
        key: String,
        decode_error: fn(serde_json::Error) -> OperationStatusStoreError,
    ) -> Result<Option<T>, OperationStatusStoreError>
    where
        T: DeserializeOwned,
    {
        let Some(payload) = with_io_timeout(operation, self.bucket.get(key))
            .await?
            .map_err(|error| OperationStatusStoreError::GetStatus {
                message: error.to_string(),
            })?
        else {
            return Ok(None);
        };

        serde_json::from_slice(&payload)
            .map(Some)
            .map_err(decode_error)
    }

    /// Create-only record write with conflict re-read: adopt or verify the
    /// stored record when another writer got there first.
    async fn create_or_adopt<T>(
        &self,
        get_operation: &'static str,
        create_operation: &'static str,
        key: String,
        value: &T,
        policy: AdoptPolicy,
    ) -> Result<T, OperationStatusStoreError>
    where
        T: Serialize + DeserializeOwned + Clone + PartialEq,
    {
        if let Some(existing) = self
            .get_record::<T>(
                get_operation,
                key.clone(),
                OperationStatusStoreError::DecodeSubmission,
            )
            .await?
        {
            return adopt_record(existing, value, policy);
        }

        let payload =
            serde_json::to_vec(value).map_err(OperationStatusStoreError::EncodeSubmission)?;
        match with_io_timeout(create_operation, self.bucket.create(&key, payload.into())).await? {
            Ok(_) => Ok(value.clone()),
            Err(error) => {
                let Some(existing) = self
                    .get_record::<T>(
                        get_operation,
                        key,
                        OperationStatusStoreError::DecodeSubmission,
                    )
                    .await?
                else {
                    return Err(OperationStatusStoreError::CasConflict {
                        message: error.to_string(),
                    });
                };
                adopt_record(existing, value, policy)
            }
        }
    }

    pub async fn put_if_newer(
        &self,
        status: &OperationStatus,
    ) -> Result<OperationStatusWrite, OperationStatusStoreError> {
        let key = operation_status_key(status.id());
        let incoming_sequence = status.last_event_sequence();
        let payload =
            serde_json::to_vec(status).map_err(OperationStatusStoreError::EncodeStatus)?;
        let Some(existing) = with_io_timeout(
            "operation status entry read",
            self.bucket.entry(key.clone()),
        )
        .await?
        .map_err(|error| OperationStatusStoreError::GetStatus {
            message: error.to_string(),
        })?
        else {
            let revision = match with_io_timeout(
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
        let current_sequence = current.last_event_sequence();
        if current_sequence >= incoming_sequence {
            return Ok(OperationStatusWrite::Stale {
                current_sequence,
                attempted_sequence: incoming_sequence,
            });
        }

        let revision = match with_io_timeout(
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
        self.get_record(
            "deploy submission get",
            deploy_submission_key(idempotency_key),
            OperationStatusStoreError::DecodeSubmission,
        )
        .await
    }

    pub async fn put_deploy_submission_if_absent(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        submission: &StoredDeploySubmission,
    ) -> Result<StoredDeploySubmission, OperationStatusStoreError> {
        self.create_or_adopt(
            "deploy submission get",
            "deploy submission create",
            deploy_submission_key(idempotency_key),
            submission,
            AdoptPolicy::AdoptExisting,
        )
        .await
    }

    pub async fn cert_submission(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredCertSubmission>, OperationStatusStoreError> {
        self.get_record(
            "cert submission get",
            cert_submission_key(idempotency_key),
            OperationStatusStoreError::DecodeSubmission,
        )
        .await
    }

    pub async fn put_cert_submission_if_absent(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        submission: &StoredCertSubmission,
    ) -> Result<StoredCertSubmission, OperationStatusStoreError> {
        self.create_or_adopt(
            "cert submission get",
            "cert submission create",
            cert_submission_key(idempotency_key),
            submission,
            AdoptPolicy::AdoptExisting,
        )
        .await
    }

    pub async fn backup_submission(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredBackupSubmission>, OperationStatusStoreError> {
        self.get_record(
            "backup submission get",
            backup_submission_key(idempotency_key),
            OperationStatusStoreError::DecodeSubmission,
        )
        .await
    }

    pub async fn put_backup_submission_if_absent(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        submission: &StoredBackupSubmission,
    ) -> Result<StoredBackupSubmission, OperationStatusStoreError> {
        self.create_or_adopt(
            "backup submission get",
            "backup submission create",
            backup_submission_key(idempotency_key),
            submission,
            AdoptPolicy::AdoptExisting,
        )
        .await
    }

    pub async fn machine_add_submission(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredMachineAddSubmission>, OperationStatusStoreError> {
        self.get_record(
            "machine add submission get",
            machine_add_submission_key(idempotency_key),
            OperationStatusStoreError::DecodeSubmission,
        )
        .await
    }

    pub async fn put_machine_add_submission_if_absent(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        submission: &StoredMachineAddSubmission,
    ) -> Result<StoredMachineAddSubmission, OperationStatusStoreError> {
        self.create_or_adopt(
            "machine add submission get",
            "machine add submission create",
            machine_add_submission_key(idempotency_key),
            submission,
            AdoptPolicy::AdoptExisting,
        )
        .await
    }

    /// Every stored machine-add submission. Used by control-start mint
    /// reconciliation to find accepted machine-adds whose mint worker died
    /// with a previous control process.
    pub async fn machine_add_submissions(
        &self,
    ) -> Result<Vec<StoredMachineAddSubmission>, OperationStatusStoreError> {
        let entries = bounded_bucket_key_scan_entries_with_prefix(
            &self.bucket,
            MACHINE_ADD_SUBMISSION_KEY_PREFIX,
        )
        .await
        .map_err(|error| OperationStatusStoreError::GetStatus {
            message: error.message,
        })?;

        let mut submissions = Vec::with_capacity(entries.len());
        for entry in entries {
            submissions.push(
                serde_json::from_slice::<StoredMachineAddSubmission>(&entry.value)
                    .map_err(OperationStatusStoreError::DecodeSubmission)?,
            );
        }
        Ok(submissions)
    }

    pub async fn machine_add_mint_claim(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredMachineAddMintClaim>, OperationStatusStoreError> {
        self.get_record(
            "machine add mint claim get",
            machine_add_mint_claim_key(idempotency_key),
            OperationStatusStoreError::DecodeSubmission,
        )
        .await
    }

    /// Create-only claim of minted credential material for one idempotency
    /// key. The first writer wins; later writers receive the winning claim
    /// and must continue with it instead of their own candidate.
    pub async fn put_machine_add_mint_claim_if_absent(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        claim: &StoredMachineAddMintClaim,
    ) -> Result<StoredMachineAddMintClaim, OperationStatusStoreError> {
        self.create_or_adopt(
            "machine add mint claim get",
            "machine add mint claim create",
            machine_add_mint_claim_key(idempotency_key),
            claim,
            AdoptPolicy::AdoptExisting,
        )
        .await
    }

    pub async fn machine_add_secret_delivery(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredMachineAddSecretDelivery>, OperationStatusStoreError> {
        self.get_record(
            "machine add secret delivery get",
            machine_add_secret_delivery_key(idempotency_key),
            OperationStatusStoreError::DecodeSubmission,
        )
        .await
    }

    pub async fn put_machine_add_secret_delivery_if_absent(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        secret_delivery: &StoredMachineAddSecretDelivery,
    ) -> Result<StoredMachineAddSecretDelivery, OperationStatusStoreError> {
        self.create_or_adopt(
            "machine add secret delivery get",
            "machine add secret delivery create",
            machine_add_secret_delivery_key(idempotency_key),
            secret_delivery,
            AdoptPolicy::RequireEqual {
                conflict_message: "machine add secret delivery is already assigned",
            },
        )
        .await
    }

    pub async fn delete_machine_add_secret_delivery(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<(), OperationStatusStoreError> {
        with_io_timeout(
            "machine add secret delivery delete",
            self.bucket
                .delete(machine_add_secret_delivery_key(idempotency_key)),
        )
        .await?
        .map_err(|error| OperationStatusStoreError::CasConflict {
            message: error.to_string(),
        })
    }

    pub async fn put_machine_add_join_token_if_absent(
        &self,
        fingerprint: &JoinTokenFingerprint,
        token: &StoredMachineAddJoinToken,
    ) -> Result<StoredMachineAddJoinToken, OperationStatusStoreError> {
        self.create_or_adopt(
            "machine add join token index get",
            "machine add join token index create",
            machine_add_join_token_key(fingerprint),
            token,
            AdoptPolicy::RequireEqual {
                conflict_message: "join token fingerprint is already assigned",
            },
        )
        .await
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
        self.get_record(
            "machine add join token index get",
            machine_add_join_token_key(fingerprint),
            OperationStatusStoreError::DecodeSubmission,
        )
        .await
    }

    pub async fn record_machine_add_submission_sequence(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        sequence: EventSequence,
    ) -> Result<StoredMachineAddSubmission, OperationStatusStoreError> {
        let key = machine_add_submission_key(idempotency_key);
        let Some(existing) = with_io_timeout(
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
        match with_io_timeout(
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
            with_io_timeout("operation owner lease read", self.bucket.entry(key.clone()))
                .await?
                .map_err(|error| OperationStatusStoreError::GetStatus {
                    message: error.to_string(),
                })?
        else {
            return match with_io_timeout(
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

        match with_io_timeout(
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
            with_io_timeout("operation owner lease read", self.bucket.entry(key.clone()))
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
        match with_io_timeout(
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
        self.get_record(
            "operation owner lease get",
            operation_owner_lease_key(operation_id),
            OperationStatusStoreError::DecodeLease,
        )
        .await
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
            with_io_timeout("operation status conflict read", self.bucket.entry(key))
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
        let current_sequence = current.last_event_sequence();
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
        let Some(payload) = with_io_timeout(
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

fn adopt_record<T>(
    existing: T,
    candidate: &T,
    policy: AdoptPolicy,
) -> Result<T, OperationStatusStoreError>
where
    T: PartialEq,
{
    match policy {
        AdoptPolicy::AdoptExisting => Ok(existing),
        AdoptPolicy::RequireEqual { conflict_message } => {
            if existing == *candidate {
                Ok(existing)
            } else {
                Err(OperationStatusStoreError::CasConflict {
                    message: conflict_message.to_owned(),
                })
            }
        }
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

impl From<NatsIoTimeout> for OperationStatusStoreError {
    fn from(timeout: NatsIoTimeout) -> Self {
        Self::Timeout {
            operation: timeout.operation,
        }
    }
}

impl From<NatsIoTimeout> for OperationStatusReadError {
    fn from(timeout: NatsIoTimeout) -> Self {
        Self::Timeout {
            operation: timeout.operation,
        }
    }
}
