//! JetStream KV helpers.

use async_nats::jetstream;
use futures_util::TryStreamExt;
use std::future::Future;
use std::time::Duration;

use crate::bootstrap::ResourceReplicas;

pub const KV_CORE_BUCKET: &str = "KV_CORE";
pub const KV_LOCKS_BUCKET: &str = "KV_LOCKS";

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

pub async fn bounded_bucket_key_scan_entries_with_prefix(
    bucket: &jetstream::kv::Store,
    prefix: &str,
    timeout: Duration,
) -> Result<Vec<jetstream::kv::Entry>, KvScanError> {
    with_kv_scan_timeout(
        "kv prefix scan",
        async {
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

            Ok(entries)
        },
        timeout,
    )
    .await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvScanError {
    pub message: String,
}

async fn with_kv_scan_timeout<T>(
    operation: &'static str,
    future: impl Future<Output = Result<T, KvScanError>>,
    timeout: Duration,
) -> Result<T, KvScanError> {
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| KvScanError {
            message: format!("{operation} timed out"),
        })?
}
