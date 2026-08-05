//! Recovery identity for Corrosion-owned v2 containers.

use serde::{Deserialize, Serialize};

use crate::ids::{NamespaceRowId, OperationRowId, ServiceRowId};

/// Version-specific identity supplied to v2 container adapters.
///
/// This row-id-only shape is deliberately separate from the incumbent
/// revision-and-step identity used by the frozen control plane.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct V2ManagedContainerIdentity {
    pub namespace_id: NamespaceRowId,
    pub service_id: ServiceRowId,
    pub operation_id: OperationRowId,
}
