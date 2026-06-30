//! JetStream KV helpers.

use async_nats::jetstream;
use futures_util::TryStreamExt;
use std::future::Future;
use std::time::Duration;

use crate::bootstrap::ResourceReplicas;

pub use ployz_core::state::KV_CORE_BUCKET;

pub(crate) const NATS_IO_TIMEOUT: Duration = Duration::from_secs(10);

/// One NATS I/O call exceeded [`NATS_IO_TIMEOUT`]. Each store error enum
/// converts this into its own `Timeout` variant via `From`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NatsIoTimeout {
    pub(crate) operation: &'static str,
}

pub(crate) async fn with_io_timeout<T>(
    operation: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, NatsIoTimeout> {
    tokio::time::timeout(NATS_IO_TIMEOUT, future)
        .await
        .map_err(|_| NatsIoTimeout { operation })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvBucketSpec {
    pub name: &'static str,
    pub(crate) replicas: ResourceReplicas,
}

impl KvBucketSpec {
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            replicas: ResourceReplicas::SINGLE_CORE,
        }
    }

    #[must_use]
    pub const fn replicas(&self) -> ResourceReplicas {
        self.replicas
    }

    #[must_use]
    pub(crate) const fn with_observed_replicas(mut self, replicas: ResourceReplicas) -> Self {
        self.replicas = replicas;
        self
    }
}

/// Scans every key under `prefix` and returns only the current (`Put`)
/// entries, in key order. Deleted and purged keys are dropped.
pub async fn bounded_bucket_key_scan_entries_with_prefix(
    bucket: &jetstream::kv::Store,
    prefix: &str,
) -> Result<Vec<jetstream::kv::Entry>, KvScanError> {
    let scan = async {
        let keys = bucket.keys().await.map_err(|error| KvScanError {
            message: error.to_string(),
        })?;
        let keys = keys
            .try_collect::<Vec<String>>()
            .await
            .map_err(|error| KvScanError {
                message: error.to_string(),
            })?;
        let mut entries = Vec::new();

        for key in keys.into_iter().filter(|key| key.starts_with(prefix)) {
            let Some(entry) = bucket.entry(&key).await.map_err(|error| KvScanError {
                message: error.to_string(),
            })?
            else {
                continue;
            };
            entries.push(entry);
        }

        Ok::<Vec<jetstream::kv::Entry>, KvScanError>(entries)
    };
    with_io_timeout("kv prefix scan", scan).await?
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvScanError {
    pub message: String,
}

impl From<NatsIoTimeout> for KvScanError {
    fn from(timeout: NatsIoTimeout) -> Self {
        Self {
            message: format!("{} timed out", timeout.operation),
        }
    }
}

/// Scans current entries under `prefix`, decodes each as `T`, verifies the
/// stored key matches the canonical key derived from the decoded value, and
/// returns the values sorted by `sort_key`.
pub(crate) async fn list_current<T, K>(
    bucket: &jetstream::kv::Store,
    prefix: &str,
    canonical_key: impl Fn(&T) -> String,
    sort_key: impl Fn(&T) -> K,
) -> Result<Vec<T>, KvListError>
where
    T: serde::de::DeserializeOwned,
    K: Ord,
{
    let entries = bounded_bucket_key_scan_entries_with_prefix(bucket, prefix)
        .await
        .map_err(|error| KvListError::Scan {
            message: error.message,
        })?;
    let mut values = Vec::with_capacity(entries.len());

    for entry in entries {
        let value: T = serde_json::from_slice(&entry.value).map_err(KvListError::Decode)?;
        let actual_key = canonical_key(&value);
        if entry.key != actual_key {
            return Err(KvListError::CorruptKey {
                key: entry.key,
                actual_key,
            });
        }
        values.push(value);
    }

    values.sort_by_key(|value| sort_key(value));
    Ok(values)
}

#[derive(Debug)]
pub(crate) enum KvListError {
    Scan { message: String },
    Decode(serde_json::Error),
    CorruptKey { key: String, actual_key: String },
}
