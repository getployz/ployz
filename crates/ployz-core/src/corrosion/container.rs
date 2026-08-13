//! Natural runtime identity and endpoint-projection keys for managed containers.

use serde::{Deserialize, Serialize};

use crate::corrosion::{CorrosionServiceName, ServiceEndpoint};
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

/// Natural identity for one endpoint inside a machine-owned testimony row.
#[must_use]
pub fn service_endpoint_key(endpoint: &ServiceEndpoint, machine: &MachineName) -> String {
    let slot = match endpoint.replica_slot {
        ReplicaSlot::Global => "global".to_owned(),
        ReplicaSlot::Replicated { number } => number.get().to_string(),
    };
    format!(
        "{}/{}/{}/{}/{slot}",
        endpoint.namespace_id.as_str(),
        endpoint.service_name.as_str(),
        endpoint.deploy.as_str(),
        machine.as_str(),
    )
}
