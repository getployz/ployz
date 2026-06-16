//! JetStream Object Store resource specs.

use crate::bootstrap::ResourceReplicas;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectBucketSpec {
    pub name: &'static str,
    pub(crate) replicas: ResourceReplicas,
}

impl ObjectBucketSpec {
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            replicas: ResourceReplicas::SINGLE_CORE,
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
}
