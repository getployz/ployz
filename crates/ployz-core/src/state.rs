//! Current-state records stored in JetStream KV.

use serde::{Deserialize, Serialize};

use crate::ids::{RevisionId, ServiceId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveServiceState {
    pub service_id: ServiceId,
    pub active_revision: RevisionId,
}
