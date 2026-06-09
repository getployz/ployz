//! WireGuard/eBPF preparation models.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::deploy::DeployPlan;
use crate::ids::{NodeId, OperationId};
use crate::ops::FailureMessage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireGuardEbpfPrepareRequest {
    pub operation_id: OperationId,
    pub nodes: Vec<NodeId>,
}

impl WireGuardEbpfPrepareRequest {
    #[must_use]
    pub fn for_deploy_plan(operation_id: OperationId, plan: &DeployPlan) -> Self {
        Self {
            operation_id,
            nodes: plan.target_nodes(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum WireGuardEbpfComponent {
    #[serde(rename = "wireguard")]
    WireGuard,
    EbpfForwarding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardEbpfPrepareReport {
    pub nodes: Vec<WireGuardEbpfNodeReady>,
}

impl WireGuardEbpfPrepareReport {
    pub fn for_request(
        request: &WireGuardEbpfPrepareRequest,
        nodes: impl IntoIterator<Item = WireGuardEbpfNodeReady>,
    ) -> Result<Self, WireGuardEbpfPrepareReportError> {
        let nodes = nodes.into_iter().collect::<Vec<_>>();
        if request.nodes.is_empty() || nodes.is_empty() {
            return Err(WireGuardEbpfPrepareReportError::Empty);
        }
        let requested = request.nodes.iter().collect::<BTreeSet<_>>();
        if requested.len() != request.nodes.len() {
            return Err(WireGuardEbpfPrepareReportError::DuplicateNode);
        }
        let actual = nodes
            .iter()
            .map(WireGuardEbpfNodeReady::node_id)
            .collect::<BTreeSet<_>>();
        if actual.len() != nodes.len() {
            return Err(WireGuardEbpfPrepareReportError::DuplicateNode);
        }
        if requested != actual {
            return Err(WireGuardEbpfPrepareReportError::NodeSetMismatch);
        }

        Ok(Self { nodes })
    }

    pub fn from_nodes(
        nodes: impl IntoIterator<Item = WireGuardEbpfNodeReady>,
    ) -> Result<Self, WireGuardEbpfPrepareReportError> {
        let nodes = nodes.into_iter().collect::<Vec<_>>();
        if nodes.is_empty() {
            return Err(WireGuardEbpfPrepareReportError::Empty);
        }
        let mut seen = BTreeSet::new();
        if nodes
            .iter()
            .any(|node| !seen.insert(node.node_id().clone()))
        {
            return Err(WireGuardEbpfPrepareReportError::DuplicateNode);
        }

        Ok(Self { nodes })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardEbpfNodeReady {
    node_id: NodeId,
    wireguard: WireGuardReady,
    ebpf_forwarding: EbpfForwardingReady,
}

impl WireGuardEbpfNodeReady {
    #[must_use]
    pub fn new(node_id: NodeId, ready: WireGuardEbpfReady) -> Self {
        Self {
            node_id,
            wireguard: ready.wireguard,
            ebpf_forwarding: ready.ebpf_forwarding,
        }
    }

    #[must_use]
    pub const fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    #[must_use]
    pub const fn wireguard(&self) -> &WireGuardReady {
        &self.wireguard
    }

    #[must_use]
    pub const fn ebpf_forwarding(&self) -> &EbpfForwardingReady {
        &self.ebpf_forwarding
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardEbpfReady {
    pub wireguard: WireGuardReady,
    pub ebpf_forwarding: EbpfForwardingReady,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardReady {
    pub evidence: Vec<WireGuardReadyEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct EbpfForwardingReady {
    pub evidence: Vec<EbpfForwardingReadyEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireGuardReadyEvidence {
    HostPath { path: String },
    Command { program: String, args: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EbpfForwardingReadyEvidence {
    HostPath { path: String },
    Command { program: String, args: Vec<String> },
    PloyzTcBytecode { path: String, symbols: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireGuardEbpfPrepareReportError {
    Empty,
    DuplicateNode,
    NodeSetMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireGuardEbpfPrepareError {
    Unavailable {
        node_id: NodeId,
        component: WireGuardEbpfComponent,
        message: FailureMessage,
    },
    InvalidReport {
        message: FailureMessage,
    },
}
