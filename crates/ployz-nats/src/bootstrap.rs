//! JetStream bucket, stream, and Object Store bootstrap.

#[path = "bootstrap/assurance.rs"]
mod assurance;
#[path = "bootstrap/resources.rs"]
mod resources;

use crate::kv::{KV_CORE_BUCKET, KvBucketSpec};
use crate::objects::ObjectBucketSpec;
use crate::replication::ReplicationFactor;
use crate::schedules::{NatsServerVersion, NatsServerVersionParseError, ScheduleCapability};
use crate::streams::{DiscardPolicy, RetentionPolicy, StorageBackend, StreamSpec};
pub use assurance::{BootstrapAssuranceError, assure_nats_resources};
use ployz_core::ha::CoreTopology;
use ployz_core::ids::NodeId;
use ployz_core::subjects::{
    AUDIT_STREAM_SUBJECT, JOBS_STREAM_SUBJECT, OBS_TRANSITION_STREAM_SUBJECT, OPS_STREAM_SUBJECT,
    SCHEDULE_STREAM_SUBJECT,
};
pub use resources::{
    BootstrapResourceAction, BootstrapResourceDecision, BootstrapResourceKind,
    BootstrapResourceRef, BootstrapResourceRefusal,
};
use resources::{ExistingResourceView, PlannedResource};
use std::fmt;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapPlan {
    pub kv_buckets: Vec<KvBucketSpec>,
    pub streams: Vec<StreamSpec>,
    pub object_buckets: Vec<ObjectBucketSpec>,
    pub schedule_capability: ScheduleCapability,
}

impl BootstrapPlan {
    pub fn for_single_server_client_and_topology(
        client: &async_nats::Client,
        _topology: &CoreTopology,
        node_id: NodeId,
    ) -> Result<Self, BootstrapRefusal> {
        let server = client.server_info();
        let version = NatsServerVersion::parse(&server.version).map_err(|source| {
            BootstrapRefusal::InvalidServerVersion {
                value: server.version,
                source,
            }
        })?;
        let capabilities = NatsServerCapabilities::new(version, server.jetstream, node_id);

        Self::for_single_core(capabilities)
    }

    pub fn for_core_topology(
        capabilities: NatsServerCapabilities,
        topology: &CoreTopology,
    ) -> Result<Self, BootstrapRefusal> {
        let _single_core = topology.node();

        Self::for_single_core(capabilities)
    }

