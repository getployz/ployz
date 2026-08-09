//! Recovery identity for Corrosion-owned v2 containers.

use serde::{Deserialize, Serialize};

use crate::corrosion::CorrosionServiceName;
use crate::deploy::ReplicaSlot;
use crate::ids::{CorrosionNamespaceName, DeployName};
use crate::machine::MachineName;

/// Exact identity supplied to v2 container adapters and recovered from Docker
/// labels before an existing container is changed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct V2ManagedContainerIdentity {
    pub namespace_id: CorrosionNamespaceName,
    pub service_name: CorrosionServiceName,
    pub operation_id: DeployName,
    pub replica_slot: ReplicaSlot,
}

/// Stable Corrosion key for one managed replica. Docker's random id remains
/// runtime evidence in the document, never the distributed row identity.
#[must_use]
pub fn managed_container_key(
    identity: &V2ManagedContainerIdentity,
    machine: &MachineName,
) -> String {
    let slot = match identity.replica_slot {
        ReplicaSlot::Global => "global".to_owned(),
        ReplicaSlot::Replicated { number } => number.get().to_string(),
    };
    format!(
        "{}/{}/{}/{}/{slot}",
        identity.namespace_id.as_str(),
        identity.service_name.as_str(),
        identity.operation_id.as_str(),
        machine.as_str(),
    )
}
