//! WireGuard/eBPF preparation models.

use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireGuardEbpfPrepareError {
    Unavailable {
        node_id: NodeId,
        component: WireGuardEbpfComponent,
        message: FailureMessage,
    },
}
