//! Current-state records stored in JetStream KV.

use serde::{Deserialize, Serialize};

use crate::ids::{NodeId, RevisionId, ServiceId};

pub const ACTIVE_SERVICE_STATE_PREFIX: &str = "services";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveServiceState {
    pub service_id: ServiceId,
    pub active_revision: RevisionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveServiceStateKey(String);

impl ActiveServiceStateKey {
    #[must_use]
    pub fn from_service_id(service_id: &ServiceId) -> Self {
        Self(format!(
            "{ACTIVE_SERVICE_STATE_PREFIX}.{}",
            service_id.as_str()
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreStateRevision(u64);

impl CoreStateRevision {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveServiceCommitRequest {
    pub service_id: ServiceId,
    pub expected_current: ExpectedActiveService,
    pub target_revision: RevisionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ExpectedActiveService {
    Absent,
    Revision(RevisionId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveServiceCommit {
    Stored {
        revision: CoreStateRevision,
    },
    AlreadyCommitted {
        current_revision: RevisionId,
    },
    ActiveServiceChanged {
        expected_current: ExpectedActiveService,
        current_revision: Option<RevisionId>,
        attempted_revision: RevisionId,
    },
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
