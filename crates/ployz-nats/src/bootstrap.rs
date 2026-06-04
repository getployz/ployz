//! JetStream bucket, stream, and Object Store bootstrap.

use crate::kv::KvBucketSpec;
use crate::objects::ObjectBucketSpec;
use crate::schedules::{NatsServerVersion, ScheduleCapability};
use crate::streams::{DiscardPolicy, RetentionPolicy, StorageBackend, StreamSpec};

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
    #[must_use]
    pub fn single_node(server_version: NatsServerVersion) -> Self {
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
                    vec!["plz.op.>".to_owned(), "plz.job.>".to_owned()],
                    RetentionPolicy::Limits,
                    StorageBackend::File,
                    ReplicationFactor::One,
                    DiscardPolicy::Old,
                ),
                StreamSpec::new(
                    "PLZ_AUDIT",
                    vec!["plz.audit.>".to_owned()],
                    RetentionPolicy::Limits,
                    StorageBackend::File,
                    ReplicationFactor::One,
                    DiscardPolicy::Old,
                ),
                StreamSpec::new(
                    "PLZ_OBS_TRANSITIONS",
                    vec!["plz.obs.transition.>".to_owned()],
                    RetentionPolicy::Limits,
                    StorageBackend::File,
                    ReplicationFactor::One,
                    DiscardPolicy::Old,
                ),
                StreamSpec::new(
                    "PLZ_SCHEDULES",
                    vec!["plz.schedule.>".to_owned()],
                    RetentionPolicy::Limits,
                    StorageBackend::File,
                    ReplicationFactor::One,
                    DiscardPolicy::New,
                )
                .with_message_schedules(schedule_capability.message_schedules_available),
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
    pub fn bucket_named(&self, name: &str) -> bool {
        self.kv_buckets.iter().any(|bucket| bucket.name == name)
    }

    #[must_use]
    pub fn stream_named(&self, name: &str) -> bool {
        self.streams.iter().any(|stream| stream.name == name)
    }

    #[must_use]
    pub fn object_bucket_named(&self, name: &str) -> bool {
        self.object_buckets.iter().any(|bucket| bucket.name == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicationFactor {
    One,
    Three,
}

impl ReplicationFactor {
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::One => 1,
            Self::Three => 3,
        }
    }
}
