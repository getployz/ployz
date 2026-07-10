//! NATS subject construction helpers.

use crate::ids::{MachineId, NamespaceId, OperationId};
use crate::ops::{
    DeployRunningStage, NamespaceRemoveRunningStage, ServiceRestartRunningStage,
    VolumeRemoveRunningStage,
};

pub const OPERATION_PROGRESS_SCOPE: &str = "plz.v1.progress.>";

pub const OPERATOR_RPC_QUERY_SCOPE: &str = "plz.v1.rpc.operator.query.>";
pub const OPERATOR_RPC_COMMAND_SCOPE: &str = "plz.v1.rpc.operator.command.>";
pub const JOIN_RPC_COMMAND_SCOPE: &str = "plz.v1.rpc.join.command.>";
pub const CORE_RPC_QUERY_SCOPE: &str = "plz.v1.rpc.core.query.>";
pub const MACHINE_RPC_QUERY_SCOPE: &str = "plz.v1.rpc.machine.query.>";
pub const MACHINE_RPC_COMMAND_SCOPE: &str = "plz.v1.rpc.machine.command.>";
pub const OPERATOR_MACHINE_IMAGE_QUERY_SCOPE: &str = "plz.v1.rpc.machine.query.*.image.>";
pub const OPERATOR_MACHINE_IMAGE_COMMAND_SCOPE: &str = "plz.v1.rpc.machine.command.*.image.>";
pub const INTENT_GET: &str = "plz.v1.rpc.core.query.intent.get";
pub const INTENT_CHANGED: &str = "plz.v1.signal.intent.changed";
pub const PENDING_MACHINE_JOINS_CHANGED: &str = "plz.v1.signal.machine.join.pending";

pub const OPERATOR_DEPLOY_SUBMIT: &str = "plz.v1.rpc.operator.command.deploy.submit";
pub const OPERATOR_DEPLOY_RESERVE: &str = "plz.v1.rpc.operator.command.deploy.reserve";
pub const OPERATOR_OPS_LIST: &str = "plz.v1.rpc.operator.query.ops.list";
pub const OPERATOR_OPS_STATUS: &str = "plz.v1.rpc.operator.query.ops.status";
pub const OPERATOR_OPS_WATCH: &str = "plz.v1.rpc.operator.query.ops.watch";
pub const OPERATOR_INIT_FIRST_MACHINE_ACTIVATE: &str =
    "plz.v1.rpc.operator.command.init.first_machine.activate";
