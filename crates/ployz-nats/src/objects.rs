//! JetStream Object Store helpers.

use crate::replication::ReplicationFactor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectBucketSpec {
    pub name: &'static str,
    pub replicas: ReplicationFactor,
}

impl ObjectBucketSpec {
    #[must_use]
    pub const fn new(name: &'static str, replicas: ReplicationFactor) -> Self {
        Self { name, replicas }
    }
}
