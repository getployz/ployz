//! Concrete NATS subject construction.

use ployz_core::ids::{MachineId, NamespaceId, OperationId};
use ployz_core::operation::{
    DeployRunningStage, NamespaceRemoveRunningStage, NetworkRepairRunningStage,
    ServiceRestartRunningStage, VolumeRemoveRunningStage,
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
pub const INGRESS_ENDPOINT_GET: &str = "plz.v1.rpc.core.query.ingress.endpoint.get";
pub const INGRESS_ENDPOINT_CHANGED: &str = "plz.v1.signal.ingress.endpoint.changed";
pub const PENDING_MACHINE_JOINS_CHANGED: &str = "plz.v1.signal.machine.join.pending";
pub const RUNTIME_SNAPSHOT_STREAM: &str = "plz.v1.projection.runtime.snapshot";
pub const RUNTIME_SNAPSHOT_SEED: &str = "plz.v1.rpc.operator.query.runtime.snapshot.seed";

pub const OPERATOR_DEPLOY_SUBMIT: &str = "plz.v1.rpc.operator.command.deploy.submit";
pub const OPERATOR_BUILD_SUBMIT: &str = "plz.v1.rpc.operator.command.build.submit";
pub const OPERATOR_BUILD_CANCEL: &str = "plz.v1.rpc.operator.command.build.cancel";
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
pub const OPERATOR_NETWORK_STATUS: &str = "plz.v1.rpc.operator.query.network.status";
pub const OPERATOR_NETWORK_RESOLVE: &str = "plz.v1.rpc.operator.query.network.resolve";
pub const OPERATOR_NETWORK_REPAIR: &str = "plz.v1.rpc.operator.command.network.repair";
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
pub const OPERATOR_MACHINE_STORAGE_PREPARE: &str =
    "plz.v1.rpc.operator.command.machine.storage_prepare";
pub const OPERATOR_CORE_REPLACE: &str = "plz.v1.rpc.operator.command.core.replace";
pub const OPERATOR_CORE_REPLACE_REPORT: &str = "plz.v1.rpc.operator.command.core.replace.report";
pub const OPERATOR_CREDENTIAL_ADD: &str = "plz.v1.rpc.operator.command.credential.add";
pub const OPERATOR_CREDENTIAL_LIST: &str = "plz.v1.rpc.operator.query.credential.list";
pub const OPERATOR_CREDENTIAL_REMOVE: &str = "plz.v1.rpc.operator.command.credential.remove";
pub const OPERATOR_INGRESS_CONFIGURE: &str = "plz.v1.rpc.operator.command.ingress.configure";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationApiEndpoint {
    BuildSubmit,
    BuildCancel,
    DeployReserve,
    DeploySubmit,
    InitFirstMachineActivate,
    MachineAdd,
    MachineUpdate,
    MachineStoragePrepare,
    MachineDrain,
    MachineResume,
    MachineList,
    MachineInspect,
    NetworkStatus,
    NetworkResolve,
    NetworkRepair,
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
    CredentialAdd,
    CredentialList,
    CredentialRemove,
    IngressConfigure,
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
            Self::BuildSubmit => "build.submit",
            Self::BuildCancel => "build.cancel",
            Self::DeployReserve => "deploy.reserve",
            Self::DeploySubmit => "deploy.submit",
            Self::InitFirstMachineActivate => "init.first_machine.activate",
            Self::MachineAdd => "machine.add",
            Self::MachineUpdate => "machine.update",
            Self::MachineStoragePrepare => "machine.storage_prepare",
            Self::MachineDrain => "machine.drain",
            Self::MachineResume => "machine.resume",
            Self::MachineList => "machine.list",
            Self::MachineInspect => "machine.inspect",
            Self::NetworkStatus => "network.status",
            Self::NetworkResolve => "network.resolve",
            Self::NetworkRepair => "network.repair",
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
            Self::CredentialAdd => "credential.add",
            Self::CredentialList => "credential.list",
            Self::CredentialRemove => "credential.remove",
            Self::IngressConfigure => "ingress.configure",
        }
    }

    #[must_use]
    pub const fn subject(self) -> &'static str {
        match self {
            Self::BuildSubmit => OPERATOR_BUILD_SUBMIT,
            Self::BuildCancel => OPERATOR_BUILD_CANCEL,
            Self::DeployReserve => OPERATOR_DEPLOY_RESERVE,
            Self::DeploySubmit => OPERATOR_DEPLOY_SUBMIT,
            Self::InitFirstMachineActivate => OPERATOR_INIT_FIRST_MACHINE_ACTIVATE,
            Self::MachineAdd => OPERATOR_MACHINE_ADD,
            Self::MachineUpdate => OPERATOR_MACHINE_UPDATE,
            Self::MachineStoragePrepare => OPERATOR_MACHINE_STORAGE_PREPARE,
            Self::MachineDrain => OPERATOR_MACHINE_DRAIN,
            Self::MachineResume => OPERATOR_MACHINE_RESUME,
            Self::MachineList => OPERATOR_MACHINE_LIST,
            Self::MachineInspect => OPERATOR_MACHINE_INSPECT,
            Self::NetworkStatus => OPERATOR_NETWORK_STATUS,
            Self::NetworkResolve => OPERATOR_NETWORK_RESOLVE,
            Self::NetworkRepair => OPERATOR_NETWORK_REPAIR,
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
            Self::CredentialAdd => OPERATOR_CREDENTIAL_ADD,
            Self::CredentialList => OPERATOR_CREDENTIAL_LIST,
            Self::CredentialRemove => OPERATOR_CREDENTIAL_REMOVE,
            Self::IngressConfigure => OPERATOR_INGRESS_CONFIGURE,
        }
    }

    #[must_use]
    pub const fn execution(self) -> OperationApiEndpointExecution {
        match self {
            Self::BuildSubmit
            | Self::DeploySubmit
            | Self::MachineAdd
            | Self::MachineUpdate
            | Self::MachineStoragePrepare
            | Self::MachineDrain
            | Self::MachineResume
            | Self::NetworkRepair
            | Self::ServiceRestart
            | Self::NamespaceRemove
            | Self::VolumeRemove
            | Self::CoreReplace
            | Self::CredentialAdd
            | Self::CredentialRemove
            | Self::IngressConfigure => OperationApiEndpointExecution::AcceptsOperation,
            Self::BuildCancel
            | Self::DeployReserve
            | Self::InitFirstMachineActivate
            | Self::MachineJoinRedeem
            | Self::MachineJoinReport
            | Self::CoreReplaceReport => OperationApiEndpointExecution::MutatesOperation,
            Self::MachineList
            | Self::MachineInspect
            | Self::NetworkStatus
            | Self::NetworkResolve
            | Self::ServiceList
            | Self::VolumeList
            | Self::ServiceInspect
            | Self::RuntimeSnapshot
            | Self::LogsTail
            | Self::OpsList
            | Self::OpsStatus
            | Self::OpsWatch
            | Self::CredentialList => OperationApiEndpointExecution::Query,
        }
    }
}

