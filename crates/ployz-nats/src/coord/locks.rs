use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::kv;
use ployz_types::error::{Error, Result};
use ployz_types::model::MachineId;
use ployz_types::spec::Namespace;
use ployz_types::time::now_unix_secs;
use tokio::sync::Mutex;

use crate::NatsStore;
use crate::buckets::LOCKS_BUCKET;
use crate::store::kv_json;
use crate::subjects;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseValue {
    pub owner: String,
    pub nonce: String,
    pub expires_at: u64,
}

pub struct NatsDeployLock {
    locks: NatsLocks,
    lease: Arc<Mutex<Option<Lease>>>,
}

impl NatsDeployLock {
    pub async fn acquire(
        locks: NatsLocks,
        namespace: &Namespace,
        nonce: &str,
        owner: &MachineId,
        ttl: Duration,
    ) -> Result<Self> {
        let lease = locks
            .acquire(
                &subjects::deploy_lock(namespace),
                owner.0.clone(),
                nonce.to_string(),
                ttl,
                now_unix_secs().saturating_add(ttl.as_secs()),
            )
            .await?;
        Ok(Self {
            locks,
            lease: Arc::new(Mutex::new(Some(lease))),
        })
    }

    pub async fn renew(&self, ttl: Duration) -> Result<()> {
        let Some(lease) = self.lease.lock().await.clone() else {
            return Ok(());
        };
        let renewed = self
            .locks
            .renew(&lease, ttl, now_unix_secs().saturating_add(ttl.as_secs()))
            .await?;
        let mut current = self.lease.lock().await;
        let Some(current_lease) = current.as_ref() else {
            return Ok(());
        };
        if current_lease.revision != lease.revision
            || current_lease.value.nonce != lease.value.nonce
        {
            return Err(Error::operation(
                "nats_deploy_lock_renew",
                "deploy lock changed before renewal completed",
            ));
        }
        *current = Some(renewed);
        Ok(())
    }

    pub async fn release(self) -> Result<()> {
        let Some(lease) = self.lease.lock().await.take() else {
            return Ok(());
        };
        self.locks.release(lease).await
    }
}

impl Clone for NatsDeployLock {
    fn clone(&self) -> Self {
        Self {
            locks: self.locks.clone(),
            lease: self.lease.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    key: String,
    revision: u64,
    value: LeaseValue,
}

impl Lease {
    #[must_use]
    pub fn new(key: impl Into<String>, revision: u64, value: LeaseValue) -> Self {
        Self {
            key: key.into(),
            revision,
            value,
        }
    }

    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn nonce(&self) -> &str {
        &self.value.nonce
    }

    #[must_use]
    pub fn into_release_guard(self) -> ReleaseGuard {
        ReleaseGuard {
            key: self.key,
            expected_revision: self.revision,
            expected_nonce: self.value.nonce,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseGuard {
    pub key: String,
    pub expected_revision: u64,
    pub expected_nonce: String,
}

#[must_use]
pub fn release_is_allowed(
    current_revision: u64,
    current: &LeaseValue,
    guard: &ReleaseGuard,
) -> bool {
    current_revision == guard.expected_revision && current.nonce == guard.expected_nonce
}

#[derive(Clone)]
pub struct NatsLocks {
    bucket: kv::Store,
}

impl NatsLocks {
    pub async fn new(store: &NatsStore) -> Result<Self> {
        let bucket =
            kv_json::get_bucket(store.jetstream(), LOCKS_BUCKET, "nats_locks_bucket").await?;
        Ok(Self { bucket })
    }

    pub async fn acquire(
        &self,
        key: &str,
        owner: impl Into<String>,
        nonce: impl Into<String>,
        ttl: Duration,
        expires_at: u64,
    ) -> Result<Lease> {
        let now = expires_at.saturating_sub(ttl.as_secs());
        let value = LeaseValue {
            owner: owner.into(),
            nonce: nonce.into(),
            expires_at,
        };
        let payload = serde_json::to_vec(&value)
            .map_err(|error| Error::operation("nats_lock_encode", error.to_string()))?;
        let revision = match self.bucket.create(key, payload.clone().into()).await {
            Ok(revision) => revision,
            Err(create_error) => {
                let Some(entry) = self.bucket.entry(key).await.map_err(|error| {
                    Error::operation("nats_lock_read_for_acquire", format!("{error:?}"))
                })?
                else {
                    return Err(Error::operation(
                        "nats_lock_acquire",
                        format!("{create_error:?}"),
                    ));
                };
                match entry.operation {
                    kv::Operation::Put => {
                        let current: LeaseValue =
                            kv_json::decode_json("nats_lock_decode", entry.value.as_ref())?;
                        if current.expires_at > now {
                            return Err(Error::operation(
                                "nats_lock_acquire",
                                format!("lock '{key}' is already held"),
                            ));
                        }
                    }
                    kv::Operation::Delete | kv::Operation::Purge => {}
                }
                self.bucket
                    .update(key, payload.into(), entry.revision)
                    .await
                    .map_err(|error| {
                        Error::operation(
                            "nats_lock_acquire",
                            format!("lock '{key}' contention: {error:?}"),
                        )
                    })?
            }
        };
        Ok(Lease::new(key, revision, value))
    }

    pub async fn renew(&self, lease: &Lease, _ttl: Duration, expires_at: u64) -> Result<Lease> {
        let value = LeaseValue {
            owner: lease.value.owner.clone(),
            nonce: lease.value.nonce.clone(),
            expires_at,
        };
        let payload = serde_json::to_vec(&value)
            .map_err(|error| Error::operation("nats_lock_encode", error.to_string()))?;
        let revision = self
            .bucket
            .update(&lease.key, payload.into(), lease.revision)
            .await
            .map_err(|error| Error::operation("nats_lock_renew", format!("{error:?}")))?;
        Ok(Lease::new(lease.key.clone(), revision, value))
    }

    pub async fn release(&self, lease: Lease) -> Result<()> {
        let guard = lease.into_release_guard();
        let Some(entry) = self
            .bucket
            .entry(guard.key.clone())
            .await
            .map_err(|error| {
                Error::operation("nats_lock_read_for_release", format!("{error:?}"))
            })?
        else {
            return Ok(());
        };
        let current: LeaseValue = kv_json::decode_json("nats_lock_decode", entry.value.as_ref())?;
        if !release_is_allowed(entry.revision, &current, &guard) {
            return Err(Error::operation(
                "nats_lock_release",
                format!(
                    "lock '{}' is held by another lease; refusing stale release",
                    guard.key
                ),
            ));
        }
        self.bucket
            .delete_expect_revision(&guard.key, Some(guard.expected_revision))
            .await
            .map_err(|error| Error::operation("nats_lock_release", format!("{error:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_holder_cannot_release_newer_lease() {
        let stale = Lease::new(
            "locks.deploy.prod",
            7,
            LeaseValue {
                owner: "a".into(),
                nonce: "old".into(),
                expires_at: 10,
            },
        )
        .into_release_guard();
        let current = LeaseValue {
            owner: "b".into(),
            nonce: "new".into(),
            expires_at: 20,
        };
        assert!(!release_is_allowed(8, &current, &stale));
    }
}
