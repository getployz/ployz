//! NATS-backed namespace-lock adapter.

mod namespace_lock;

use crate::kv::{KV_CORE_BUCKET, NatsIoTimeout, with_io_timeout};
use async_nats::jetstream;
pub use namespace_lock::{
    NAMESPACE_LOCK_RENEW_INTERVAL_MS, NAMESPACE_LOCK_TTL_MS, NamespaceLockAcquire,
    NamespaceLockRenew,
};
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
    Put {
        key: String,
        message: String,
    },
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
    CorruptNamespaceLock {
        key: String,
        message: String,
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
            Self::Encode(error) => write!(formatter, "encode core state: {error}"),
            Self::Decode(error) => write!(formatter, "decode core state: {error}"),
            Self::Put { key, message } => write!(formatter, "put {key}: {message}"),
            Self::CasConflict { message } => write!(formatter, "cas conflict: {message}"),
            Self::Get { key, message } => write!(formatter, "get {key}: {message}"),
            Self::Delete { key, message } => write!(formatter, "delete {key}: {message}"),
            Self::CorruptNamespaceLock { key, message } => {
                write!(formatter, "namespace lock at {key} is corrupt: {message}")
            }
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