    pub fn for_single_core(capabilities: NatsServerCapabilities) -> Result<Self, BootstrapRefusal> {
        Self::validate_single_server(&capabilities)?;
        Ok(Self::manifest(capabilities.version))
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

    fn manifest(server_version: NatsServerVersion) -> Self {
        let schedule_capability = ScheduleCapability::from_server_version(server_version);
        let replicas = ReplicationFactor::One;

        Self {
            kv_buckets: vec![
                KvBucketSpec::new(KV_CORE_BUCKET, replicas),
                KvBucketSpec::new("KV_OPS", replicas),
                KvBucketSpec::new("KV_OBS", replicas),
                KvBucketSpec::new("KV_LOCKS", replicas),
            ],
            streams: vec![
                StreamSpec::new(
                    "PLZ_OPS",
                    vec![OPS_STREAM_SUBJECT.to_owned()],
                    RetentionPolicy::Limits,
                    StorageBackend::File,
                    replicas,
                    DiscardPolicy::Old,
                ),
                StreamSpec::new(
                    "PLZ_JOBS",
                    vec![JOBS_STREAM_SUBJECT.to_owned()],
                    RetentionPolicy::Limits,
                    StorageBackend::File,
                    replicas,
                    DiscardPolicy::Old,
                ),
                StreamSpec::new(
                    "PLZ_AUDIT",
                    vec![AUDIT_STREAM_SUBJECT.to_owned()],
                    RetentionPolicy::Limits,
                    StorageBackend::File,
                    replicas,
                    DiscardPolicy::Old,
                ),
                StreamSpec::new(
                    "PLZ_OBS_TRANSITIONS",
                    vec![OBS_TRANSITION_STREAM_SUBJECT.to_owned()],
                    RetentionPolicy::Limits,
                    StorageBackend::File,
                    replicas,
                    DiscardPolicy::Old,
                ),
                StreamSpec::new(
                    "PLZ_SCHEDULES",
                    vec![SCHEDULE_STREAM_SUBJECT.to_owned()],
                    RetentionPolicy::Limits,
                    StorageBackend::File,
                    replicas,
                    DiscardPolicy::Old,
                )
                .with_message_schedules(schedule_capability.message_schedules_available()),
            ],
            object_buckets: vec![
                ObjectBucketSpec::new("PLZ_BUNDLES", replicas),
                ObjectBucketSpec::new("PLZ_DIAGNOSTICS", replicas),
                ObjectBucketSpec::new("PLZ_CERTS", replicas),
                ObjectBucketSpec::new("PLZ_BACKUPS", replicas),
            ],
            schedule_capability,
        }
    }

    #[must_use]
    pub fn diff_against(&self, existing: &ExistingResources) -> BootstrapDiff {
        BootstrapDiff::from_resources(
            self.planned_resources()
                .into_iter()
                .map(|resource| resource.bootstrap_decision(&existing.view()))
                .collect(),
        )
    }

    fn planned_resources(&self) -> Vec<PlannedResource<'_>> {
        let mut resources = Vec::new();
        resources.extend(self.kv_buckets.iter().map(PlannedResource::KvBucket));
        resources.extend(self.streams.iter().map(PlannedResource::Stream));
        resources.extend(
            self.object_buckets
                .iter()
                .map(PlannedResource::ObjectBucket),
        );
        resources
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServerCapabilities {
    pub version: NatsServerVersion,
    pub jetstream_enabled: bool,
    pub configured_node_id: NodeId,
}

impl NatsServerCapabilities {
    #[must_use]
    pub const fn new(version: NatsServerVersion, jetstream_enabled: bool, node_id: NodeId) -> Self {
        Self {
            version,
            jetstream_enabled,
            configured_node_id: node_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapRefusal {
    JetStreamDisabled,
    InvalidServerVersion {
        value: String,
        source: NatsServerVersionParseError,
    },
    UnsupportedServerVersion {
        minimum: NatsServerVersion,
        actual: NatsServerVersion,
    },
}

impl fmt::Display for BootstrapRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::JetStreamDisabled => formatter.write_str("JetStream is disabled"),
            Self::InvalidServerVersion { value, source } => {
                write!(
                    formatter,
                    "invalid NATS server version {value:?}: {source:?}"
                )
            }
            Self::UnsupportedServerVersion { minimum, actual } => write!(
                formatter,
                "NATS server version {}.{}.{} is below required {}.{}.{}",
                actual.major,
                actual.minor,
                actual.patch,
                minimum.major,
                minimum.minor,
                minimum.patch
            ),
        }
    }
}

impl std::error::Error for BootstrapRefusal {}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExistingResources {
    pub kv_buckets: Vec<KvBucketSpec>,
    pub streams: Vec<StreamSpec>,
    pub object_buckets: Vec<ObjectBucketSpec>,
}

impl ExistingResources {
    fn view(&self) -> ExistingResourceView<'_> {
        ExistingResourceView {
            kv_buckets: &self.kv_buckets,
            streams: &self.streams,
            object_buckets: &self.object_buckets,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapDiff {
    pub kv_buckets: Vec<BootstrapResourceDecision>,
    pub streams: Vec<BootstrapResourceDecision>,
    pub object_buckets: Vec<BootstrapResourceDecision>,
}

impl BootstrapDiff {
    fn from_resources(resources: Vec<BootstrapResourceDecision>) -> Self {
        let mut diff = Self {
            kv_buckets: Vec::new(),
            streams: Vec::new(),
            object_buckets: Vec::new(),
        };

        for resource in resources {
            match resource.resource.kind {
                BootstrapResourceKind::KvBucket => diff.kv_buckets.push(resource),
                BootstrapResourceKind::Stream => diff.streams.push(resource),
                BootstrapResourceKind::ObjectBucket => diff.object_buckets.push(resource),
            }
        }

        diff
    }

    #[must_use]
    pub fn kv_bucket(&self, name: &str) -> Option<&BootstrapResourceDecision> {
        self.kv_buckets
            .iter()
            .find(|decision| decision.resource.name == name)
    }

    #[must_use]
    pub fn stream(&self, name: &str) -> Option<&BootstrapResourceDecision> {
        self.streams
            .iter()
            .find(|decision| decision.resource.name == name)
    }

    #[must_use]
    pub fn object_bucket(&self, name: &str) -> Option<&BootstrapResourceDecision> {
        self.object_buckets
            .iter()
            .find(|decision| decision.resource.name == name)
    }

    pub fn all_resources(&self) -> impl Iterator<Item = &BootstrapResourceDecision> {
        self.kv_buckets
            .iter()
            .chain(self.streams.iter())
            .chain(self.object_buckets.iter())
    }
}
