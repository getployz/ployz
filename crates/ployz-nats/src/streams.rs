//! JetStream stream and durable consumer helpers.

use crate::replication::ReplicationFactor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSpec {
    pub name: &'static str,
    pub subjects: Vec<String>,
    pub retention: RetentionPolicy,
    pub storage: StorageBackend,
    pub replicas: ReplicationFactor,
    pub discard: DiscardPolicy,
    pub allow_message_schedules: bool,
}

impl StreamSpec {
    #[must_use]
    pub fn new(
        name: &'static str,
        subjects: Vec<String>,
        retention: RetentionPolicy,
        storage: StorageBackend,
        replicas: ReplicationFactor,
        discard: DiscardPolicy,
    ) -> Self {
        Self {
            name,
            subjects,
            retention,
            storage,
            replicas,
            discard,
            allow_message_schedules: false,
        }
    }

    #[must_use]
    pub const fn with_message_schedules(mut self, allow_message_schedules: bool) -> Self {
        self.allow_message_schedules = allow_message_schedules;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionPolicy {
    Limits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardPolicy {
    Old,
    New,
}
