use serde::{Deserialize, Serialize};

use crate::corrosion::{
    CorrosionNamespaceName, CorrosionServiceName, IngressMode, RouteBindingDocument,
};
use crate::ids::{NamespaceRowId, OperationId, RouteBindingRowId, ServiceRowId};
use crate::ingress::IngressConfiguration;
use crate::operation::{EventSequence, RouteHostname, RoutePort};

use super::ops::{AcceptedOperation, OperationApiResponse};
use super::removal::RouteRemoveRequest;

/// Mesh-authenticated request to attach one hostname to one named service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct RouteAttachRequest {
    pub hostname: RouteHostname,
    pub namespace_name: CorrosionNamespaceName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace_id: Option<NamespaceRowId>,
    pub service_name: CorrosionServiceName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_id: Option<ServiceRowId>,
    pub endpoint_port: RoutePort,
    pub ingress_mode: IngressMode,
}

/// Whether attach created an identity or found that exact intent already attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum RouteAttachOutcome {
    Attached,
    AlreadyAttached,
}

/// The synchronous result of attaching one route-binding row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct RouteAttachReply {
    pub route_id: RouteBindingRowId,
    pub outcome: RouteAttachOutcome,
}

/// A route attach refusal. Each identity conflict retains enough evidence for
/// the operator to select an exact existing row or remove the occupying route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteAttachRefusal {
    NamespaceNotFound {
        namespace_name: CorrosionNamespaceName,
    },
    NamespaceAmbiguous {
        namespace_name: CorrosionNamespaceName,
        namespace_ids: Vec<NamespaceRowId>,
    },
    NamespaceIdMismatch {
        namespace_name: CorrosionNamespaceName,
        requested: NamespaceRowId,
        found: NamespaceRowId,
    },
    NamespaceIdentityMismatch {
        namespace_id: NamespaceRowId,
        requested_name: CorrosionNamespaceName,
        found_name: CorrosionNamespaceName,
    },
    NamespaceStoredRowUnselectable {
        namespace_id: NamespaceRowId,
    },
    ServiceNotFound {
        namespace_id: NamespaceRowId,
        service_name: CorrosionServiceName,
    },
    ServiceAmbiguous {
        namespace_id: NamespaceRowId,
        service_name: CorrosionServiceName,
        service_ids: Vec<ServiceRowId>,
    },
    ServiceIdMismatch {
        namespace_id: NamespaceRowId,
        service_name: CorrosionServiceName,
        requested: ServiceRowId,
        found: ServiceRowId,
    },
    ServiceIdentityMismatch {
        service_id: ServiceRowId,
        requested_namespace_id: NamespaceRowId,
        requested_name: CorrosionServiceName,
        found_namespace_id: NamespaceRowId,
        found_name: CorrosionServiceName,
    },
    ServiceStoredRowUnselectable {
        service_id: ServiceRowId,
    },
    HostnameAlreadyAttached {
        hostname: RouteHostname,
        route_id: RouteBindingRowId,
        remove: RouteRemoveRequest,
    },
}

/// The identity-free value compared when deciding whether attach is idempotent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteAttachIntent {
    pub hostname: RouteHostname,
    pub namespace_id: NamespaceRowId,
    pub service_id: ServiceRowId,
    pub endpoint_port: RoutePort,
    pub ingress_mode: IngressMode,
}

impl RouteAttachIntent {
    /// Returns true only when the stored route is the same declared intent.
    #[must_use]
    pub fn matches(&self, document: &RouteBindingDocument) -> bool {
        document.hostname == self.hostname
            && document.namespace_id == self.namespace_id
            && document.service_id == self.service_id
            && document.endpoint_port == self.endpoint_port
            && document.ingress_mode == self.ingress_mode
            && document.origin == crate::ingress::RouteBindingOrigin::Declared
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct IngressConfigureRequest {
    pub operation_id: OperationId,
    pub configuration: IngressConfiguration,
}

pub type IngressConfigureResponse = OperationApiResponse<AcceptedOperation, IngressConfigureError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum IngressConfigureError {
    #[error("invalid ingress configuration: {message}")]
    InvalidConfiguration { message: String },
    #[error("ingress configuration is busy with operation {}", .owner.as_str())]
    ResourceBusy { owner: OperationId },
    #[error("ingress configuration {} unavailable: {message}", .operation_id.as_str())]
    Unavailable {
        operation_id: OperationId,
        message: String,
    },
    #[error(
        "operation {} already recorded a different event at sequence {}",
        .operation_id.as_str(),
        .sequence.get()
    )]
    DuplicateSequenceMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_rejects_ployz_hostnames_without_dns_target() {
        let request = serde_json::json!({
            "operation_id": "op_ingress_configure",
            "configuration": {
                "automatic_hostnames": { "mode": "ployz" },
                "ployz_dns_target": "disabled"
            }
        });

        assert!(serde_json::from_value::<IngressConfigureRequest>(request).is_err());
    }
}
