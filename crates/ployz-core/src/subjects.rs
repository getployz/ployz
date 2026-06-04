//! NATS subject construction helpers.

use crate::ids::{ContainerId, NodeId, OperationId};
use crate::ops::DeployRunningStage;

pub const OPS_STREAM_SUBJECT: &str = "plz.v1.op.>";
pub const JOBS_STREAM_SUBJECT: &str = "plz.v1.job.>";
pub const AUDIT_STREAM_SUBJECT: &str = "plz.v1.audit.>";
pub const OBS_TRANSITION_STREAM_SUBJECT: &str = "plz.v1.obs.node.>";
pub const SCHEDULE_STREAM_SUBJECT: &str = "plz.v1.sched.>";

pub const API_SERVICE_SCOPE: &str = "plz.v1.svc.api.>";
pub const NODE_SERVICE_SCOPE: &str = "plz.v1.svc.node.>";
pub const DEPLOY_SUBMITTED_EVENTS_SUBJECT: &str = "plz.v1.op.*.deploy.submitted";

pub const API_DEPLOY_SUBMIT: &str = "plz.v1.svc.api.deploy.submit";
pub const API_DEPLOY_PLAN: &str = "plz.v1.svc.api.deploy.plan";
pub const API_OPS_STATUS: &str = "plz.v1.svc.api.ops.status";
pub const API_OPS_WATCH: &str = "plz.v1.svc.api.ops.watch";
pub const API_MACHINE_ADD: &str = "plz.v1.svc.api.machine.add";

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
            Self::RouteCutover => "route_cutover",
            Self::ActiveServiceCommit => "active_service_commit",
            Self::CleaningUp => "cleaning_up",
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
