use super::{AsyncNatsCoreStateStore, CoreStateStoreError};
use crate::kv::with_io_timeout;
use ployz_core::ids::{NamespaceId, OperationId};
use ployz_core::state::{NamespaceLockState, NamespaceLockStateKey};

pub const NAMESPACE_LOCK_TTL_MS: u64 = 30_000;
pub const NAMESPACE_LOCK_RENEW_INTERVAL_MS: u64 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceLockAcquire {
    Acquired,
    Busy { owner: OperationId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceLockRenew {
    Renewed,
    Lost,
}

impl AsyncNatsCoreStateStore {
    pub async fn acquire_namespace_lock(
        &self,
        namespace_id: &NamespaceId,
        operation_id: &OperationId,
        now_unix_ms: u64,
    ) -> Result<NamespaceLockAcquire, CoreStateStoreError> {
        let key = NamespaceLockStateKey::from_namespace_id(namespace_id);
        let state = namespace_lock_state(namespace_id, operation_id, now_unix_ms);
        let payload = serde_json::to_vec(&state).map_err(CoreStateStoreError::Encode)?;

        match with_io_timeout(
            "namespace lock create",
            self.bucket.create(key.as_str(), payload.clone().into()),
        )
        .await?
        {
            Ok(_) => return Ok(NamespaceLockAcquire::Acquired),
            Err(_) => {}
        }

        let Some(existing) = with_io_timeout(
            "namespace lock conflict read",
            self.bucket.entry(key.as_str()),
        )
        .await?
        .map_err(|error| CoreStateStoreError::Get {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?
        else {
            return Err(CoreStateStoreError::CasConflict {
                message: "namespace lock create conflicted but no lock was readable".to_owned(),
            });
        };
        let current = decode_namespace_lock(&key, &existing.value)?;
        if current.operation_id != *operation_id && current.expires_at_unix_ms > now_unix_ms {
            return Ok(NamespaceLockAcquire::Busy {
                owner: current.operation_id,
            });
        }

        match with_io_timeout(
            "namespace lock update",
            self.bucket
                .update(key.as_str(), payload.into(), existing.revision),
        )
        .await?
        {
            Ok(_) => Ok(NamespaceLockAcquire::Acquired),
            Err(error) => Err(CoreStateStoreError::CasConflict {
                message: error.to_string(),
            }),
        }
    }

    pub async fn renew_namespace_lock(
        &self,
        namespace_id: &NamespaceId,
        operation_id: &OperationId,
        now_unix_ms: u64,
    ) -> Result<NamespaceLockRenew, CoreStateStoreError> {
        let key = NamespaceLockStateKey::from_namespace_id(namespace_id);
        let Some(existing) =
            with_io_timeout("namespace lock renew read", self.bucket.entry(key.as_str()))
                .await?
                .map_err(|error| CoreStateStoreError::Get {
                    key: key.as_str().to_owned(),
                    message: error.to_string(),
                })?
        else {
            return Ok(NamespaceLockRenew::Lost);
        };
        let current = decode_namespace_lock(&key, &existing.value)?;
        if current.operation_id != *operation_id {
            return Ok(NamespaceLockRenew::Lost);
        }

        let state = namespace_lock_state(namespace_id, operation_id, now_unix_ms);
        let payload = serde_json::to_vec(&state).map_err(CoreStateStoreError::Encode)?;
        match with_io_timeout(
            "namespace lock renew update",
            self.bucket
                .update(key.as_str(), payload.into(), existing.revision),
        )
        .await?
        {
            Ok(_) => Ok(NamespaceLockRenew::Renewed),
            Err(error) => {
                let Some(existing) = with_io_timeout(
                    "namespace lock renew conflict read",
                    self.bucket.entry(key.as_str()),
                )
                .await?
                .map_err(|error| CoreStateStoreError::Get {
                    key: key.as_str().to_owned(),
                    message: error.to_string(),
                })?
                else {
                    return Ok(NamespaceLockRenew::Lost);
                };
                let current = decode_namespace_lock(&key, &existing.value)?;
                if current.operation_id != *operation_id {
                    return Ok(NamespaceLockRenew::Lost);
                }

                if current.expires_at_unix_ms >= state.expires_at_unix_ms {
                    return Ok(NamespaceLockRenew::Renewed);
                }

                Err(CoreStateStoreError::CasConflict {
                    message: error.to_string(),
                })
            }
        }
    }

    pub async fn release_namespace_lock(
        &self,
        namespace_id: &NamespaceId,
        operation_id: &OperationId,
    ) -> Result<(), CoreStateStoreError> {
        let key = NamespaceLockStateKey::from_namespace_id(namespace_id);
        let Some(existing) = with_io_timeout(
            "namespace lock release read",
            self.bucket.entry(key.as_str()),
        )
        .await?
        .map_err(|error| CoreStateStoreError::Get {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?
        else {
            return Ok(());
        };
        let current = decode_namespace_lock(&key, &existing.value)?;
        if current.operation_id != *operation_id {
            return Ok(());
        }

        with_io_timeout(
            "namespace lock release delete",
            self.bucket
                .delete_expect_revision(key.as_str(), Some(existing.revision)),
        )
        .await?
        .map_err(|error| CoreStateStoreError::Delete {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?;

        Ok(())
    }
}

fn namespace_lock_state(
    namespace_id: &NamespaceId,
    operation_id: &OperationId,
    now_unix_ms: u64,
) -> NamespaceLockState {
    NamespaceLockState {
        namespace_id: namespace_id.clone(),
        operation_id: operation_id.clone(),
        expires_at_unix_ms: now_unix_ms.saturating_add(NAMESPACE_LOCK_TTL_MS),
    }
}

fn decode_namespace_lock(
    key: &NamespaceLockStateKey,
    payload: &[u8],
) -> Result<NamespaceLockState, CoreStateStoreError> {
    serde_json::from_slice(payload).map_err(|error| CoreStateStoreError::CorruptNamespaceLock {
        key: key.as_str().to_owned(),
        message: error.to_string(),
    })
}
