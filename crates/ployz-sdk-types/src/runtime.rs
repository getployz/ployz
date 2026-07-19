use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core_types::*;
use crate::machine::MachineSnapshot;
use crate::ops::OperationApiResponse;
use crate::service::ServiceSnapshot;

pub type RuntimeSnapshotResponse =
    OperationApiResponse<RuntimeSnapshotResult, RuntimeSnapshotError>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSnapshotRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSnapshotResult {
    pub snapshot: RuntimeSnapshot,
    #[serde(default)]
    pub control_health: Option<ControlHealth>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ControlHealth {
    pub nats_authorization: ControlNatsAuthorizationHealth,
    pub task_supervisor: ControlTaskSupervisorHealth,
    pub runtime_projection: ControlRuntimeProjectionHealth,
    pub ingress_endpoint_projection: ControlIngressEndpointProjectionHealth,
    pub certificate_renewal: ControlCertificateRenewalHealth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ControlNatsAuthorizationHealth {
    pub consecutive_failures: u64,
    pub last_failure: Option<String>,
    pub next_expiry_at_unix_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ControlTaskSupervisorHealth {
    pub active_tasks: usize,
    pub panicked_tasks: u64,
    pub last_failure: Option<ControlTaskSupervisorFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ControlTaskSupervisorFailure {
    pub task_id: u64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ControlIngressEndpointProjectionHealth {
    pub consecutive_failures: u64,
    pub last_failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ControlRuntimeProjectionHealth {
    pub projection: ControlRuntimeProjectionLoopHealth,
    pub publisher: ControlRuntimeProjectionLoopHealth,
    pub seed_service: ControlRuntimeProjectionServiceHealth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ControlRuntimeProjectionLoopHealth {
    pub running: bool,
    pub consecutive_failures: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ControlRuntimeProjectionServiceHealth {
    pub endpoint_tasks_started: usize,
    pub endpoint_tasks_finished: usize,
    pub request_timeouts: usize,
    pub handler_failures: usize,
    pub domain_failures: usize,
    pub response_failures: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ControlCertificateRenewalHealth {
    pub last_attempt: Option<ControlCertificateRenewalAttempt>,
    pub consecutive_failures: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlCertificateRenewalAttempt {
    Completed {
        outcome: ControlCertificateRenewalOutcome,
    },
    Failed {
        failure: ControlCertificateRenewalFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "failure_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlCertificateRenewalFailure {
    IntentStore { message: String },
    MachineRoster { message: String },
    OperationStatus { message: String },
    OperationEvidence { message: String },
    PloyzDnsTarget { message: String },
    Worker { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlCertificateRenewalOutcome {
    NoAction,
    AwaitingPloyzWildcard,
    Attempted {
        attempted: usize,
        failed: usize,
    },
    Degraded {
        attempted: usize,
        failed: usize,
        unknown_machine_ids: Vec<MachineId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSnapshot {
    pub automatic_hostname_configuration: AutomaticHostnameConfiguration,
    pub ployz_dns_target: RuntimePloyzDnsTarget,
    pub ingress_endpoint_projection: IngressEndpointProjection,
    pub active_certificates: Vec<ActiveCertificateMetadata>,
    pub route_tls: Vec<RouteTlsStatus>,
    pub machines: Vec<MachineSnapshot>,
    pub services: Vec<ServiceSnapshot>,
    pub routes: Vec<RouteBindingState>,
    pub containers: Vec<ManagedContainerObservation>,
    pub revisions: Vec<RuntimeServiceRevision>,
    pub releases: Vec<RuntimeServiceRelease>,
    pub instances: Vec<RuntimeServiceInstance>,
    pub projection_sources: RuntimeProjectionSources,
    pub updated_at_unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimePloyzDnsTarget {
    pub intent: PloyzDnsTargetIntent,
    pub allocation: RuntimePloyzDnsTargetAllocation,
    pub publication: RuntimePloyzDnsTargetPublication,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimePloyzDnsTargetAllocation {
    Unacquired,
    Allocated {
        hostname: RouteHostname,
        issued_at_unix_seconds: u64,
        expires_at_unix_seconds: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimePloyzDnsTargetPublication {
    Unpublished,
    Applied {
        ingress_projection: Option<IngressEndpointProjectionIdentity>,
    },
    Withdrawn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RouteTlsStatus {
    pub route_binding_id: RouteBindingId,
    pub availability: RouteTlsAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteTlsAvailability {
    Available { certificate_id: CertId },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeServiceRevision {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeServiceRelease {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub routes: Vec<RouteTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeServiceInstance {
    pub namespace_id: NamespaceId,
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub operation_id: OperationId,
    pub step_id: StepId,
    pub state: ContainerRuntimeState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProjectionSources {
    pub intent: RuntimeProjectionSource,
    pub facts: RuntimeProjectionSource,
    pub revisions: RuntimeDerivedCollectionSource,
    pub releases: RuntimeDerivedCollectionSource,
    pub instances: RuntimeDerivedCollectionSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProjectionSource {
    pub read_at_unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDerivedCollectionSource {
    pub status: RuntimeDerivedCollectionStatus,
    pub source_count: usize,
    pub missing_link_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDerivedCollectionStatus {
    Complete,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum RuntimeSnapshotError {
    #[error("runtime snapshot unavailable: {message}")]
    Unavailable { message: String },
}
