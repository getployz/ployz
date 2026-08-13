//! Typed, synchronous removal contracts for named resources.

use serde::{Deserialize, Serialize};

use crate::corrosion::{
    ClusterDocument, CorrosionServiceName, NamespaceDocument, OperatorWriteProvenance,
    PeerDocument, RouteBindingDocument, StoredRow, read_named_rows, read_roster_rows, read_rows,
};
use crate::ids::{CorrosionNamespaceName, PeerName};
use crate::operation::RouteHostname;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NamedRemovalOutcome {
    Removed,
    AlreadyAbsent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PeerRemoveRequest {
    pub peer_name: PeerName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PeerRemoveReply {
    pub peer_name: PeerName,
    pub outcome: NamedRemovalOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PeerRemoveRefusal {
    NotFound { peer_name: PeerName },
    StoredRowUnselectable { peer_name: PeerName },
    ConcurrentMutation { peer_name: PeerName },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerRemoveSelection {
    Delete {
        peer_name: PeerName,
        stored_document: String,
    },
    AlreadyAbsent {
        peer_name: PeerName,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct RouteRemoveRequest {
    pub hostname: RouteHostname,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RouteRemoveReply {
    pub hostname: RouteHostname,
    pub outcome: NamedRemovalOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteRemoveRefusal {
    NotFound { hostname: RouteHostname },
    StoredRowUnselectable { hostname: RouteHostname },
    ConcurrentMutation { hostname: RouteHostname },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteRemoveSelection {
    Delete {
        hostname: RouteHostname,
        stored_document: String,
    },
    AlreadyAbsent {
        hostname: RouteHostname,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceRemoveRequest {
    pub namespace_name: CorrosionNamespaceName,
    pub service_name: CorrosionServiceName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ServiceRemoveReply {
    pub namespace_name: CorrosionNamespaceName,
    pub service_name: CorrosionServiceName,
    pub outcome: NamedRemovalOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceRemoveRefusal {
    NotFound {
        namespace_name: CorrosionNamespaceName,
        service_name: CorrosionServiceName,
    },
    NamespaceStoredRowUnselectable {
        namespace_name: CorrosionNamespaceName,
    },
    ConcurrentMutation {
        namespace_name: CorrosionNamespaceName,
        service_name: CorrosionServiceName,
    },
    RuntimeCleanupIncomplete {
        namespace_name: CorrosionNamespaceName,
        service_name: CorrosionServiceName,
        machines: Vec<crate::machine::MachineName>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceRemoveSelection {
    Replace {
        namespace_name: CorrosionNamespaceName,
        stored_document: String,
        replacement_document: NamespaceDocument,
    },
    AlreadyAbsent {
        namespace_name: CorrosionNamespaceName,
        service_name: CorrosionServiceName,
    },
}

pub fn select_peer_removal(
    cluster: &ClusterDocument,
    rows: Vec<StoredRow>,
    request: &PeerRemoveRequest,
) -> Result<PeerRemoveSelection, PeerRemoveRefusal> {
    let key = request.peer_name.as_str();
    let report = read_roster_rows::<PeerDocument>(cluster, rows.clone());
    let Some(row) = report
        .accepted
        .into_iter()
        .find(|row| row.source.key == key)
    else {
        if rows.iter().any(|row| row.key == key) {
            return Err(PeerRemoveRefusal::StoredRowUnselectable {
                peer_name: request.peer_name.clone(),
            });
        }
        return Err(PeerRemoveRefusal::NotFound {
            peer_name: request.peer_name.clone(),
        });
    };
    Ok(PeerRemoveSelection::Delete {
        peer_name: request.peer_name.clone(),
        stored_document: row.source.document,
    })
}

pub fn select_service_removal(
    cluster_id: &crate::ids::ClusterName,
    row: Option<StoredRow>,
    request: &ServiceRemoveRequest,
    provenance: OperatorWriteProvenance,
) -> Result<ServiceRemoveSelection, ServiceRemoveRefusal> {
    let Some(row) = row else {
        return Err(ServiceRemoveRefusal::NotFound {
            namespace_name: request.namespace_name.clone(),
            service_name: request.service_name.clone(),
        });
    };
    let report = read_named_rows::<NamespaceDocument>(cluster_id, [row]);
    let Some(mut row) = report.accepted.into_iter().next() else {
        return Err(ServiceRemoveRefusal::NamespaceStoredRowUnselectable {
            namespace_name: request.namespace_name.clone(),
        });
    };
    if row.value.services.remove(&request.service_name).is_none() {
        return Ok(ServiceRemoveSelection::AlreadyAbsent {
            namespace_name: request.namespace_name.clone(),
            service_name: request.service_name.clone(),
        });
    }
    row.value.provenance = provenance;
    Ok(ServiceRemoveSelection::Replace {
        namespace_name: request.namespace_name.clone(),
        stored_document: row.source.document,
        replacement_document: row.value,
    })
}

pub fn select_route_removal(
    cluster_id: &crate::ids::ClusterName,
    rows: Vec<StoredRow>,
    request: &RouteRemoveRequest,
) -> Result<RouteRemoveSelection, RouteRemoveRefusal> {
    let key = request.hostname.as_str();
    let report = read_rows::<RouteBindingDocument>(cluster_id, rows.clone());
    let Some(row) = report
        .accepted
        .into_iter()
        .find(|row| row.source.key == key)
    else {
        if rows.iter().any(|row| row.key == key) {
            return Err(RouteRemoveRefusal::StoredRowUnselectable {
                hostname: request.hostname.clone(),
            });
        }
        return Err(RouteRemoveRefusal::NotFound {
            hostname: request.hostname.clone(),
        });
    };
    Ok(RouteRemoveSelection::Delete {
        hostname: request.hostname.clone(),
        stored_document: row.source.document,
    })
}
