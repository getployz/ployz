//! JetStream bucket, stream, and Object Store bootstrap.

use crate::kv::KvBucketSpec;
use crate::objects::ObjectBucketSpec;
use crate::replication::ReplicationFactor;
use crate::schedules::{NatsServerVersion, ScheduleCapability};
use crate::streams::{DiscardPolicy, RetentionPolicy, StorageBackend, StreamSpec};
use ployz_core::subjects::{
    AUDIT_STREAM_SUBJECT, JOBS_STREAM_SUBJECT, OBS_TRANSITION_STREAM_SUBJECT, OPS_STREAM_SUBJECT,
    SCHEDULE_STREAM_SUBJECT,
};

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
    pub fn single_node(capabilities: NatsServerCapabilities) -> Result<Self, BootstrapRefusal> {
        if !capabilities.jetstream_enabled {
            return Err(BootstrapRefusal::JetStreamDisabled);
        }

        if capabilities.version < MIN_NATS_SERVER_VERSION {
            return Err(BootstrapRefusal::UnsupportedServerVersion {
                minimum: MIN_NATS_SERVER_VERSION,
                actual: capabilities.version,
            });
        }

        Ok(Self::single_node_manifest(capabilities.version))
    }

    fn single_node_manifest(server_version: NatsServerVersion) -> Self {
        let schedule_capability = ScheduleCapability::from_server_version(server_version);

        Self {
            kv_buckets: vec![
                KvBucketSpec::new("KV_CORE", ReplicationFactor::One),
                KvBucketSpec::new("KV_OPS", ReplicationFactor::One),
                KvBucketSpec::new("KV_OBS", ReplicationFactor::One),
                KvBucketSpec::new("KV_LOCKS", ReplicationFactor::One),
            ],
            streams: vec![
                StreamSpec::new(
                    "PLZ_OPS",
                    vec![OPS_STREAM_SUBJECT.to_owned()],
                    RetentionPolicy::Limits,
                    StorageBackend::File,
                    ReplicationFactor::One,
                    DiscardPolicy::Old,
                ),
                StreamSpec::new(
                    "PLZ_JOBS",
                    vec![JOBS_STREAM_SUBJECT.to_owned()],
                    RetentionPolicy::Limits,
                    StorageBackend::File,
                    ReplicationFactor::One,
                    DiscardPolicy::Old,
                ),
                StreamSpec::new(
                    "PLZ_AUDIT",
                    vec![AUDIT_STREAM_SUBJECT.to_owned()],
                    RetentionPolicy::Limits,
                    StorageBackend::File,
                    ReplicationFactor::One,
                    DiscardPolicy::Old,
                ),
                StreamSpec::new(
                    "PLZ_OBS_TRANSITIONS",
                    vec![OBS_TRANSITION_STREAM_SUBJECT.to_owned()],
                    RetentionPolicy::Limits,
                    StorageBackend::File,
                    ReplicationFactor::One,
                    DiscardPolicy::Old,
                ),
                StreamSpec::new(
                    "PLZ_SCHEDULES",
                    vec![SCHEDULE_STREAM_SUBJECT.to_owned()],
                    RetentionPolicy::Limits,
                    StorageBackend::File,
                    ReplicationFactor::One,
                    DiscardPolicy::New,
                )
                .with_message_schedules(schedule_capability.message_schedules_available()),
            ],
            object_buckets: vec![
                ObjectBucketSpec::new("PLZ_BUNDLES", ReplicationFactor::One),
                ObjectBucketSpec::new("PLZ_DIAGNOSTICS", ReplicationFactor::One),
                ObjectBucketSpec::new("PLZ_CERTS", ReplicationFactor::One),
                ObjectBucketSpec::new("PLZ_BACKUPS", ReplicationFactor::One),
            ],
            schedule_capability,
        }
    }

    #[must_use]
    pub fn diff_against(&self, existing: &ExistingResources) -> BootstrapDiff {
        let kv_buckets = self
            .kv_buckets
            .iter()
            .map(|bucket| {
                BootstrapResourceDecision::from_kv(
                    bucket,
                    existing
                        .kv_buckets
                        .iter()
                        .find(|observed| observed.name == bucket.name),
                )
            })
            .collect();
        let streams = self
            .streams
            .iter()
            .map(|stream| {
                BootstrapResourceDecision::from_stream(
                    stream,
                    existing
                        .streams
                        .iter()
                        .find(|observed| observed.name == stream.name),
                )
            })
            .collect();
        let object_buckets = self
            .object_buckets
            .iter()
            .map(|bucket| {
                BootstrapResourceDecision::from_object_bucket(
                    bucket,
                    existing
                        .object_buckets
                        .iter()
                        .find(|observed| observed.name == bucket.name),
                )
            })
            .collect();

        BootstrapDiff {
            kv_buckets,
            streams,
            object_buckets,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
pub enum BootstrapRefusal {
    JetStreamDisabled,
    UnsupportedServerVersion {
        minimum: NatsServerVersion,
        actual: NatsServerVersion,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExistingResources {
    pub kv_buckets: Vec<KvBucketSpec>,
    pub streams: Vec<StreamSpec>,
    pub object_buckets: Vec<ObjectBucketSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapDiff {
    pub kv_buckets: Vec<BootstrapResourceDecision>,
    pub streams: Vec<BootstrapResourceDecision>,
    pub object_buckets: Vec<BootstrapResourceDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapResourceDecision {
    pub name: &'static str,
    pub action: BootstrapAction,
}

impl BootstrapResourceDecision {
    fn from_kv(expected: &KvBucketSpec, observed: Option<&KvBucketSpec>) -> Self {
        let action = observed.map_or(BootstrapAction::Create, |observed| {
            kv_bucket_action(expected, observed)
        });

        Self::from_action(expected.name, action)
    }

    fn from_stream(expected: &StreamSpec, observed: Option<&StreamSpec>) -> Self {
        let action = observed.map_or(BootstrapAction::Create, |observed| {
            stream_action(expected, observed)
        });

        Self::from_action(expected.name, action)
    }

    fn from_object_bucket(
        expected: &ObjectBucketSpec,
        observed: Option<&ObjectBucketSpec>,
    ) -> Self {
        let action = observed.map_or(BootstrapAction::Create, |observed| {
            object_bucket_action(expected, observed)
        });

        Self::from_action(expected.name, action)
    }

    const fn from_action(name: &'static str, action: BootstrapAction) -> Self {
        Self { name, action }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapAction {
    Create,
    Adopt,
    Refuse { reason: BootstrapResourceRefusal },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapResourceRefusal {
    ConfigurationDrift {
        field: &'static str,
        expected: String,
        observed: String,
    },
}

fn kv_bucket_action(expected: &KvBucketSpec, observed: &KvBucketSpec) -> BootstrapAction {
    let KvBucketSpec {
        name: expected_name,
        replicas: expected_replicas,
    } = expected;
    let KvBucketSpec {
        name: observed_name,
        replicas: observed_replicas,
    } = observed;

    if expected_name != observed_name {
        return drift("name", expected_name, observed_name);
    }

    if expected_replicas != observed_replicas {
        return drift(
            "replicas",
            &expected_replicas.as_u8().to_string(),
            &observed_replicas.as_u8().to_string(),
        );
    }

    BootstrapAction::Adopt
}

fn object_bucket_action(
    expected: &ObjectBucketSpec,
    observed: &ObjectBucketSpec,
) -> BootstrapAction {
    let ObjectBucketSpec {
        name: expected_name,
        replicas: expected_replicas,
    } = expected;
    let ObjectBucketSpec {
        name: observed_name,
        replicas: observed_replicas,
    } = observed;

    if expected_name != observed_name {
        return drift("name", expected_name, observed_name);
    }

    if expected_replicas != observed_replicas {
        return drift(
            "replicas",
            &expected_replicas.as_u8().to_string(),
            &observed_replicas.as_u8().to_string(),
        );
    }

    BootstrapAction::Adopt
}

fn stream_action(expected: &StreamSpec, observed: &StreamSpec) -> BootstrapAction {
    let StreamSpec {
        name: expected_name,
        subjects: expected_subjects,
        retention: expected_retention,
        storage: expected_storage,
        replicas: expected_replicas,
        discard: expected_discard,
        allow_message_schedules: expected_allow_message_schedules,
    } = expected;
    let StreamSpec {
        name: observed_name,
        subjects: observed_subjects,
        retention: observed_retention,
        storage: observed_storage,
        replicas: observed_replicas,
        discard: observed_discard,
        allow_message_schedules: observed_allow_message_schedules,
    } = observed;

    if expected_name != observed_name {
        return drift("name", expected_name, observed_name);
    }

    if expected_subjects != observed_subjects {
        return drift(
            "subjects",
            &format!("{expected_subjects:?}"),
            &format!("{observed_subjects:?}"),
        );
    }

    if expected_retention != observed_retention {
        return drift(
            "retention",
            &format!("{expected_retention:?}"),
            &format!("{observed_retention:?}"),
        );
    }

    if expected_storage != observed_storage {
        return drift(
            "storage",
            &format!("{expected_storage:?}"),
            &format!("{observed_storage:?}"),
        );
    }

    if expected_replicas != observed_replicas {
        return drift(
            "replicas",
            &expected_replicas.as_u8().to_string(),
            &observed_replicas.as_u8().to_string(),
        );
    }

    if expected_discard != observed_discard {
        return drift(
            "discard",
            &format!("{expected_discard:?}"),
            &format!("{observed_discard:?}"),
        );
    }

    if expected_allow_message_schedules != observed_allow_message_schedules {
        return drift(
            "allow_message_schedules",
            &expected_allow_message_schedules.to_string(),
            &observed_allow_message_schedules.to_string(),
        );
    }

    BootstrapAction::Adopt
}

fn drift(field: &'static str, expected: &str, observed: &str) -> BootstrapAction {
    BootstrapAction::Refuse {
        reason: BootstrapResourceRefusal::ConfigurationDrift {
            field,
            expected: expected.to_owned(),
            observed: observed.to_owned(),
        },
    }
}
