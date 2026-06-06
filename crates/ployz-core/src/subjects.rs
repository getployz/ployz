//! NATS subject construction helpers.

use crate::ids::{CertId, ContainerId, NodeId, OperationId};
use crate::ops::DeployRunningStage;

pub const OPS_STREAM_SUBJECT: &str = "plz.v1.op.>";
pub const JOBS_STREAM_SUBJECT: &str = "plz.v1.job.>";
pub const AUDIT_STREAM_SUBJECT: &str = "plz.v1.audit.>";
pub const OBS_TRANSITION_STREAM_SUBJECT: &str = "plz.v1.obs.node.>";
pub const SCHEDULE_STREAM_SUBJECT: &str = "plz.v1.sched.>";

pub const API_SERVICE_SCOPE: &str = "plz.v1.svc.api.>";
pub const NODE_SERVICE_SCOPE: &str = "plz.v1.svc.node.>";

pub const API_DEPLOY_SUBMIT: &str = "plz.v1.svc.api.deploy.submit";
pub const API_DEPLOY_PLAN: &str = "plz.v1.svc.api.deploy.plan";
pub const API_OPS_STATUS: &str = "plz.v1.svc.api.ops.status";
pub const API_OPS_WATCH: &str = "plz.v1.svc.api.ops.watch";
pub const API_MACHINE_ADD: &str = "plz.v1.svc.api.machine.add";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationApiEndpoint {
    DeploySubmit,
    OpsStatus,
    OpsWatch,
}

pub const OPERATION_API_ENDPOINTS: [OperationApiEndpoint; 3] = [
    OperationApiEndpoint::DeploySubmit,
    OperationApiEndpoint::OpsStatus,
    OperationApiEndpoint::OpsWatch,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationApiEndpointExecution {
    AcceptsOperation,
    Query,
}

impl OperationApiEndpoint {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::DeploySubmit => "deploy.submit",
            Self::OpsStatus => "ops.status",
            Self::OpsWatch => "ops.watch",
        }
    }

    #[must_use]
    pub const fn subject(self) -> &'static str {
        match self {
            Self::DeploySubmit => API_DEPLOY_SUBMIT,
            Self::OpsStatus => API_OPS_STATUS,
            Self::OpsWatch => API_OPS_WATCH,
        }
    }

    #[must_use]
    pub const fn execution(self) -> OperationApiEndpointExecution {
        match self {
            Self::DeploySubmit => OperationApiEndpointExecution::AcceptsOperation,
            Self::OpsStatus | Self::OpsWatch => OperationApiEndpointExecution::Query,
        }
    }
}

#[must_use]
pub fn op_watch(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.>", operation_id.as_str())
}

#[must_use]
pub fn op_deploy_submitted(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.deploy.submitted", operation_id.as_str())
}

#[must_use]
pub fn op_deploy_planning_started(operation_id: &OperationId) -> String {
    format!(
        "plz.v1.op.{}.deploy.planning.started",
        operation_id.as_str()
    )
}

#[must_use]
pub fn op_deploy_plan_created(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.deploy.plan.created", operation_id.as_str())
}

#[must_use]
pub fn op_deploy_running(operation_id: &OperationId, stage: DeployRunningStage) -> String {
    format!(
        "plz.v1.op.{}.deploy.running.{}",
        operation_id.as_str(),
        stage.as_subject(),
    )
}

#[must_use]
pub fn op_deploy_container_started(
    operation_id: &OperationId,
    node_id: &NodeId,
    container_id: &ContainerId,
) -> String {
    format!(
        "plz.v1.op.{}.deploy.container.started.{}.{}",
        operation_id.as_str(),
        node_id.as_str(),
        container_id.as_str()
    )
}

#[must_use]
pub fn op_deploy_health_check_started(operation_id: &OperationId) -> String {
    format!(
        "plz.v1.op.{}.deploy.health_check.started",
        operation_id.as_str()
    )
}

#[must_use]
pub fn op_deploy_completed(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.deploy.completed", operation_id.as_str())
}

#[must_use]
pub fn op_deploy_failed(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.deploy.failed", operation_id.as_str())
}

#[must_use]
pub fn op_cancelled(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.cancelled", operation_id.as_str())
}

#[must_use]
pub fn op_cert_submitted(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.cert.submitted", operation_id.as_str())
}

#[must_use]
pub fn op_cert_challenge_published(operation_id: &OperationId) -> String {
    format!(
        "plz.v1.op.{}.cert.challenge.published",
        operation_id.as_str()
    )
}

#[must_use]
pub fn op_cert_validation_started(operation_id: &OperationId) -> String {
    format!(
        "plz.v1.op.{}.cert.validation.started",
        operation_id.as_str()
    )
}

#[must_use]
pub fn op_cert_completed(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.cert.completed", operation_id.as_str())
}

#[must_use]
pub fn op_cert_failed(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.cert.failed", operation_id.as_str())
}

#[must_use]
pub fn cert_renewal_schedule(cert_id: &CertId) -> String {
    format!("plz.v1.sched.cert.renew.{}", cert_id.as_str())
}

#[must_use]
pub fn cert_renewal_job(cert_id: &CertId) -> String {
    format!("plz.v1.job.cert.renew.{}", cert_id.as_str())
}

#[must_use]
pub fn node_service(node_id: &NodeId, endpoint: NodeServiceEndpoint) -> String {
    format!(
        "plz.v1.svc.node.{}.{}",
        node_id.as_str(),
        endpoint.as_subject()
    )
}

#[must_use]
pub fn node_service_scope(node_id: &NodeId) -> String {
    format!("plz.v1.svc.node.{}.>", node_id.as_str())
}

impl DeployRunningStage {
    #[must_use]
    pub const fn as_subject(&self) -> &'static str {
        match self {
            Self::StartingContainers => "starting_containers",
            Self::WaitingForHealth => "waiting_for_health",
            Self::ActiveServiceCommit => "active_service_commit",
        }
    }
}

#[must_use]
pub fn node_observation(node_id: &NodeId, event: NodeObservationEvent) -> String {
    format!(
        "plz.v1.obs.node.{}.{}",
        node_id.as_str(),
        event.as_subject()
    )
}

#[must_use]
pub fn node_observation_scope(node_id: &NodeId) -> String {
    format!("plz.v1.obs.node.{}.>", node_id.as_str())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeServiceEndpoint {
    Inspect,
    ContainerRun,
    LogsTail,
}

impl NodeServiceEndpoint {
    #[must_use]
    pub const fn as_subject(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::ContainerRun => "container.run",
            Self::LogsTail => "logs.tail",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeObservationEvent {
    Heartbeat,
    PublicIpChanged,
    ContainerRunning,
    ContainerExited,
}

impl NodeObservationEvent {
    #[must_use]
    pub const fn as_subject(self) -> &'static str {
        match self {
            Self::Heartbeat => "heartbeat",
            Self::PublicIpChanged => "public_ip.changed",
            Self::ContainerRunning => "container.running",
            Self::ContainerExited => "container.exited",
        }
    }
}
