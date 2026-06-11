//! NATS-backed latest observation adapters.

use crate::kv::{
    KvListError, NatsIoTimeout, bounded_bucket_key_scan_entries_with_prefix, list_current,
    with_io_timeout,
};
use async_nats::jetstream;
use ployz_core::ids::{ContainerId, NodeId};
use ployz_core::node::{ManagedContainerObservation, NodeContainerObservationSnapshot};
use ployz_core::state::{
    GATEWAY_STATUS_OBSERVATION_PREFIX, GatewayStatusObservation, GatewayStatusObservationKey,
    NODE_CONTAINER_OBSERVATION_PREFIX, NODE_PUBLIC_IP_OBSERVATION_PREFIX,
    NodeContainerObservationKey, NodePublicIpObservation, NodePublicIpObservationKey,
};
use std::fmt;

pub use ployz_core::state::KV_OBS_BUCKET;

#[derive(Debug, Clone)]
pub struct AsyncNatsObservationStore {
    bucket: jetstream::kv::Store,
}

impl AsyncNatsObservationStore {
    pub async fn from_jetstream(
        jetstream: &jetstream::Context,
    ) -> Result<Self, ObservationStoreError> {
        let bucket = with_io_timeout(
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
        with_io_timeout(
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

    pub async fn replace_node_public_ip(
        &self,
        observation: &NodePublicIpObservation,
    ) -> Result<(), ObservationStoreError> {
        let key = NodePublicIpObservationKey::from_node_id(&observation.node_id);
        put_observation(&self.bucket, key.as_str(), observation).await
    }

    pub async fn clear_node_public_ip(
        &self,
        node_id: &NodeId,
    ) -> Result<(), ObservationStoreError> {
        if self.node_public_ip(node_id).await?.is_none() {
            return Ok(());
        }

        let key = NodePublicIpObservationKey::from_node_id(node_id);
        with_io_timeout("node public ip delete", self.bucket.delete(key.as_str()))
            .await?
            .map_err(|error| ObservationStoreError::Delete {
                key: key.as_str().to_owned(),
                message: error.to_string(),
            })?;

        Ok(())
    }

    pub async fn node_public_ip(
        &self,
        node_id: &NodeId,
    ) -> Result<Option<NodePublicIpObservation>, ObservationStoreError> {
        let key = NodePublicIpObservationKey::from_node_id(node_id);
        let Some(observation) =
            get_observation::<NodePublicIpObservation>(&self.bucket, key.as_str()).await?
        else {
            return Ok(None);
        };
        verify_observation_key(
            key.as_str(),
            NodePublicIpObservationKey::from_node_id(&observation.node_id).as_str(),
        )?;
        Ok(Some(observation))
    }

    pub async fn node_public_ips(
        &self,
    ) -> Result<Vec<NodePublicIpObservation>, ObservationStoreError> {
        list_current(
            &self.bucket,
            &format!("{NODE_PUBLIC_IP_OBSERVATION_PREFIX}."),
            |observation: &NodePublicIpObservation| {
                NodePublicIpObservationKey::from_node_id(&observation.node_id)
                    .as_str()
                    .to_owned()
            },
            |observation| observation.node_id.clone(),
        )
        .await
        .map_err(ObservationStoreError::from)
    }

    pub async fn replace_gateway_status(
        &self,
        observation: &GatewayStatusObservation,
    ) -> Result<(), ObservationStoreError> {
        let key = GatewayStatusObservationKey::from_node_id(&observation.node_id);
        put_observation(&self.bucket, key.as_str(), observation).await
    }

    pub async fn gateway_status(
        &self,
        node_id: &NodeId,
    ) -> Result<Option<GatewayStatusObservation>, ObservationStoreError> {
        let key = GatewayStatusObservationKey::from_node_id(node_id);
        let Some(observation) =
            get_observation::<GatewayStatusObservation>(&self.bucket, key.as_str()).await?
        else {
            return Ok(None);
        };
        verify_observation_key(
            key.as_str(),
            GatewayStatusObservationKey::from_node_id(&observation.node_id).as_str(),
        )?;
        Ok(Some(observation))
    }

    pub async fn gateway_statuses(
        &self,
    ) -> Result<Vec<GatewayStatusObservation>, ObservationStoreError> {
        list_current(
            &self.bucket,
            &format!("{GATEWAY_STATUS_OBSERVATION_PREFIX}."),
            |observation: &GatewayStatusObservation| {
                GatewayStatusObservationKey::from_node_id(&observation.node_id)
                    .as_str()
                    .to_owned()
            },
            |observation| observation.node_id.clone(),
        )
        .await
        .map_err(ObservationStoreError::from)
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
        let Some(payload) = with_io_timeout(
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
            &format!("{NODE_CONTAINER_OBSERVATION_PREFIX}."),
        )
        .await
        .map_err(|error| ObservationStoreError::ListKeys {
            message: error.message,
        })?;
        let mut records = Vec::new();

        for entry in entries {
            let snapshot: NodeContainerObservationSnapshot =
                serde_json::from_slice(&entry.value).map_err(ObservationStoreError::Decode)?;
            let actual_key = NodeContainerObservationKey::from_node_id(snapshot.node_id());
            if entry.key != actual_key.as_str() {
                return Err(ObservationStoreError::CorruptKey {
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

    pub async fn watch_node_container_snapshot_changes(
        &self,
    ) -> Result<jetstream::kv::Watch, ObservationStoreError> {
        with_io_timeout(
            "node observation snapshot watch",
            self.bucket
                .watch_with_history(format!("{NODE_CONTAINER_OBSERVATION_PREFIX}.>")),
        )
        .await?
        .map_err(|error| ObservationStoreError::Watch {
            message: error.to_string(),
        })
    }

    pub async fn watch_node_public_ip_changes(
        &self,
    ) -> Result<jetstream::kv::Watch, ObservationStoreError> {
        with_io_timeout(
            "node public ip watch",
            self.bucket
                .watch_with_history(format!("{NODE_PUBLIC_IP_OBSERVATION_PREFIX}.*.public_ip")),
        )
        .await?
        .map_err(|error| ObservationStoreError::Watch {
            message: error.to_string(),
        })
    }

    pub async fn watch_gateway_status_changes(
        &self,
    ) -> Result<jetstream::kv::Watch, ObservationStoreError> {
        with_io_timeout(
            "gateway status watch",
            self.bucket
                .watch_with_history(format!("{GATEWAY_STATUS_OBSERVATION_PREFIX}.*.status")),
        )
        .await?
        .map_err(|error| ObservationStoreError::Watch {
            message: error.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeContainerObservationRecord {
    pub snapshot: NodeContainerObservationSnapshot,
    pub observed_at_unix_nanos: i128,
    pub revision: u64,
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
    Watch {
        message: String,
    },
    Put {
        key: String,
        message: String,
    },
    Delete {
        key: String,
        message: String,
    },
    Get {
        key: String,
        message: String,
    },
    CorruptKey {
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
            Self::Watch { message } => write!(formatter, "watch node observation keys: {message}"),
            Self::Put { key, message } => write!(formatter, "put {key}: {message}"),
            Self::Delete { key, message } => write!(formatter, "delete {key}: {message}"),
            Self::Get { key, message } => write!(formatter, "get {key}: {message}"),
            Self::CorruptKey { key, actual_key } => write!(
                formatter,
                "observation key {} does not match observation key {}",
                key, actual_key
            ),
            Self::Timeout { operation } => write!(formatter, "{operation} timed out"),
        }
    }
}

impl From<NatsIoTimeout> for ObservationStoreError {
    fn from(timeout: NatsIoTimeout) -> Self {
        Self::Timeout {
            operation: timeout.operation,
        }
    }
}

impl From<KvListError> for ObservationStoreError {
    fn from(error: KvListError) -> Self {
        match error {
            KvListError::Scan { message } => Self::ListKeys { message },
            KvListError::Decode(error) => Self::Decode(error),
            KvListError::CorruptKey { key, actual_key } => Self::CorruptKey { key, actual_key },
        }
    }
}

async fn put_observation<T: serde::Serialize>(
    bucket: &jetstream::kv::Store,
    key: &str,
    observation: &T,
) -> Result<(), ObservationStoreError> {
    let payload = serde_json::to_vec(observation).map_err(ObservationStoreError::Encode)?;
    with_io_timeout("observation put", bucket.put(key, payload.into()))
        .await?
        .map_err(|error| ObservationStoreError::Put {
            key: key.to_owned(),
            message: error.to_string(),
        })?;

    Ok(())
}

async fn get_observation<T: serde::de::DeserializeOwned>(
    bucket: &jetstream::kv::Store,
    key: &str,
) -> Result<Option<T>, ObservationStoreError> {
    let Some(payload) = with_io_timeout("observation get", bucket.get(key))
        .await?
        .map_err(|error| ObservationStoreError::Get {
            key: key.to_owned(),
            message: error.to_string(),
        })?
    else {
        return Ok(None);
    };

    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(ObservationStoreError::Decode)
}

fn verify_observation_key(key: &str, actual_key: &str) -> Result<(), ObservationStoreError> {
    if key != actual_key {
        return Err(ObservationStoreError::CorruptKey {
            key: key.to_owned(),
            actual_key: actual_key.to_owned(),
        });
    }

    Ok(())
}