impl From<ployz_sdk_types::operation_api::OperationApiEndpoint> for OperationApiEndpoint {
    fn from(endpoint: ployz_sdk_types::operation_api::OperationApiEndpoint) -> Self {
        use ployz_sdk_types::operation_api::OperationApiEndpoint as Core;

        match endpoint {
            Core::BuildSubmit => Self::BuildSubmit,
            Core::BuildCancel => Self::BuildCancel,
            Core::DeployReserve => Self::DeployReserve,
            Core::DeploySubmit => Self::DeploySubmit,
            Core::InitFirstMachineActivate => Self::InitFirstMachineActivate,
            Core::MachineAdd => Self::MachineAdd,
            Core::MachineUpdate => Self::MachineUpdate,
            Core::MachineStoragePrepare => Self::MachineStoragePrepare,
            Core::MachineDrain => Self::MachineDrain,
            Core::MachineResume => Self::MachineResume,
            Core::MachineList => Self::MachineList,
            Core::MachineInspect => Self::MachineInspect,
            Core::NetworkStatus => Self::NetworkStatus,
            Core::NetworkResolve => Self::NetworkResolve,
            Core::NetworkRepair => Self::NetworkRepair,
            Core::MachineJoinRedeem => Self::MachineJoinRedeem,
            Core::MachineJoinReport => Self::MachineJoinReport,
            Core::ServiceList => Self::ServiceList,
            Core::ServiceInspect => Self::ServiceInspect,
            Core::ServiceRestart => Self::ServiceRestart,
            Core::NamespaceRemove => Self::NamespaceRemove,
            Core::VolumeList => Self::VolumeList,
            Core::VolumeRemove => Self::VolumeRemove,
            Core::RuntimeSnapshot => Self::RuntimeSnapshot,
            Core::LogsTail => Self::LogsTail,
            Core::OpsList => Self::OpsList,
            Core::OpsStatus => Self::OpsStatus,
            Core::OpsWatch => Self::OpsWatch,
            Core::CoreReplace => Self::CoreReplace,
            Core::CoreReplaceReport => Self::CoreReplaceReport,
            Core::CredentialAdd => Self::CredentialAdd,
            Core::CredentialList => Self::CredentialList,
            Core::CredentialRemove => Self::CredentialRemove,
            Core::IngressConfigure => Self::IngressConfigure,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationProgressScope {
    Namespace { namespace_id: NamespaceId },
    Machine { machine_id: MachineId },
    Cluster,
}

impl From<ployz_core::operation::OperationProgressScope> for OperationProgressScope {
    fn from(scope: ployz_core::operation::OperationProgressScope) -> Self {
        match scope {
            ployz_core::operation::OperationProgressScope::Namespace { namespace_id } => {
                Self::Namespace { namespace_id }
            }
            ployz_core::operation::OperationProgressScope::Machine { machine_id } => {
                Self::Machine { machine_id }
            }
            ployz_core::operation::OperationProgressScope::Cluster => Self::Cluster,
        }
    }
}

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
        OperationProgressScope::Cluster => format!(
            "plz.v1.progress.cluster.operation.{}.{suffix}",
            operation_id.as_str()
        ),
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

#[must_use]
pub const fn deploy_running_stage(stage: &DeployRunningStage) -> &'static str {
    match stage {
        DeployRunningStage::EnsuringImages => "ensuring_images",
        DeployRunningStage::StartingContainers => "starting_containers",
        DeployRunningStage::WaitingForHealth => "waiting_for_health",
        DeployRunningStage::EnsuringCertificates => "ensuring_certificates",
        DeployRunningStage::RouteCutover => "route_cutover",
        DeployRunningStage::ServingTargetCommit => "serving_target_commit",
        DeployRunningStage::RemovingSupersededContainers => "removing_superseded_containers",
    }
}

#[must_use]
pub const fn service_restart_running_stage(stage: &ServiceRestartRunningStage) -> &'static str {
    match stage {
        ServiceRestartRunningStage::RestartingContainers => "restarting_containers",
        ServiceRestartRunningStage::WaitingForHealth => "waiting_for_health",
    }
}

#[must_use]
pub const fn network_repair_running_stage(stage: &NetworkRepairRunningStage) -> &'static str {
    match stage {
        NetworkRepairRunningStage::AwaitingDataplane => "awaiting_dataplane",
        NetworkRepairRunningStage::RefreshingMachineFacts => "refreshing_machine_facts",
        NetworkRepairRunningStage::ConfirmingDnsRefresh => "confirming_dns_refresh",
    }
}