pub const OPERATOR_MACHINE_ADD: &str = "plz.v1.rpc.operator.command.machine.add";
pub const OPERATOR_MACHINE_UPDATE: &str = "plz.v1.rpc.operator.command.machine.update";
pub const OPERATOR_MACHINE_LIST: &str = "plz.v1.rpc.operator.query.machine.list";
pub const OPERATOR_MACHINE_INSPECT: &str = "plz.v1.rpc.operator.query.machine.inspect";
pub const JOIN_MACHINE_REDEEM: &str = "plz.v1.rpc.join.command.machine.redeem";
pub const JOIN_MACHINE_REPORT: &str = "plz.v1.rpc.join.command.machine.report";
pub const OPERATOR_SERVICE_LIST: &str = "plz.v1.rpc.operator.query.service.list";
pub const OPERATOR_SERVICE_INSPECT: &str = "plz.v1.rpc.operator.query.service.inspect";
pub const OPERATOR_SERVICE_RESTART: &str = "plz.v1.rpc.operator.command.service.restart";
pub const OPERATOR_NAMESPACE_REMOVE: &str = "plz.v1.rpc.operator.command.namespace.remove";
pub const OPERATOR_VOLUME_LIST: &str = "plz.v1.rpc.operator.query.volume.list";
pub const OPERATOR_VOLUME_REMOVE: &str = "plz.v1.rpc.operator.command.volume.remove";
pub const OPERATOR_RUNTIME_SNAPSHOT: &str = "plz.v1.rpc.operator.query.runtime.snapshot";
pub const OPERATOR_LOGS_TAIL: &str = "plz.v1.rpc.operator.query.logs.tail";
pub const OPERATOR_MACHINE_DRAIN: &str = "plz.v1.rpc.operator.command.machine.drain";
pub const OPERATOR_MACHINE_RESUME: &str = "plz.v1.rpc.operator.command.machine.resume";
pub const OPERATOR_CORE_REPLACE: &str = "plz.v1.rpc.operator.command.core.replace";
pub const OPERATOR_CORE_REPLACE_REPORT: &str = "plz.v1.rpc.operator.command.core.replace.report";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationApiEndpoint {
    DeployReserve,
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
    ServiceRestart,
    NamespaceRemove,
    VolumeList,
    VolumeRemove,
    RuntimeSnapshot,
    LogsTail,
    OpsList,
    OpsStatus,
    OpsWatch,
    CoreReplace,
    CoreReplaceReport,
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
            Self::DeployReserve => "deploy.reserve",
            Self::DeploySubmit => "deploy.submit",
            Self::InitFirstMachineActivate => "init.first_machine.activate",
            Self::MachineAdd => "machine.add",
            Self::MachineUpdate => "machine.update",
            Self::MachineDrain => "machine.drain",
            Self::MachineResume => "machine.resume",
            Self::MachineList => "machine.list",
            Self::MachineInspect => "machine.inspect",
            Self::MachineJoinRedeem => "machine.redeem",
            Self::MachineJoinReport => "machine.report",
            Self::ServiceList => "service.list",
            Self::ServiceInspect => "service.inspect",
            Self::ServiceRestart => "service.restart",
            Self::NamespaceRemove => "namespace.remove",
            Self::VolumeList => "volume.list",
            Self::VolumeRemove => "volume.remove",
            Self::RuntimeSnapshot => "runtime.snapshot",
            Self::LogsTail => "logs.tail",
            Self::OpsList => "ops.list",
            Self::OpsStatus => "ops.status",
            Self::OpsWatch => "ops.watch",
            Self::CoreReplace => "core.replace",
            Self::CoreReplaceReport => "core.replace.report",
        }
    }

    #[must_use]
    pub const fn subject(self) -> &'static str {
        match self {
            Self::DeployReserve => OPERATOR_DEPLOY_RESERVE,
            Self::DeploySubmit => OPERATOR_DEPLOY_SUBMIT,
            Self::InitFirstMachineActivate => OPERATOR_INIT_FIRST_MACHINE_ACTIVATE,
            Self::MachineAdd => OPERATOR_MACHINE_ADD,
            Self::MachineUpdate => OPERATOR_MACHINE_UPDATE,
            Self::MachineDrain => OPERATOR_MACHINE_DRAIN,
            Self::MachineResume => OPERATOR_MACHINE_RESUME,
            Self::MachineList => OPERATOR_MACHINE_LIST,
            Self::MachineInspect => OPERATOR_MACHINE_INSPECT,
            Self::MachineJoinRedeem => JOIN_MACHINE_REDEEM,
            Self::MachineJoinReport => JOIN_MACHINE_REPORT,
            Self::ServiceList => OPERATOR_SERVICE_LIST,
            Self::ServiceInspect => OPERATOR_SERVICE_INSPECT,
            Self::ServiceRestart => OPERATOR_SERVICE_RESTART,
            Self::NamespaceRemove => OPERATOR_NAMESPACE_REMOVE,
            Self::VolumeList => OPERATOR_VOLUME_LIST,
            Self::VolumeRemove => OPERATOR_VOLUME_REMOVE,
            Self::RuntimeSnapshot => OPERATOR_RUNTIME_SNAPSHOT,
            Self::LogsTail => OPERATOR_LOGS_TAIL,
            Self::OpsList => OPERATOR_OPS_LIST,
            Self::OpsStatus => OPERATOR_OPS_STATUS,
            Self::OpsWatch => OPERATOR_OPS_WATCH,
            Self::CoreReplace => OPERATOR_CORE_REPLACE,
            Self::CoreReplaceReport => OPERATOR_CORE_REPLACE_REPORT,
        }
    }

    #[must_use]
    pub const fn execution(self) -> OperationApiEndpointExecution {
        match self {
            Self::DeploySubmit
            | Self::MachineAdd
            | Self::MachineUpdate
            | Self::MachineDrain
            | Self::MachineResume
            | Self::ServiceRestart
            | Self::NamespaceRemove
            | Self::VolumeRemove
            | Self::CoreReplace => OperationApiEndpointExecution::AcceptsOperation,
            Self::DeployReserve
            | Self::InitFirstMachineActivate
            | Self::MachineJoinRedeem
            | Self::MachineJoinReport
            | Self::CoreReplaceReport => OperationApiEndpointExecution::MutatesOperation,
            Self::MachineList
            | Self::MachineInspect
            | Self::ServiceList
            | Self::VolumeList
            | Self::ServiceInspect
            | Self::RuntimeSnapshot
            | Self::LogsTail
            | Self::OpsList
            | Self::OpsStatus
            | Self::OpsWatch => OperationApiEndpointExecution::Query,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationProgressScope {
    Namespace { namespace_id: NamespaceId },
    Machine { machine_id: MachineId },
    Cluster,
}

/// One operation's event subject for a suffix rendered by an operation event.
/// The prefix here and the [`operation_progress_watch`] pattern must agree.
#[must_use]
pub fn operation_progress_subject(
    scope: &OperationProgressScope,
    operation_id: &OperationId,
    suffix: &str,
) -> String {
    match scope {
        OperationProgressScope::Namespace { namespace_id } => format!(
            "plz.v1.progress.namespace.{}.operation.{}.{suffix}",
            namespace_id.as_str(),
            operation_id.as_str()
        ),
        OperationProgressScope::Machine { machine_id } => format!(
            "plz.v1.progress.machine.{}.operation.{}.{suffix}",
            machine_id.as_str(),
            operation_id.as_str()
        ),
        OperationProgressScope::Cluster => {
            format!(
                "plz.v1.progress.cluster.operation.{}.{suffix}",
                operation_id.as_str()
            )
        }
    }
}

#[must_use]
pub fn operation_progress_watch(
    scope: &OperationProgressScope,
    operation_id: &OperationId,
) -> String {
    operation_progress_subject(scope, operation_id, ">")
}

#[must_use]
pub fn machine_service(machine_id: &MachineId, endpoint: MachineServiceEndpoint) -> String {
    let class = match endpoint.execution() {
        MachineServiceEndpointExecution::Query => "query",
        MachineServiceEndpointExecution::Command => "command",
    };
    format!(
        "plz.v1.rpc.machine.{class}.{}.{}",
        machine_id.as_str(),
        endpoint.as_subject()
    )
}

#[must_use]
pub fn machine_service_query_scope(machine_id: &MachineId) -> String {
    format!("plz.v1.rpc.machine.query.{}.>", machine_id.as_str())
}

#[must_use]
pub fn machine_service_command_scope(machine_id: &MachineId) -> String {
    format!("plz.v1.rpc.machine.command.{}.>", machine_id.as_str())
}

#[must_use]
pub fn machine_facts(machine_id: &MachineId) -> String {
    format!("plz.v1.testimony.machine.{}.snapshot", machine_id.as_str())
}

#[must_use]
pub fn machine_container_facts(machine_id: &MachineId) -> String {
    format!(
        "plz.v1.testimony.machine.{}.containers",
        machine_id.as_str()
    )
}

#[must_use]
pub fn machine_facts_scope() -> String {
    "plz.v1.testimony.machine.>".to_owned()
}

#[must_use]
pub fn gateway_status(machine_id: &MachineId) -> String {
    format!("plz.v1.testimony.gateway.{}.status", machine_id.as_str())
}

#[must_use]
pub fn gateway_status_scope() -> String {
    "plz.v1.testimony.gateway.>".to_owned()
}

impl DeployRunningStage {
    #[must_use]
    pub const fn as_subject(&self) -> &'static str {
        match self {
            Self::PreparingDataplane => "preparing_dataplane",
            Self::EnsuringImages => "ensuring_images",
            Self::StartingContainers => "starting_containers",
            Self::WaitingForHealth => "waiting_for_health",
            Self::RouteCutover => "route_cutover",
            Self::ServingTargetCommit => "serving_target_commit",
            Self::RemovingSupersededContainers => "removing_superseded_containers",
        }
    }
}

impl ServiceRestartRunningStage {
    #[must_use]
    pub const fn as_subject(&self) -> &'static str {
        match self {
            Self::RestartingContainers => "restarting_containers",
            Self::WaitingForHealth => "waiting_for_health",
        }
    }
}

