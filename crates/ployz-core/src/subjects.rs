//! NATS subject construction helpers.

use crate::ids::{CertId, MachineId, OperationId};
use crate::ops::DeployRunningStage;

pub const OPS_STREAM_SUBJECT: &str = "plz.v1.op.>";

pub const API_SERVICE_SCOPE: &str = "plz.v1.svc.api.>";
pub const MACHINE_SERVICE_SCOPE: &str = "plz.v1.svc.machine.>";

pub const API_DEPLOY_SUBMIT: &str = "plz.v1.svc.api.deploy.submit";
pub const API_DEPLOY_PLAN: &str = "plz.v1.svc.api.deploy.plan";
pub const API_OPS_LIST: &str = "plz.v1.svc.api.ops.list";
pub const API_OPS_STATUS: &str = "plz.v1.svc.api.ops.status";
pub const API_OPS_WATCH: &str = "plz.v1.svc.api.ops.watch";
pub const API_INIT_FIRST_MACHINE_ACTIVATE: &str = "plz.v1.svc.api.init.first_machine.activate";
pub const API_MACHINE_ADD: &str = "plz.v1.svc.api.machine.add";
pub const API_MACHINE_UPDATE: &str = "plz.v1.svc.api.machine.update";
pub const API_MACHINE_LIST: &str = "plz.v1.svc.api.machine.list";
pub const API_MACHINE_INSPECT: &str = "plz.v1.svc.api.machine.inspect";
pub const API_MACHINE_JOIN_REDEEM: &str = "plz.v1.svc.api.machine.join.redeem";
pub const API_MACHINE_JOIN_REPORT: &str = "plz.v1.svc.api.machine.join.report";
pub const API_SERVICE_LIST: &str = "plz.v1.svc.api.service.list";
pub const API_SERVICE_INSPECT: &str = "plz.v1.svc.api.service.inspect";
pub const API_RUNTIME_SNAPSHOT: &str = "plz.v1.svc.api.runtime.snapshot";
pub const API_LOGS_TAIL: &str = "plz.v1.svc.api.logs.tail";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationApiEndpoint {
    DeploySubmit,
    InitFirstMachineActivate,
    MachineAdd,
    MachineUpdate,
    MachineList,
    MachineInspect,
    MachineJoinRedeem,
    MachineJoinReport,
    ServiceList,
    ServiceInspect,
    RuntimeSnapshot,
    LogsTail,
    OpsList,
    OpsStatus,
    OpsWatch,
}

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
            Self::InitFirstMachineActivate => "init.first_machine.activate",
            Self::MachineAdd => "machine.add",
            Self::MachineUpdate => "machine.update",
            Self::MachineList => "machine.list",
            Self::MachineInspect => "machine.inspect",
            Self::MachineJoinRedeem => "machine.join.redeem",
            Self::MachineJoinReport => "machine.join.report",
            Self::ServiceList => "service.list",
            Self::ServiceInspect => "service.inspect",
            Self::RuntimeSnapshot => "runtime.snapshot",
            Self::LogsTail => "logs.tail",
            Self::OpsList => "ops.list",
            Self::OpsStatus => "ops.status",
            Self::OpsWatch => "ops.watch",
        }
    }

    #[must_use]
    pub const fn subject(self) -> &'static str {
        match self {
            Self::DeploySubmit => API_DEPLOY_SUBMIT,
            Self::InitFirstMachineActivate => API_INIT_FIRST_MACHINE_ACTIVATE,
            Self::MachineAdd => API_MACHINE_ADD,
            Self::MachineUpdate => API_MACHINE_UPDATE,
            Self::MachineList => API_MACHINE_LIST,
            Self::MachineInspect => API_MACHINE_INSPECT,
            Self::MachineJoinRedeem => API_MACHINE_JOIN_REDEEM,
            Self::MachineJoinReport => API_MACHINE_JOIN_REPORT,
            Self::ServiceList => API_SERVICE_LIST,
            Self::ServiceInspect => API_SERVICE_INSPECT,
            Self::RuntimeSnapshot => API_RUNTIME_SNAPSHOT,
            Self::LogsTail => API_LOGS_TAIL,
            Self::OpsList => API_OPS_LIST,
            Self::OpsStatus => API_OPS_STATUS,
            Self::OpsWatch => API_OPS_WATCH,
        }
    }

    #[must_use]
    pub const fn execution(self) -> OperationApiEndpointExecution {
        match self {
            Self::DeploySubmit | Self::MachineAdd | Self::MachineUpdate => {
                OperationApiEndpointExecution::AcceptsOperation
            }
            Self::InitFirstMachineActivate | Self::MachineJoinRedeem | Self::MachineJoinReport => {
                OperationApiEndpointExecution::MutatesOperation
            }
            Self::MachineList
            | Self::MachineInspect
            | Self::ServiceList
            | Self::ServiceInspect
            | Self::RuntimeSnapshot
            | Self::LogsTail
            | Self::OpsList
            | Self::OpsStatus
            | Self::OpsWatch => OperationApiEndpointExecution::Query,
        }
    }
}

