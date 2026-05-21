use serde::{Deserialize, Serialize};

use crate::three_server_harness::{
    ContainerDnsClientProbe, HttpProbe, HttpsProbe, ProductCommandRecord, ServingRoleProbe,
};

pub(super) const SCENARIO: &str = "three-node-parity-smoke";
pub(super) const NODES: &[&str] = &["node-a", "node-b", "node-c"];

#[derive(Debug, Serialize)]
pub(super) struct ThreeNodeParityReport {
    pub scenario: &'static str,
    pub stage: &'static str,
    pub installed_node_bin: String,
    pub nodes: Vec<&'static str>,
    pub privileged_preflight: PrivilegedPreflight,
    pub installed_help: ProductCommandRecord,
    pub bootstrap_commands: Vec<ProductCommandRecord>,
    pub join_commands: Vec<ProductCommandRecord>,
    pub admit_commands: Vec<ProductCommandRecord>,
    pub daemon_statuses: Vec<NodeCommandStatus>,
    pub runtime_placements: Vec<RuntimePlacementEvidence>,
    pub gateway_http_checks: Vec<GatewayHttpEvidence>,
    pub acme_https_checks: Vec<GatewayHttpsEvidence>,
    pub container_dns_checks: Vec<ContainerDnsEvidence>,
    pub update_drain_checks: Vec<UpdateDrainEvidence>,
    pub daemon_restart_checks: Vec<DaemonRestartEvidence>,
    pub gateway_statuses: Vec<NodeRoleStatus>,
    pub dns_statuses: Vec<NodeRoleStatus>,
    pub elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
pub(super) struct PrivilegedPreflight {
    pub missing_prerequisites: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct NodeCommandStatus {
    pub node: &'static str,
    pub status: ProductCommandRecord,
}

#[derive(Debug, Serialize)]
pub(super) struct NodeRoleStatus {
    pub node: &'static str,
    pub status: ServingRoleProbe,
}

#[derive(Debug, Serialize)]
pub(super) struct RuntimePlacementEvidence {
    pub service: &'static str,
    pub target_node: &'static str,
    pub deploy: ProductCommandRecord,
    pub deploy_status: ProductCommandRecord,
    pub active_backends: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct GatewayHttpEvidence {
    pub service: &'static str,
    pub gateway_node: &'static str,
    pub probe: HttpProbe,
}

#[derive(Debug, Serialize)]
pub(super) struct GatewayHttpsEvidence {
    pub service: &'static str,
    pub gateway_node: &'static str,
    pub acme_issue: ProductCommandRecord,
    pub probe: HttpsProbe,
}

#[derive(Debug, Serialize)]
pub(super) struct ContainerDnsEvidence {
    pub client_node: &'static str,
    pub service: &'static str,
    pub dns_answer: String,
    pub probe: ContainerDnsClientProbe,
}

#[derive(Debug, Serialize)]
pub(super) struct UpdateDrainEvidence {
    pub service: &'static str,
    pub target_node: &'static str,
    pub pre_update_checks: Vec<GatewayHttpEvidence>,
    pub update_deploy: ProductCommandRecord,
    pub update_deploy_status: ProductCommandRecord,
    pub updated_active_backends: Vec<String>,
    pub old_backends_to_drain: usize,
    pub gateway_reloads: Vec<NodeRoleStatus>,
    pub post_update_checks: Vec<GatewayHttpEvidence>,
}

#[derive(Debug, Serialize)]
pub(super) struct DaemonRestartEvidence {
    pub restarted_node: &'static str,
    pub post_restart_status: ProductCommandRecord,
    pub post_restart_checks: Vec<GatewayHttpEvidence>,
    pub post_restart_container_dns: Vec<ContainerDnsEvidence>,
}

#[derive(Debug, Deserialize)]
pub(super) struct DeployResponse {
    pub active_backends: Vec<String>,
    pub old_backends: usize,
}