impl NamespaceRemoveRunningStage {
    #[must_use]
    pub const fn as_subject(&self) -> &'static str {
        match self {
            Self::RemovingRouteBindings => "removing_route_bindings",
            Self::RemovingServingTargets => "removing_serving_targets",
            Self::RemovingContainers => "removing_containers",
        }
    }
}

impl VolumeRemoveRunningStage {
    #[must_use]
    pub const fn as_subject(&self) -> &'static str {
        match self {
            Self::RemovingVolumeData => "removing_volume_data",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineServiceEndpoint {
    Inspect,
    FactsGet,
    ContainerEnsureEndpointNetwork,
    ContainerInspect,
    ContainerResolveImage,
    ContainerRun,
    ContainerRunHook,
    ContainerRestart,
    ContainerStop,
    ContainerRemove,
    VolumeRemove,
    DataplanePrepare,
    DataplaneProbeMtu,
    SubstrateUpdate,
    SubstrateReport,
    LogsTail,
    ImageBlobCheck,
    ImageBlobPush,
    ImageManifestPush,
    ImageEnsure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineServiceEndpointExecution {
    Query,
    Command,
}

impl MachineServiceEndpoint {
    #[must_use]
    pub const fn as_subject(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::FactsGet => "facts.get",
            Self::ContainerEnsureEndpointNetwork => "container.ensure_endpoint_network",
            Self::ContainerInspect => "container.inspect",
            Self::ContainerResolveImage => "container.resolve_image",
            Self::ContainerRun => "container.run",
            Self::ContainerRunHook => "container.run_hook",
            Self::ContainerRestart => "container.restart",
            Self::ContainerStop => "container.stop",
            Self::ContainerRemove => "container.remove",
            Self::VolumeRemove => "volume.remove",
            Self::DataplanePrepare => "dataplane.prepare",
            Self::DataplaneProbeMtu => "dataplane.probe_mtu",
            Self::SubstrateUpdate => "substrate.update",
            Self::SubstrateReport => "substrate.report",
            Self::LogsTail => "logs.tail",
            Self::ImageBlobCheck => "image.blob.check",
            Self::ImageBlobPush => "image.blob.push",
            Self::ImageManifestPush => "image.manifest.push",
            // Deliberately outside the operator-granted `*.image.>` scopes:
            // ensure mutates the seed's Docker store (self-pull), so it lives
            // under the controller-only container command namespace while the
            // three staging endpoints above stay operator-reachable.
            Self::ImageEnsure => "container.ensure_image",
        }
    }

    #[must_use]
    pub const fn execution(self) -> MachineServiceEndpointExecution {
        match self {
            Self::Inspect
            | Self::FactsGet
            | Self::ContainerInspect
            | Self::ContainerResolveImage
            | Self::DataplaneProbeMtu
            | Self::SubstrateReport
            | Self::LogsTail
            | Self::ImageBlobCheck => MachineServiceEndpointExecution::Query,
            Self::ContainerEnsureEndpointNetwork
            | Self::ContainerRun
            | Self::ContainerRunHook
            | Self::ContainerRestart
            | Self::ContainerStop
            | Self::ContainerRemove
            | Self::VolumeRemove
            | Self::DataplanePrepare
            | Self::SubstrateUpdate
            | Self::ImageBlobPush
            | Self::ImageManifestPush
            | Self::ImageEnsure => MachineServiceEndpointExecution::Command,
        }
    }
}
