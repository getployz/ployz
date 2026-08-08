//! Recovery identity for Corrosion-owned v2 containers.

use serde::{Deserialize, Serialize};

use crate::deploy::ReplicaSlot;
use crate::ids::{NamespaceRowId, OperationRowId, ServiceRowId};

/// Exact identity supplied to v2 container adapters and recovered from Docker
/// labels before an existing container is changed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct V2ManagedContainerIdentity {
    pub namespace_id: NamespaceRowId,
    pub service_id: ServiceRowId,
    pub operation_id: OperationRowId,
    pub replica_slot: ReplicaSlot,
}
