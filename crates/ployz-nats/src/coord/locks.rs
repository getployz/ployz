use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::kv;
use async_nats::{HeaderMap, header};
use ployz_types::error::{Error, Result};
use ployz_types::model::MachineId;
use ployz_types::spec::Namespace;
use ployz_types::time::now_unix_secs;
use tokio::sync::Mutex;

use crate::NatsStore;
use crate::buckets::LOCKS_BUCKET;
use crate::config;
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
    jetstream: async_nats::jetstream::Context,
    bucket: kv::Store,
}

impl NatsLocks {
    pub async fn new(store: &NatsStore) -> Result<Self> {
        let bucket =
            kv_json::get_bucket(store.jetstream(), LOCKS_BUCKET, "nats_locks_bucket").await?;
        Ok(Self {
            jetstream: store.jetstream().clone(),
            bucket,
        })
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
        let revision = match self.update_with_ttl(key, payload.clone(), 0, ttl).await {
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
                self.update_with_ttl(key, payload, entry.revision, ttl)
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

    pub async fn renew(&self, lease: &Lease, ttl: Duration, expires_at: u64) -> Result<Lease> {
        let value = LeaseValue {
            owner: lease.value.owner.clone(),
            nonce: lease.value.nonce.clone(),
            expires_at,
        };
        let payload = serde_json::to_vec(&value)
            .map_err(|error| Error::operation("nats_lock_encode", error.to_string()))?;
        let revision = self
            .update_with_ttl(&lease.key, payload, lease.revision, ttl)
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

    async fn update_with_ttl(
        &self,
        key: &str,
        value: Vec<u8>,
        revision: u64,
        ttl: Duration,
    ) -> Result<u64> {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::NATS_EXPECTED_LAST_SUBJECT_SEQUENCE,
            revision.to_string(),
        );
        headers.insert(header::NATS_MESSAGE_TTL, ttl.as_secs().to_string());
        self.jetstream
            .publish_with_headers(kv_put_subject(&self.bucket, key), headers, value.into())
            .await
            .map_err(|error| Error::operation("nats_lock_publish", format!("{error:?}")))?
            .await
            .map(|ack| ack.sequence)
            .map_err(|error| Error::operation("nats_lock_publish_ack", format!("{error:?}")))
    }
}

fn kv_put_subject(bucket: &kv::Store, key: &str) -> String {
    kv_put_subject_parts(
        bucket.use_jetstream_prefix,
        bucket.put_prefix.as_deref(),
        &bucket.prefix,
        key,
    )
}

fn kv_put_subject_parts(
    use_jetstream_prefix: bool,
    put_prefix: Option<&str>,
    prefix: &str,
    key: &str,
) -> String {
    let mut subject = String::new();
    if use_jetstream_prefix {
        subject.push_str("$JS.");
        subject.push_str(config::HUB_DOMAIN);
        subject.push_str(".API.");
    }
    subject.push_str(put_prefix.unwrap_or(prefix));
    subject.push_str(key);
    subject
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_guard_requires_revision_and_nonce() {
        let current = LeaseValue {
            owner: "node-a".into(),
            nonce: "nonce-a".into(),
            expires_at: 10,
        };
        let guard = ReleaseGuard {
            key: "locks.deploy.default".into(),
            expected_revision: 7,
            expected_nonce: "nonce-a".into(),
        };

        assert!(release_is_allowed(7, &current, &guard));
        assert!(!release_is_allowed(8, &current, &guard));

        let stale_nonce = LeaseValue {
            nonce: "nonce-b".into(),
            ..current
        };
        assert!(!release_is_allowed(7, &stale_nonce, &guard));
    }

    #[test]
    fn kv_subject_uses_hub_prefix_for_domain_scoped_bucket() {
        assert_eq!(
            kv_put_subject_parts(true, None, "$KV.locks.", "locks.deploy.default"),
            "$JS.hub.API.$KV.locks.locks.deploy.default"
        );
    }

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
