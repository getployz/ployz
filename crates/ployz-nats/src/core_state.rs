//! NATS-backed canonical current-state adapters.

mod active_machine;
mod active_route;
mod active_service;
mod nats_authorized_user;

use crate::kv::{KV_CORE_BUCKET, KvListError, NatsIoTimeout, with_io_timeout};
pub use active_machine::{ActiveMachineReadError, ActiveMachineWriteError};
pub use active_route::ActiveRouteStoreError;
use async_nats::jetstream;
use ployz_core::ids::ServiceId;
use std::fmt;

#[derive(Debug, Clone)]
pub struct AsyncNatsCoreStateStore {
    pub(crate) bucket: jetstream::kv::Store,
}

impl AsyncNatsCoreStateStore {
    pub async fn from_jetstream(
        jetstream: &jetstream::Context,
    ) -> Result<Self, CoreStateStoreError> {
        let bucket = with_io_timeout(
            "core state bucket open",
            jetstream.get_key_value(KV_CORE_BUCKET),
        )
        .await?
        .map_err(|error| CoreStateStoreError::OpenBucket {
            bucket: KV_CORE_BUCKET,
            message: error.to_string(),
        })?;

        Ok(Self { bucket })
    }
}

#[derive(Debug)]
pub enum CoreStateStoreError {
    OpenBucket {
        bucket: &'static str,
        message: String,
    },
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    CasConflict {
        message: String,
    },
    Get {
        key: String,
        message: String,
    },
    Delete {
        key: String,
        message: String,
    },
    ListKeys {
        message: String,
    },
    CorruptActiveServiceState {
        key: String,
        expected_service_id: ServiceId,
        actual_service_id: ServiceId,
    },
    CorruptKey {
        key: String,
        actual_key: String,
    },
    Timeout {
        operation: &'static str,
    },
}

impl fmt::Display for CoreStateStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenBucket { bucket, message } => {
                write!(formatter, "open bucket {bucket}: {message}")
            }
            Self::Encode(error) => write!(formatter, "encode active service state: {error}"),
            Self::Decode(error) => write!(formatter, "decode active service state: {error}"),
            Self::CasConflict { message } => write!(formatter, "cas conflict: {message}"),
            Self::Get { key, message } => write!(formatter, "get {key}: {message}"),
            Self::Delete { key, message } => write!(formatter, "delete {key}: {message}"),
            Self::ListKeys { message } => write!(formatter, "list active service keys: {message}"),
            Self::CorruptActiveServiceState {
                key,
                expected_service_id,
                actual_service_id,
            } => write!(
                formatter,
                "active service state at {} belongs to {}, not {}",
                key,
                actual_service_id.as_str(),
                expected_service_id.as_str()
            ),
            Self::CorruptKey { key, actual_key } => write!(
                formatter,
                "core state key {key} does not match canonical key {actual_key}"
            ),
            Self::Timeout { operation } => write!(formatter, "{operation} timed out"),
        }
    }
}

impl From<NatsIoTimeout> for CoreStateStoreError {
    fn from(timeout: NatsIoTimeout) -> Self {
        Self::Timeout {
            operation: timeout.operation,
        }
    }
}

impl From<KvListError> for CoreStateStoreError {
    fn from(error: KvListError) -> Self {
        match error {
            KvListError::Scan { message } => Self::ListKeys { message },
            KvListError::Decode(error) => Self::Decode(error),
            KvListError::CorruptKey { key, actual_key } => Self::CorruptKey { key, actual_key },
        }
    }
}
