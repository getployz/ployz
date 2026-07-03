//! JetStream bucket and stream bootstrap.

#[path = "bootstrap/assurance.rs"]
mod assurance;

use crate::kv::{KV_CORE_BUCKET, KvBucketSpec};
use crate::observations::KV_OBS_BUCKET;
use crate::operations::{KV_OPS_BUCKET, PLZ_OPS_STREAM};
use crate::schedules::{NatsServerVersion, NatsServerVersionParseError};
use crate::streams::{DiscardPolicy, RetentionPolicy, StorageBackend, StreamSpec};
pub use assurance::{BootstrapAssuranceError, assure_nats_resources};
use ployz_core::subjects::OPS_STREAM_SUBJECT;

pub const MIN_NATS_SERVER_VERSION: NatsServerVersion = NatsServerVersion {
    major: 2,
    minor: 12,
    patch: 0,
};

pub const RECOMMENDED_NATS_SERVER_VERSION: NatsServerVersion = NatsServerVersion {
    major: 2,
    minor: 14,
    patch: 2,
};

pub const SINGLE_CORE_REPLICAS: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceReplicas(usize);

impl ResourceReplicas {
    pub const SINGLE_CORE: Self = Self(SINGLE_CORE_REPLICAS);

    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub fn observed(value: usize) -> Result<Self, InvalidResourceReplicas> {
        match value {
            1 | 3 | 5 => Ok(Self(value)),
            _ => Err(InvalidResourceReplicas { value }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidResourceReplicas {
    pub value: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapPlan {
    pub kv_buckets: Vec<KvBucketSpec>,
    pub streams: Vec<StreamSpec>,
}

impl BootstrapPlan {
    pub fn for_single_server_client(client: &async_nats::Client) -> Result<Self, BootstrapRefusal> {
        let server = client.server_info();
        let version = NatsServerVersion::parse(&server.version).map_err(|source| {
            BootstrapRefusal::InvalidServerVersion {
                value: server.version,
                source,
            }
        })?;
        let capabilities = NatsServerCapabilities::new(version, server.jetstream);

        Self::for_single_core(capabilities)
    }

    pub fn for_single_core(capabilities: NatsServerCapabilities) -> Result<Self, BootstrapRefusal> {
        Self::validate_single_server(&capabilities)?;
        Ok(Self::manifest())
    }

    fn validate_single_server(
        capabilities: &NatsServerCapabilities,
    ) -> Result<(), BootstrapRefusal> {
        if !capabilities.jetstream_enabled {
            return Err(BootstrapRefusal::JetStreamDisabled);
        }

        if capabilities.version < MIN_NATS_SERVER_VERSION {
            return Err(BootstrapRefusal::UnsupportedServerVersion {
                minimum: MIN_NATS_SERVER_VERSION,
                actual: capabilities.version,
            });
        }

        Ok(())
    }

    fn manifest() -> Self {
        Self {
            kv_buckets: vec![
                KvBucketSpec::new(KV_CORE_BUCKET),
                KvBucketSpec::new(KV_OPS_BUCKET),
                KvBucketSpec::new(KV_OBS_BUCKET),
            ],
            streams: vec![StreamSpec::new(
                PLZ_OPS_STREAM,
                vec![OPS_STREAM_SUBJECT.to_owned()],
                RetentionPolicy::Limits,
                StorageBackend::File,
                DiscardPolicy::Old,
            )],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BootstrapRefusal {
    #[error("JetStream is disabled")]
    JetStreamDisabled,
    #[error("invalid NATS server version {value:?}: {source:?}")]
    InvalidServerVersion {
        value: String,
        source: NatsServerVersionParseError,
    },
    #[error(
        "NATS server version {}.{}.{} is below required {}.{}.{}",
        actual.major,
        actual.minor,
        actual.patch,
        minimum.major,
        minimum.minor,
        minimum.patch
    )]
    UnsupportedServerVersion {
        minimum: NatsServerVersion,
        actual: NatsServerVersion,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServerCapabilities {
    pub version: NatsServerVersion,
    pub jetstream_enabled: bool,
}

impl NatsServerCapabilities {
    #[must_use]
    pub const fn new(version: NatsServerVersion, jetstream_enabled: bool) -> Self {
        Self {
            version,
            jetstream_enabled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapResourceKind {
    KvBucket,
    Stream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapResourceRef {
    pub kind: BootstrapResourceKind,
    pub name: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapResourceRefusal {
    MissingResource,
    ConfigurationDrift {
        field: &'static str,
        expected: String,
        observed: String,
    },
}
