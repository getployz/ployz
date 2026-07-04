//! NATS-backed latest observation adapters.

use crate::kv::{
    KvListError, NatsIoTimeout, bounded_bucket_key_scan_entries_with_prefix, list_current,
    with_io_timeout,
};
use async_nats::jetstream;
use ployz_core::ids::{ContainerId, MachineId};
use ployz_core::machine_runtime::{
    MachineContainerObservationSnapshot, ManagedContainerObservation,
};
use ployz_core::state::{
    GATEWAY_STATUS_OBSERVATION_PREFIX, GatewayStatusObservation, GatewayStatusObservationKey,
    MACHINE_CONTAINER_OBSERVATION_PREFIX, MACHINE_PUBLIC_IP_OBSERVATION_PREFIX,
    MachineContainerObservationKey, MachinePublicIpObservation, MachinePublicIpObservationKey,
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

    pub async fn replace_machine_containers(
        &self,
        snapshot: &MachineContainerObservationSnapshot,
    ) -> Result<(), ObservationStoreError> {
        let key = MachineContainerObservationKey::from_machine_id(snapshot.machine_id());
        let payload = serde_json::to_vec(snapshot).map_err(ObservationStoreError::Encode)?;
        with_io_timeout(
            "machine observation snapshot put",
            self.bucket.put(key.as_str(), payload.into()),
        )
        .await?
        .map_err(|error| ObservationStoreError::Put {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?;

        Ok(())
    }

    pub async fn replace_machine_public_ip(
        &self,
        observation: &MachinePublicIpObservation,
    ) -> Result<(), ObservationStoreError> {
        let key = MachinePublicIpObservationKey::from_machine_id(&observation.machine_id);
        put_observation(&self.bucket, key.as_str(), observation).await
    }

    pub async fn clear_machine_public_ip(
        &self,
        machine_id: &MachineId,
    ) -> Result<(), ObservationStoreError> {
        if self.machine_public_ip(machine_id).await?.is_none() {
            return Ok(());
        }

        let key = MachinePublicIpObservationKey::from_machine_id(machine_id);
        with_io_timeout("machine public ip delete", self.bucket.delete(key.as_str()))
            .await?
            .map_err(|error| ObservationStoreError::Delete {
                key: key.as_str().to_owned(),
                message: error.to_string(),
            })?;

        Ok(())
    }

    pub async fn machine_public_ip(
        &self,
        machine_id: &MachineId,
    ) -> Result<Option<MachinePublicIpObservation>, ObservationStoreError> {
        let key = MachinePublicIpObservationKey::from_machine_id(machine_id);
        let Some(observation) =
            get_observation::<MachinePublicIpObservation>(&self.bucket, key.as_str()).await?
        else {
            return Ok(None);
        };
        verify_observation_key(
            key.as_str(),
            MachinePublicIpObservationKey::from_machine_id(&observation.machine_id).as_str(),
        )?;
        Ok(Some(observation))
    }

    pub async fn machine_public_ips(
        &self,
    ) -> Result<Vec<MachinePublicIpObservation>, ObservationStoreError> {
        list_current(
            &self.bucket,
            &format!("{MACHINE_PUBLIC_IP_OBSERVATION_PREFIX}."),
            |observation: &MachinePublicIpObservation| {
                MachinePublicIpObservationKey::from_machine_id(&observation.machine_id)
                    .as_str()
                    .to_owned()
            },
            |observation| observation.machine_id.clone(),
        )
        .await
        .map_err(ObservationStoreError::from)
    }

    pub async fn replace_gateway_status(
        &self,
        observation: &GatewayStatusObservation,
    ) -> Result<(), ObservationStoreError> {
        let key = GatewayStatusObservationKey::from_machine_id(&observation.machine_id);
        put_observation(&self.bucket, key.as_str(), observation).await
    }

    pub async fn gateway_status(
        &self,
        machine_id: &MachineId,
    ) -> Result<Option<GatewayStatusObservation>, ObservationStoreError> {
        let key = GatewayStatusObservationKey::from_machine_id(machine_id);
        let Some(observation) =
            get_observation::<GatewayStatusObservation>(&self.bucket, key.as_str()).await?
        else {
            return Ok(None);
        };
        verify_observation_key(
            key.as_str(),
            GatewayStatusObservationKey::from_machine_id(&observation.machine_id).as_str(),
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
                GatewayStatusObservationKey::from_machine_id(&observation.machine_id)
                    .as_str()
                    .to_owned()
            },
            |observation| observation.machine_id.clone(),
        )
        .await
        .map_err(ObservationStoreError::from)
    }

    pub async fn container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<Option<ManagedContainerObservation>, ObservationStoreError> {
        let Some(snapshot) = self.machine_snapshot(machine_id).await? else {
            return Ok(None);
        };

        Ok(snapshot.container(container_id).cloned())
    }

    pub async fn machine_snapshot(
        &self,
        machine_id: &MachineId,
    ) -> Result<Option<MachineContainerObservationSnapshot>, ObservationStoreError> {
        let key = MachineContainerObservationKey::from_machine_id(machine_id);
        let Some(payload) = with_io_timeout(
            "machine observation snapshot get",
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

    pub async fn machine_snapshot_record(
        &self,
        machine_id: &MachineId,
    ) -> Result<Option<MachineContainerObservationRecord>, ObservationStoreError> {
        let key = MachineContainerObservationKey::from_machine_id(machine_id);
        let Some(entry) = with_io_timeout(
            "machine observation snapshot entry get",
            self.bucket.entry(key.as_str()),
        )
        .await?
        .map_err(|error| ObservationStoreError::Get {
            key: key.as_str().to_owned(),
            message: error.to_string(),
        })?
        else {
            return Ok(None);
        };
        if entry.operation != jetstream::kv::Operation::Put {
            return Ok(None);
        }

        let snapshot: MachineContainerObservationSnapshot =
            serde_json::from_slice(&entry.value).map_err(ObservationStoreError::Decode)?;
        Ok(Some(MachineContainerObservationRecord {
            snapshot,
            observed_at_unix_nanos: entry.created.unix_timestamp_nanos(),
            revision: entry.revision,
        }))
    }

    pub async fn machine_snapshots(
        &self,
    ) -> Result<Vec<MachineContainerObservationSnapshot>, ObservationStoreError> {
        Ok(self
            .machine_snapshot_records()
            .await?
            .into_iter()
            .map(|record| record.snapshot)
            .collect())
    }

    pub async fn machine_snapshot_records(
        &self,
    ) -> Result<Vec<MachineContainerObservationRecord>, ObservationStoreError> {
        let entries = bounded_bucket_key_scan_entries_with_prefix(
            &self.bucket,
            &format!("{MACHINE_CONTAINER_OBSERVATION_PREFIX}."),
        )
        .await
        .map_err(|error| ObservationStoreError::ListKeys {
            message: error.message,
        })?;
        let mut records = Vec::new();

        for entry in entries {
            let snapshot: MachineContainerObservationSnapshot =
                serde_json::from_slice(&entry.value).map_err(ObservationStoreError::Decode)?;
            let actual_key = MachineContainerObservationKey::from_machine_id(snapshot.machine_id());
            if entry.key != actual_key.as_str() {
                return Err(ObservationStoreError::CorruptKey {
                    key: entry.key,
                    actual_key: actual_key.as_str().to_owned(),
                });
            }
            records.push(MachineContainerObservationRecord {
                snapshot,
                observed_at_unix_nanos: entry.created.unix_timestamp_nanos(),
                revision: entry.revision,
            });
        }

        records.sort_by(|left, right| {
            left.snapshot
                .machine_id()
                .cmp(right.snapshot.machine_id())
                .then_with(|| left.revision.cmp(&right.revision))
        });
        Ok(records)
    }

    pub async fn watch_machine_container_snapshot_changes(
        &self,
    ) -> Result<jetstream::kv::Watch, ObservationStoreError> {
        with_io_timeout(
            "machine observation snapshot watch",
            self.bucket
                .watch_with_history(format!("{MACHINE_CONTAINER_OBSERVATION_PREFIX}.>")),
        )
        .await?
        .map_err(|error| ObservationStoreError::Watch {
            message: error.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineContainerObservationRecord {
    pub snapshot: MachineContainerObservationSnapshot,
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
                write!(formatter, "list machine observation keys: {message}")
            }
            Self::Watch { message } => {
                write!(formatter, "watch machine observation keys: {message}")
            }
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
