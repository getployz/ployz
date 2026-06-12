//! JetStream stream specs.

use crate::bootstrap::ResourceReplicas;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSpec {
    pub name: &'static str,
    pub subjects: Vec<String>,
    pub retention: RetentionPolicy,
    pub storage: StorageBackend,
    pub(crate) replicas: ResourceReplicas,
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
        discard: DiscardPolicy,
    ) -> Self {
        Self {
            name,
            subjects,
            retention,
            storage,
            replicas: ResourceReplicas::SINGLE_CORE,
            discard,
            allow_message_schedules: false,
        }
    }

    #[must_use]
    pub const fn replicas(&self) -> ResourceReplicas {
        self.replicas
    }

    #[must_use]
    pub(crate) const fn with_observed_replicas(mut self, replicas: ResourceReplicas) -> Self {
        self.replicas = replicas;
        self
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageId(String);

impl MessageId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
