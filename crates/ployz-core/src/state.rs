//! Current-state records stored in JetStream KV.

use serde::{Deserialize, Serialize};

use crate::ids::{NodeId, RevisionId, ServiceId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveServiceState {
    pub service_id: ServiceId,
    pub active_revision: RevisionId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeContainerObservationKey(String);

impl NodeContainerObservationKey {
    #[must_use]
    pub fn from_node_id(node_id: &NodeId) -> Self {
        Self(format!("containers.{}", node_id.as_str()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