#[must_use]
pub const fn namespace_remove_running_stage(stage: &NamespaceRemoveRunningStage) -> &'static str {
    match stage {
        NamespaceRemoveRunningStage::RemovingRouteBindings => "removing_route_bindings",
        NamespaceRemoveRunningStage::RemovingServingTargets => "removing_serving_targets",
        NamespaceRemoveRunningStage::RemovingContainers => "removing_containers",
    }
}

#[must_use]
pub const fn volume_remove_running_stage(stage: &VolumeRemoveRunningStage) -> &'static str {
    match stage {
        VolumeRemoveRunningStage::RemovingVolumeData => "removing_volume_data",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineServiceEndpoint {
    Inspect,
    FactsGet,
    FactsRefresh,
    DnsResolve,
    DnsStatus,
    ContainerInspect,
    ContainerResolveImage,
    ContainerRun,
    ContainerRunHook,
    ContainerRestart,
    ContainerStop,
    ContainerRemove,
    VolumeRemove,
    DataplanePublicKey,
    DataplaneStatus,
    SubstrateUpdate,
    SubstrateReport,
    StoragePrepare,
    StoragePrepareReport,
    LogsTail,
    ImageBlobCheck,
    ImageBlobPush,
    ImageManifestPush,
    ImageEnsure,
    ImageRemove,
    CertificateArtifactStatus,
    CertificateArtifactPush,
    CertificateArtifactRemove,
    CertificateChallengeApply,
    CertificateChallengeRemove,
    CertificateChallengeStatus,
    GatewayStatusGet,
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
            Self::FactsRefresh => "facts.refresh",
            Self::DnsResolve => "dns.resolve",
            Self::DnsStatus => "dns.status",
            Self::ContainerInspect => "container.inspect",
            Self::ContainerResolveImage => "container.resolve_image",
            Self::ContainerRun => "container.run",
            Self::ContainerRunHook => "container.run_hook",
            Self::ContainerRestart => "container.restart",
            Self::ContainerStop => "container.stop",
            Self::ContainerRemove => "container.remove",
            Self::VolumeRemove => "volume.remove",
            Self::DataplanePublicKey => "dataplane.public_key",
            Self::DataplaneStatus => "dataplane.status",
            Self::SubstrateUpdate => "substrate.update",
            Self::SubstrateReport => "substrate.report",
            Self::StoragePrepare => "storage.prepare",
            Self::StoragePrepareReport => "storage.prepare.report",
            Self::LogsTail => "logs.tail",
            Self::ImageBlobCheck => "image.blob.check",
            Self::ImageBlobPush => "image.blob.push",
            Self::ImageManifestPush => "image.manifest.push",
            Self::ImageEnsure => "container.ensure_image",
            Self::ImageRemove => "container.remove_image",
            Self::CertificateArtifactStatus => "certificate.artifact.status",
            Self::CertificateArtifactPush => "certificate.artifact.push",
            Self::CertificateArtifactRemove => "certificate.artifact.remove",
            Self::CertificateChallengeApply => "certificate.challenge.apply",
            Self::CertificateChallengeRemove => "certificate.challenge.remove",
            Self::CertificateChallengeStatus => "certificate.challenge.status",
            Self::GatewayStatusGet => "gateway.status.get",
        }
    }

    #[must_use]
    pub const fn execution(self) -> MachineServiceEndpointExecution {
        match self {
            Self::Inspect
            | Self::FactsGet
            | Self::DnsResolve
            | Self::DnsStatus
            | Self::ContainerInspect
            | Self::ContainerResolveImage
            | Self::DataplanePublicKey
            | Self::SubstrateReport
            | Self::StoragePrepareReport
            | Self::DataplaneStatus
            | Self::LogsTail
            | Self::ImageBlobCheck
            | Self::CertificateArtifactStatus
            | Self::CertificateChallengeStatus
            | Self::GatewayStatusGet => MachineServiceEndpointExecution::Query,
            Self::FactsRefresh
            | Self::ContainerRun
            | Self::ContainerRunHook
            | Self::ContainerRestart
            | Self::ContainerStop
            | Self::ContainerRemove
            | Self::VolumeRemove
            | Self::SubstrateUpdate
            | Self::StoragePrepare
            | Self::ImageBlobPush
            | Self::ImageManifestPush
            | Self::ImageEnsure
            | Self::ImageRemove
            | Self::CertificateArtifactPush
            | Self::CertificateArtifactRemove
            | Self::CertificateChallengeApply
            | Self::CertificateChallengeRemove => MachineServiceEndpointExecution::Command,
        }
    }
}