/// One operation's event subject for a suffix rendered by
/// [`crate::ops::OperationEvent::subject`]. The prefix here and the
/// [`op_watch`] pattern must agree.
#[must_use]
pub fn op_event_subject(operation_id: &OperationId, suffix: &str) -> String {
    format!("plz.v1.op.{}.{suffix}", operation_id.as_str())
}

#[must_use]
pub fn op_watch(operation_id: &OperationId) -> String {
    op_event_subject(operation_id, ">")
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
pub fn machine_service(machine_id: &MachineId, endpoint: MachineServiceEndpoint) -> String {
    format!(
        "plz.v1.svc.machine.{}.{}",
        machine_id.as_str(),
        endpoint.as_subject()
    )
}

#[must_use]
pub fn machine_service_scope(machine_id: &MachineId) -> String {
    format!("plz.v1.svc.machine.{}.>", machine_id.as_str())
}

impl DeployRunningStage {
    #[must_use]
    pub const fn as_subject(&self) -> &'static str {
        match self {
            Self::PreparingDataplane => "preparing_dataplane",
            Self::StartingContainers => "starting_containers",
            Self::WaitingForHealth => "waiting_for_health",
            Self::RouteCutover => "route_cutover",
            Self::ServingTargetCommit => "serving_target_commit",
            Self::RemovingSupersededContainers => "removing_superseded_containers",
        }
    }
}

#[must_use]
pub fn machine_observation(machine_id: &MachineId, event: MachineObservationEvent) -> String {
    format!(
        "plz.v1.obs.machine.{}.{}",
        machine_id.as_str(),
        event.as_subject()
    )
}

#[must_use]
pub fn machine_observation_scope(machine_id: &MachineId) -> String {
    format!("plz.v1.obs.machine.{}.>", machine_id.as_str())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineServiceEndpoint {
    Inspect,
    ContainerEnsureEndpointNetwork,
    ContainerRun,
    ContainerStop,
    ContainerRemove,
    DataplanePrepare,
    SubstrateUpdate,
    SubstrateReport,
    LogsTail,
}

impl MachineServiceEndpoint {
    #[must_use]
    pub const fn as_subject(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::ContainerEnsureEndpointNetwork => "container.ensure_endpoint_network",
            Self::ContainerRun => "container.run",
            Self::ContainerStop => "container.stop",
            Self::ContainerRemove => "container.remove",
            Self::DataplanePrepare => "dataplane.prepare",
            Self::SubstrateUpdate => "substrate.update",
            Self::SubstrateReport => "substrate.report",
            Self::LogsTail => "logs.tail",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineObservationEvent {
    Heartbeat,
    PublicIpChanged,
    ContainerRunning,
    ContainerExited,
}

impl MachineObservationEvent {
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
