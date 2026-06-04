//! NATS subject construction helpers.

use crate::ids::{NodeId, OperationId};
use crate::ops::DeployRunningStage;

pub const OPS_STREAM_SUBJECT: &str = "plz.v1.op.>";
pub const JOBS_STREAM_SUBJECT: &str = "plz.v1.job.>";
pub const AUDIT_STREAM_SUBJECT: &str = "plz.v1.audit.>";
pub const OBS_TRANSITION_STREAM_SUBJECT: &str = "plz.v1.obs.node.>";
pub const SCHEDULE_STREAM_SUBJECT: &str = "plz.v1.sched.>";

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
pub fn op_deploy_running(operation_id: &OperationId, stage: DeployRunningStage) -> String {
    format!(
        "plz.v1.op.{}.deploy.running.{}",
        operation_id.as_str(),
        stage.as_subject(),
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

impl DeployRunningStage {
    #[must_use]
    pub const fn as_subject(&self) -> &'static str {
        match self {
            Self::RunningPreDeploy => "predeploy",
            Self::StartingContainers => "starting_containers",
            Self::WaitingForHealth => "waiting_for_health",
            Self::CuttingOverRoute => "cutting_over_route",
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