#[cfg(test)]
mod build_contract_tests {
    use super::*;

    #[test]
    fn build_endpoint_metadata_is_stable() {
        assert_eq!(OperationApiEndpoint::BuildSubmit.name(), "build.submit");
        assert_eq!(
            OperationApiEndpoint::BuildSubmit.subject(),
            "plz.v1.rpc.operator.command.build.submit"
        );
        assert_eq!(
            OperationApiEndpoint::BuildSubmit.execution(),
            OperationApiEndpointExecution::AcceptsOperation
        );
        assert_eq!(OperationApiEndpoint::BuildCancel.name(), "build.cancel");
        assert_eq!(
            OperationApiEndpoint::BuildCancel.subject(),
            "plz.v1.rpc.operator.command.build.cancel"
        );
        assert_eq!(
            OperationApiEndpoint::BuildCancel.execution(),
            OperationApiEndpointExecution::MutatesOperation
        );
    }

    #[test]
    fn sdk_registry_maps_to_build_endpoints() {
        assert_eq!(
            OperationApiEndpoint::from(
                ployz_sdk_types::operation_api::OperationApiEndpoint::BuildSubmit
            ),
            OperationApiEndpoint::BuildSubmit
        );
        assert_eq!(
            OperationApiEndpoint::from(
                ployz_sdk_types::operation_api::OperationApiEndpoint::BuildCancel
            ),
            OperationApiEndpoint::BuildCancel
        );
    }
}
