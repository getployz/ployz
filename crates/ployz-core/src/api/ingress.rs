use serde::{Deserialize, Serialize};

use crate::corrosion::{
    CorrosionNamespaceName, CorrosionServiceName, IngressMode, RouteBindingDocument,
};
use crate::operation::{RouteHostname, RoutePort};

use super::removal::RouteRemoveRequest;

/// Mesh-authenticated request to attach one hostname to one named service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct RouteAttachRequest {
    pub hostname: RouteHostname,
    pub namespace_name: CorrosionNamespaceName,
    pub service_name: CorrosionServiceName,
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
    pub outcome: RouteAttachOutcome,
}

/// A route attach refusal for canonical named resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteAttachRefusal {
    UnsupportedIngressMode {
        requested: IngressMode,
    },
    NamespaceNotFound {
        namespace_name: CorrosionNamespaceName,
    },
    NamespaceStoredRowUnselectable {
        namespace_name: CorrosionNamespaceName,
    },
    ServiceNotFound {
        namespace_name: CorrosionNamespaceName,
        service_name: CorrosionServiceName,
    },
    HostnameAlreadyAttached {
        hostname: RouteHostname,
        remove: RouteRemoveRequest,
    },
}

/// The identity-free value compared when deciding whether attach is idempotent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteAttachIntent {
    pub hostname: RouteHostname,
    pub namespace_id: CorrosionNamespaceName,
    pub service_name: CorrosionServiceName,
    pub endpoint_port: RoutePort,
    pub ingress_mode: IngressMode,
}

impl RouteAttachIntent {
    /// Returns true only when the stored route is the same declared intent.
    #[must_use]
    pub fn matches(&self, document: &RouteBindingDocument) -> bool {
        document.hostname == self.hostname
            && document.namespace_id == self.namespace_id
            && document.service_name == self.service_name
            && document.endpoint_port == self.endpoint_port
            && document.ingress_mode == self.ingress_mode
            && document.origin == crate::ingress::RouteBindingOrigin::Declared
    }
}
