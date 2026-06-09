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
pub const API_MACHINE_LIST: &str = "plz.v1.svc.api.machine.list";
pub const API_MACHINE_INSPECT: &str = "plz.v1.svc.api.machine.inspect";
pub const API_MACHINE_JOIN_REDEEM: &str = "plz.v1.svc.api.machine.join.redeem";
pub const API_MACHINE_JOIN_REPORT: &str = "plz.v1.svc.api.machine.join.report";
pub const API_SERVICE_LIST: &str = "plz.v1.svc.api.service.list";
pub const API_SERVICE_INSPECT: &str = "plz.v1.svc.api.service.inspect";
pub const API_LOGS_TAIL: &str = "plz.v1.svc.api.logs.tail";
pub const API_BACKUP_CREATE: &str = "plz.v1.svc.api.backup.create";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationApiEndpoint {
    DeploySubmit,
    MachineAdd,
    MachineList,
    MachineInspect,
    MachineJoinRedeem,
    MachineJoinReport,
    ServiceList,
    ServiceInspect,
    LogsTail,
    OpsStatus,
    OpsWatch,
    BackupCreate,
}

pub const OPERATION_API_ENDPOINTS: [OperationApiEndpoint; 12] = [
    OperationApiEndpoint::DeploySubmit,
    OperationApiEndpoint::MachineAdd,
    OperationApiEndpoint::MachineList,
    OperationApiEndpoint::MachineInspect,
    OperationApiEndpoint::MachineJoinRedeem,
    OperationApiEndpoint::MachineJoinReport,
    OperationApiEndpoint::ServiceList,
    OperationApiEndpoint::ServiceInspect,
    OperationApiEndpoint::LogsTail,
    OperationApiEndpoint::OpsStatus,
    OperationApiEndpoint::OpsWatch,
    OperationApiEndpoint::BackupCreate,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationApiEndpointExecution {
    AcceptsOperation,
    MutatesOperation,
    Query,
}

impl OperationApiEndpoint {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::DeploySubmit => "deploy.submit",
            Self::MachineAdd => "machine.add",
            Self::MachineList => "machine.list",
            Self::MachineInspect => "machine.inspect",
            Self::MachineJoinRedeem => "machine.join.redeem",
            Self::MachineJoinReport => "machine.join.report",
            Self::ServiceList => "service.list",
            Self::ServiceInspect => "service.inspect",
            Self::LogsTail => "logs.tail",
            Self::OpsStatus => "ops.status",
            Self::OpsWatch => "ops.watch",
            Self::BackupCreate => "backup.create",
        }
    }

    #[must_use]
    pub const fn subject(self) -> &'static str {
        match self {
            Self::DeploySubmit => API_DEPLOY_SUBMIT,
            Self::MachineAdd => API_MACHINE_ADD,
            Self::MachineList => API_MACHINE_LIST,
            Self::MachineInspect => API_MACHINE_INSPECT,
            Self::MachineJoinRedeem => API_MACHINE_JOIN_REDEEM,
            Self::MachineJoinReport => API_MACHINE_JOIN_REPORT,
            Self::ServiceList => API_SERVICE_LIST,
            Self::ServiceInspect => API_SERVICE_INSPECT,
            Self::LogsTail => API_LOGS_TAIL,
            Self::OpsStatus => API_OPS_STATUS,
            Self::OpsWatch => API_OPS_WATCH,
            Self::BackupCreate => API_BACKUP_CREATE,
        }
    }

    #[must_use]
    pub const fn execution(self) -> OperationApiEndpointExecution {
        match self {
            Self::DeploySubmit | Self::MachineAdd | Self::BackupCreate => {
                OperationApiEndpointExecution::AcceptsOperation
            }
            Self::MachineJoinRedeem | Self::MachineJoinReport => {
                OperationApiEndpointExecution::MutatesOperation
            }
            Self::MachineList
            | Self::MachineInspect
            | Self::ServiceList
            | Self::ServiceInspect
            | Self::LogsTail
            | Self::OpsStatus
            | Self::OpsWatch => OperationApiEndpointExecution::Query,
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
pub fn op_deploy_wireguard_ebpf_prepared(operation_id: &OperationId) -> String {
    format!(
        "plz.v1.op.{}.deploy.wireguard_ebpf.prepared",
        operation_id.as_str()
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
pub fn op_deploy_cleanup_finished(operation_id: &OperationId) -> String {
    format!(
        "plz.v1.op.{}.deploy.cleanup.finished",
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
pub fn op_machine_add_submitted(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.machine.add.submitted", operation_id.as_str())
}

#[must_use]
pub fn op_machine_add_joined(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.machine.add.joined", operation_id.as_str())
}

#[must_use]
pub fn op_machine_add_completed(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.machine.add.completed", operation_id.as_str())
}

#[must_use]
pub fn op_machine_add_failed(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.machine.add.failed", operation_id.as_str())
}

#[must_use]
pub fn op_backup_submitted(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.backup.submitted", operation_id.as_str())
}

#[must_use]
pub fn op_backup_running(
    operation_id: &OperationId,
    stage: crate::ops::BackupRunningStage,
) -> String {
    format!(
        "plz.v1.op.{}.backup.running.{}",
        operation_id.as_str(),
        stage.as_subject(),
    )
}

#[must_use]
pub fn op_backup_completed(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.backup.completed", operation_id.as_str())
}

#[must_use]
pub fn op_backup_failed(operation_id: &OperationId) -> String {
    format!("plz.v1.op.{}.backup.failed", operation_id.as_str())
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
            Self::PreparingWireGuardEbpf => "preparing_wireguard_ebpf",
            Self::StartingContainers => "starting_containers",
            Self::WaitingForHealth => "waiting_for_health",
            Self::RouteCutover => "route_cutover",
            Self::ActiveServiceCommit => "active_service_commit",
            Self::RemovingSupersededContainers => "removing_superseded_containers",
        }
    }
}

impl crate::ops::BackupRunningStage {
    #[must_use]
    pub const fn as_subject(&self) -> &'static str {
        match self {
            Self::SnapshottingControlPlane => "snapshotting_control_plane",
            Self::WritingManifest { .. } => "writing_manifest",
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
    ContainerRemove,
    WireGuardEbpfPrepare,
    LogsTail,
}

impl NodeServiceEndpoint {
    #[must_use]
    pub const fn as_subject(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::ContainerRun => "container.run",
            Self::ContainerRemove => "container.remove",
            Self::WireGuardEbpfPrepare => "wireguard_ebpf.prepare",
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
