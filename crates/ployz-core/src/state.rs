//! Current-state records stored in JetStream KV.

use serde::{Deserialize, Serialize};

use crate::ids::{ContainerId, NodeId, RevisionId, ServiceId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveServiceState {
    pub service_id: ServiceId,
    pub active_revision: RevisionId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContainerObservationKey(String);

impl ContainerObservationKey {
    #[must_use]
    pub fn from_ids(node_id: &NodeId, container_id: &ContainerId) -> Self {
        Self(format!(
            "containers.{}.{}",
            node_id.as_str(),
            container_id.as_str()
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
