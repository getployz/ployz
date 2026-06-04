//! JetStream KV helpers.

use crate::replication::ReplicationFactor;

pub const KV_CORE_BUCKET: &str = "KV_CORE";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvBucketSpec {
    pub name: &'static str,
    pub replicas: ReplicationFactor,
}

impl KvBucketSpec {
    #[must_use]
    pub const fn new(name: &'static str, replicas: ReplicationFactor) -> Self {
        Self { name, replicas }
    }
}
