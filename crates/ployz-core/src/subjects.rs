//! NATS subject construction helpers.

use crate::ids::{CertId, MachineId, OperationId};
use crate::ops::DeployRunningStage;

pub const OPS_STREAM_SUBJECT: &str = "plz.v1.op.>";

pub const API_SERVICE_SCOPE: &str = "plz.v1.svc.api.>";
pub const MACHINE_SERVICE_SCOPE: &str = "plz.v1.svc.machine.>";
pub const INTENT_GET: &str = "plz.v1.svc.intent.get";
pub const INTENT_CHANGED: &str = "plz.v1.intent.changed";

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
pub const API_MACHINE_DRAIN: &str = "plz.v1.svc.api.machine.drain";
pub const API_MACHINE_RESUME: &str = "plz.v1.svc.api.machine.resume";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationApiEndpoint {
    DeploySubmit,
    InitFirstMachineActivate,
    MachineAdd,
    MachineUpdate,
    MachineDrain,
    MachineResume,
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
            Self::MachineDrain => "machine.drain",
            Self::MachineResume => "machine.resume",
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
            Self::MachineDrain => API_MACHINE_DRAIN,
            Self::MachineResume => API_MACHINE_RESUME,
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
            Self::DeploySubmit
            | Self::MachineAdd
            | Self::MachineUpdate
            | Self::MachineDrain
            | Self::MachineResume => OperationApiEndpointExecution::AcceptsOperation,
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

#[must_use]
pub fn machine_facts(machine_id: &MachineId) -> String {
    format!("plz.v1.facts.machine.{}", machine_id.as_str())
}

#[must_use]
pub fn machine_container_facts(machine_id: &MachineId) -> String {
    format!("plz.v1.facts.machine.{}.containers", machine_id.as_str())
}

#[must_use]
pub fn machine_facts_scope() -> String {
    "plz.v1.facts.machine.>".to_owned()
}

#[must_use]
pub fn gateway_status(machine_id: &MachineId) -> String {
    format!("plz.v1.facts.gateway.{}", machine_id.as_str())
}

#[must_use]
pub fn gateway_status_scope() -> String {
    "plz.v1.facts.gateway.>".to_owned()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineServiceEndpoint {
    Inspect,
    FactsGet,
    ContainerEnsureEndpointNetwork,
    ContainerInspect,
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
            Self::FactsGet => "facts.get",
            Self::ContainerEnsureEndpointNetwork => "container.ensure_endpoint_network",
            Self::ContainerInspect => "container.inspect",
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
