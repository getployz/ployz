use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::deploy::ProductDeployOptions;
use crate::error::{NodeError, NodeResult};

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub(super) enum DaemonControlRequest {
    Status,
    Deploy(DaemonDeployRequest),
}

#[derive(Debug, Deserialize)]
pub(super) struct DaemonDeployRequest {
    deploy_id: String,
    target_node: String,
    service: String,
    revision: String,
    hostname: String,
}

impl DaemonDeployRequest {
    pub(super) fn into_options(self, state_dir: PathBuf) -> ProductDeployOptions {
        ProductDeployOptions::new(state_dir)
            .with_deploy_id(self.deploy_id)
            .with_target_node(self.target_node)
            .with_service(self.service)
            .with_revision(self.revision)
            .with_hostname(self.hostname)
    }
}

#[derive(Debug, Serialize)]
struct DaemonStatusResponse<'a> {
    status: &'static str,
    node: &'a str,
    imported_batches: u64,
    imported_operations: u64,
    node_agent_handlers: usize,
}

#[derive(Debug, Serialize)]
struct DaemonDeployResponse {
    status: &'static str,
    deploy_id: String,
    active_backends: Vec<String>,
    old_backends: usize,
    old_backends_to_drain: Vec<String>,
    visible_nodes: usize,
    host_network_backends: usize,
}

#[derive(Debug, Serialize)]
struct DaemonFailureResponse {
    status: &'static str,
    error: String,
}

pub(super) fn parse_daemon_control_request(bytes: &[u8]) -> Option<DaemonControlRequest> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    if text.is_empty() || text == "status" {
        return Some(DaemonControlRequest::Status);
    }
    serde_json::from_str(text).ok()
}

pub(super) fn daemon_status_json(
    node: &str,
    imported_batches: u64,
    imported_operations: u64,
    node_agent_handlers: usize,
) -> NodeResult<String> {
    serde_json::to_string(&DaemonStatusResponse {
        status: "ready",
        node,
        imported_batches,
        imported_operations,
        node_agent_handlers,
    })
    .map_err(|source| NodeError::EncodeNodeAgentRpc { source })
}

pub(super) fn daemon_failure_json(message: impl Into<String>) -> NodeResult<String> {
    serde_json::to_string(&DaemonFailureResponse {
        status: "failed",
        error: message.into(),
    })
    .map_err(|source| NodeError::EncodeNodeAgentRpc { source })
}

impl From<crate::ProductDeployReport> for DaemonDeployResponse {
    fn from(report: crate::ProductDeployReport) -> Self {
        Self {
            status: "deployed",
            deploy_id: report.deploy_id.to_string(),
            active_backends: report
                .active_backends
                .into_iter()
                .map(|backend| format!("{}@{}", backend.node_id, backend.address))
                .collect(),
            old_backends: report.old_backends_to_drain.len(),
            old_backends_to_drain: report
                .old_backends_to_drain
                .into_iter()
                .map(|backend| format!("{}@{}", backend.node_id, backend.address))
                .collect(),
            visible_nodes: report.visible_nodes,
            host_network_backends: report.host_network_backends,
        }
    }
}

pub(super) fn daemon_deploy_json(report: crate::ProductDeployReport) -> NodeResult<String> {
    serde_json::to_string(&DaemonDeployResponse::from(report))
        .map_err(|source| NodeError::EncodeNodeAgentRpc { source })
}
