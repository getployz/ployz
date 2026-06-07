//! NATS-backed latest observation adapters.

use crate::kv::bounded_bucket_key_scan_entries_with_prefix;
use async_nats::jetstream;
use async_nats::jetstream::kv::Operation;
use ployz_core::ids::{ContainerId, NodeId};
use ployz_core::node::{ManagedContainerObservation, NodeContainerObservationSnapshot};
use ployz_core::state::NodeContainerObservationKey;
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;

pub const KV_OBS_BUCKET: &str = "KV_OBS";
const NATS_OBSERVATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct AsyncNatsObservationStore {
    bucket: jetstream::kv::Store,
}

impl AsyncNatsObservationStore {
    pub async fn from_jetstream(
        jetstream: &jetstream::Context,
    ) -> Result<Self, ObservationStoreError> {
        let bucket = with_observation_timeout(
            "observation bucket open",
            jetstream.get_key_value(KV_OBS_BUCKET),
        )
        .await?
        .map_err(|error| ObservationStoreError::OpenBucket {
            bucket: KV_OBS_BUCKET,
            message: error.to_string(),
        })?;

        Ok(Self { bucket })
    }

    #[must_use]
    pub fn new(bucket: jetstream::kv::Store) -> Self {
        Self { bucket }
    }

    pub async fn replace_node_containers(
        &self,
        snapshot: &NodeContainerObservationSnapshot,
    ) -> Result<(), ObservationStoreError> {
        let key = NodeContainerObservationKey::from_node_id(snapshot.node_id());
        let payload = serde_json::to_vec(snapshot).map_err(ObservationStoreError::Encode)?;
        with_observation_timeout(
            "node observation snapshot put",
            self.bucket.put(key.as_str(), payload.into()),
        )
        .await?
        .map_err(|error| ObservationStoreError::Put {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?;

        Ok(())
    }

    pub async fn container(
        &self,
        node_id: &NodeId,
        container_id: &ContainerId,
    ) -> Result<Option<ManagedContainerObservation>, ObservationStoreError> {
        let Some(snapshot) = self.node_snapshot(node_id).await? else {
            return Ok(None);
        };

        Ok(snapshot.container(container_id).cloned())
    }

    pub async fn node_snapshot(
        &self,
        node_id: &NodeId,
    ) -> Result<Option<NodeContainerObservationSnapshot>, ObservationStoreError> {
        let key = NodeContainerObservationKey::from_node_id(node_id);
        let Some(payload) = with_observation_timeout(
            "node observation snapshot get",
            self.bucket.get(key.as_str()),
        )
        .await?
        .map_err(|error| ObservationStoreError::Get {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?
        else {
            return Ok(None);
        };

        serde_json::from_slice(&payload)
            .map(Some)
            .map_err(ObservationStoreError::Decode)
    }

    pub async fn node_snapshots(
        &self,
    ) -> Result<Vec<NodeContainerObservationSnapshot>, ObservationStoreError> {
        Ok(self
            .node_snapshot_records()
            .await?
            .into_iter()
            .map(|record| record.snapshot)
            .collect())
    }

    pub async fn node_snapshot_records(
        &self,
    ) -> Result<Vec<NodeContainerObservationRecord>, ObservationStoreError> {
        let entries = bounded_bucket_key_scan_entries_with_prefix(
            &self.bucket,
            "containers.",
            NATS_OBSERVATION_TIMEOUT,
        )
        .await
        .map_err(|error| ObservationStoreError::ListKeys {
            message: error.message,
        })?;
        let current_entries = current_kv_entries(entries);
        let mut records = Vec::new();

        for entry in current_entries.into_values() {
            let snapshot: NodeContainerObservationSnapshot =
                serde_json::from_slice(&entry.value).map_err(ObservationStoreError::Decode)?;
            let actual_key = NodeContainerObservationKey::from_node_id(snapshot.node_id());
            if entry.key != actual_key.as_str() {
                return Err(ObservationStoreError::CorruptNodeSnapshotKey {
                    key: entry.key,
                    actual_key: actual_key.as_str().to_owned(),
                });
            }
            records.push(NodeContainerObservationRecord {
                snapshot,
                observed_at_unix_nanos: entry.created.unix_timestamp_nanos(),
                revision: entry.revision,
            });
        }

        records.sort_by(|left, right| {
            left.snapshot
                .node_id()
                .cmp(right.snapshot.node_id())
                .then_with(|| left.revision.cmp(&right.revision))
        });
        Ok(records)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeContainerObservationRecord {
    pub snapshot: NodeContainerObservationSnapshot,
    pub observed_at_unix_nanos: i128,
    pub revision: u64,
}

fn current_kv_entries(
    entries: impl IntoIterator<Item = jetstream::kv::Entry>,
) -> BTreeMap<String, jetstream::kv::Entry> {
    let mut current = BTreeMap::new();

    for entry in entries {
        match entry.operation {
            Operation::Put => {
                current.insert(entry.key.clone(), entry);
            }
            Operation::Delete | Operation::Purge => {
                current.remove(&entry.key);
            }
        }
    }

    current
}

#[derive(Debug)]
pub enum ObservationStoreError {
    OpenBucket {
        bucket: &'static str,
        message: String,
    },
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    ListKeys {
        message: String,
    },
    Put {
        key: String,
        message: String,
    },
    Get {
        key: String,
        message: String,
    },
    CorruptNodeSnapshotKey {
        key: String,
        actual_key: String,
    },
    Timeout {
        operation: &'static str,
    },
}

impl fmt::Display for ObservationStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenBucket { bucket, message } => {
                write!(formatter, "open bucket {bucket}: {message}")
            }
            Self::Encode(error) => write!(formatter, "encode observation snapshot: {error}"),
            Self::Decode(error) => write!(formatter, "decode observation snapshot: {error}"),
            Self::ListKeys { message } => {
                write!(formatter, "list node observation keys: {message}")
            }
            Self::Put { key, message } => write!(formatter, "put {key}: {message}"),
            Self::Get { key, message } => write!(formatter, "get {key}: {message}"),
            Self::CorruptNodeSnapshotKey { key, actual_key } => write!(
                formatter,
                "node observation snapshot key {} does not match snapshot key {}",
                key, actual_key
            ),
            Self::Timeout { operation } => write!(formatter, "{operation} timed out"),
        }
    }
}

async fn with_observation_timeout<T>(
    operation: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, ObservationStoreError> {
    tokio::time::timeout(NATS_OBSERVATION_TIMEOUT, future)
        .await
        .map_err(|_| ObservationStoreError::Timeout { operation })
}
