use clap::Args;
use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::ops::MachineAddOperationState;
use ployz_core::ops::{
    CertOperationState, CertRunningStage, ControlPlaneCommitScope, DeployOperationFailure,
    DeployOperationState, DeployRunningStage, EventSequence, HealthCheckFailure,
    MAX_OPERATION_EVENT_REPLAY_LIMIT, MachineUpdateOperationState, OperationEvent,
    OperationEventReplayLimit, OperationEventReplayRequest, OperationKind, OperationStatus,
    OperationStatusSnapshot, ReplayedOperationEvent, RetainedArtifact, RouteCutoverFailureReason,
};
use ployz_core::roles::{DnsRole, GatewayRole};
use ployz_sdk_types::{OpsListRequest, OpsListResult, OpsStatusRequest};

use crate::commands::PloyzctlCliError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsStatusCommand {
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsListCommand {
    pub active_only: bool,
}

impl OpsListCommand {
    #[must_use]
    pub const fn into_request(self) -> OpsListRequest {
        OpsListRequest {
            active_only: self.active_only,
        }
    }
}

impl OpsStatusCommand {
    #[must_use]
    pub fn into_request(self) -> OpsStatusRequest {
        OpsStatusRequest {
            operation_id: self.operation_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsWatchCommand {
    pub operation_id: OperationId,
    pub output: OpsWatchOutput,
}

impl OpsWatchCommand {
    #[must_use]
    pub fn into_request(self) -> OperationEventReplayRequest {
        OperationEventReplayRequest {
            operation_id: self.operation_id,
            start_sequence: EventSequence::first(),
            limit: OperationEventReplayLimit::try_new(MAX_OPERATION_EVENT_REPLAY_LIMIT)
                .expect("max replay limit is valid"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpsWatchOutput {
    Text,
    Json,
}

pub(crate) fn ops_status_command(
    parsed: OpsStatusCli,
) -> Result<OpsStatusCommand, PloyzctlCliError> {
    let operation_id = parse_operation_id(&parsed.operation_id)?;

    Ok(OpsStatusCommand { operation_id })
}

pub(crate) const fn ops_list_command(parsed: OpsListCli) -> OpsListCommand {
    OpsListCommand {
        active_only: parsed.active,
    }
}

pub(crate) fn ops_watch_command(parsed: OpsWatchCli) -> Result<OpsWatchCommand, PloyzctlCliError> {
    let operation_id = parse_operation_id(&parsed.operation_id)?;

    Ok(OpsWatchCommand {
        operation_id,
        output: if parsed.json {
            OpsWatchOutput::Json
        } else {
            OpsWatchOutput::Text
        },
    })
}

#[derive(Debug, Args)]
pub(crate) struct OpsStatusCli {
    operation_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct OpsListCli {
    #[arg(long)]
    active: bool,
}

#[derive(Debug, Args)]
pub(crate) struct OpsWatchCli {
    operation_id: String,
    #[arg(long)]
    json: bool,
}

fn parse_operation_id(operation_id: &str) -> Result<OperationId, PloyzctlCliError> {
    OperationId::try_new(operation_id.to_owned()).map_err(|error| PloyzctlCliError::InvalidValue {
        flag: "<operation_id>",
        message: error.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusOutput {
    pub snapshot: OperationStatusSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListOutput {
    pub result: OpsListResult,
}

impl ListOutput {
    #[must_use]
    pub const fn from_result(result: OpsListResult) -> Self {
        Self { result }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let rendered = self
            .result
            .operations
            .iter()
            .map(|operation| {
                format!(
                    "{} {} {} {}",
                    operation.status.id().as_str(),
                    operation_kind_name(operation.status.kind()),
                    operation_subject(&operation.status),
                    operation_state(&operation.status),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if rendered.is_empty() {
            rendered
        } else {
            rendered + "\n"
        }
    }
}

impl StatusOutput {
    #[must_use]
    pub fn new(snapshot: OperationStatusSnapshot) -> Self {
        Self { snapshot }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let failure_detail = status_failure_detail(&self.snapshot.status)
            .map(|detail| format!("{detail}\n"))
            .unwrap_or_default();

        format!(
            "operation {}\nkind {}\n{}\nstate {}\n{}last-event {}\n",
            self.snapshot.status.id().as_str(),
            operation_kind_name(self.snapshot.status.kind()),
            operation_subject(&self.snapshot.status),
            operation_state(&self.snapshot.status),
            failure_detail,
            self.snapshot.status.last_event_sequence().get(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchOutput {
    pub events: Vec<ReplayedOperationEvent>,
    pub output: OpsWatchOutput,
}

impl WatchOutput {
    #[must_use]
    pub fn render(&self) -> String {
        let rendered = match self.output {
            OpsWatchOutput::Text => self
                .events
                .iter()
                .scan(
                    DeployEventRenderContext { service_id: None },
                    |context, event| {
                        let rendered = render_replayed_event_text(event, context);
                        context.observe(&event.event);
                        Some(rendered)
                    },
                )
                .collect::<Vec<_>>()
                .join("\n"),
            OpsWatchOutput::Json => self
                .events
                .iter()
                .map(render_replayed_event_json)
                .collect::<Vec<_>>()
                .join("\n"),
        };

        if rendered.is_empty() {
            rendered
        } else {
            rendered + "\n"
        }
    }
}

/// CLI display names for operation kinds; identity and sequence come from
/// the core accessors on [`OperationStatus`].
const fn operation_kind_name(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Deploy => "deploy",
        OperationKind::Cert => "cert",
        OperationKind::MachineAdd => "machine-add",
        OperationKind::MachineUpdate => "machine-update",
        OperationKind::MachineLifecycle => "machine-lifecycle",
        OperationKind::CoreReplace => "core-replace",
        OperationKind::NetworkRepair => "network-repair",
        OperationKind::ServiceRestart => "service-restart",
        OperationKind::NamespaceRemove => "namespace-remove",
    }
}

fn operation_subject(status: &OperationStatus) -> String {
    match status {
        OperationStatus::Deploy { service_id, .. } => {
            format!("service {}", service_id.as_str())
        }
        OperationStatus::Cert { cert_id, .. } => {
            format!("cert {}", cert_id.as_str())
        }
        OperationStatus::MachineAdd {
            machine_id,
            name,
            roles,
            ..
        } => format!(
            "machine {} name {} gateway {} dns {}",
            machine_id.as_str(),
            name.as_str(),
            gateway_role(roles.gateway),
            dns_role(roles.dns)
        ),
        OperationStatus::MachineUpdate {
            machine_id,
            target_version,
            ..
        } => format!(
            "machine {} target-version {}",
            machine_id.as_str(),
            target_version.as_str()
        ),
        OperationStatus::MachineLifecycle {
            machine_id, target, ..
        } => format!(
            "machine {} target-lifecycle {}",
            machine_id.as_str(),
            machine_lifecycle_name(*target)
        ),
        OperationStatus::CoreReplace {
            machine_id,
            successor_nats_url,
            ..
        } => format!(
            "machine {} successor {}",
            machine_id.as_str(),
            successor_nats_url.as_str()
        ),
        OperationStatus::NetworkRepair { .. } => "cluster".to_owned(),
        OperationStatus::ServiceRestart { service_id, .. } => {
            format!("service {}", service_id.as_str())
        }
        OperationStatus::NamespaceRemove { namespace_id, .. } => {
            format!("namespace {}", namespace_id.as_str())
        }
    }
}

const fn machine_lifecycle_name(lifecycle: ployz_sdk_types::MachineLifecycle) -> &'static str {
    match lifecycle {
        ployz_sdk_types::MachineLifecycle::Active => "active",
        ployz_sdk_types::MachineLifecycle::Draining => "draining",
    }
}

const fn machine_lifecycle_state(
    state: &ployz_sdk_types::MachineLifecycleOperationState,
) -> &'static str {
    match state {
        ployz_sdk_types::MachineLifecycleOperationState::Accepted => "accepted",
        ployz_sdk_types::MachineLifecycleOperationState::Completed => "completed",
        ployz_sdk_types::MachineLifecycleOperationState::Failed { .. } => "failed",
        ployz_sdk_types::MachineLifecycleOperationState::Cancelled { .. } => "cancelled",
    }
}

fn operation_state(status: &OperationStatus) -> String {
    match status {
        OperationStatus::Deploy { state, .. } => deploy_state(state).to_owned(),
        OperationStatus::Cert { state, .. } => cert_state(state).to_owned(),
        OperationStatus::MachineAdd { state, .. } => machine_add_state(state).to_owned(),
        OperationStatus::MachineUpdate { state, .. } => machine_update_state(state).to_owned(),
        OperationStatus::MachineLifecycle { state, .. } => {
            machine_lifecycle_state(state).to_owned()
        }
        OperationStatus::CoreReplace { state, .. } => core_replace_state(state).to_owned(),
        OperationStatus::NetworkRepair { state, .. } => network_repair_state(state).to_owned(),
        OperationStatus::ServiceRestart { state, .. } => service_restart_state(state).to_owned(),
        OperationStatus::NamespaceRemove { state, .. } => namespace_remove_state(state).to_owned(),
    }
}

const fn network_repair_state(
    state: &ployz_sdk_types::NetworkRepairOperationState,
) -> &'static str {
    match state {
        ployz_sdk_types::NetworkRepairOperationState::Accepted => "accepted",
        ployz_sdk_types::NetworkRepairOperationState::Running { stage } => match stage {
            ployz_sdk_types::NetworkRepairRunningStage::PreparingDataplane => {
                "running:preparing-dataplane"
            }
        },
        ployz_sdk_types::NetworkRepairOperationState::Completed => "completed",
        ployz_sdk_types::NetworkRepairOperationState::Failed { .. } => "failed",
        ployz_sdk_types::NetworkRepairOperationState::Cancelled { .. } => "cancelled",
    }
}

const fn service_restart_state(
    state: &ployz_sdk_types::ServiceRestartOperationState,
) -> &'static str {
    match state {
        ployz_sdk_types::ServiceRestartOperationState::Accepted => "accepted",
        ployz_sdk_types::ServiceRestartOperationState::Running { stage } => {
            service_restart_running_stage(*stage)
        }
        ployz_sdk_types::ServiceRestartOperationState::Completed => "completed",
        ployz_sdk_types::ServiceRestartOperationState::Failed { .. } => "failed",
        ployz_sdk_types::ServiceRestartOperationState::Cancelled { .. } => "cancelled",
    }
}

const fn service_restart_running_stage(
    stage: ployz_sdk_types::ServiceRestartRunningStage,
) -> &'static str {
    match stage {
        ployz_sdk_types::ServiceRestartRunningStage::RestartingContainers => {
            "running:restarting-containers"
        }
        ployz_sdk_types::ServiceRestartRunningStage::WaitingForHealth => {
            "running:waiting-for-health"
        }
    }
}

const fn namespace_remove_state(
    state: &ployz_sdk_types::NamespaceRemoveOperationState,
) -> &'static str {
    match state {
        ployz_sdk_types::NamespaceRemoveOperationState::Accepted => "accepted",
        ployz_sdk_types::NamespaceRemoveOperationState::Running { stage } => {
            namespace_remove_running_stage(*stage)
        }
        ployz_sdk_types::NamespaceRemoveOperationState::Completed => "completed",
        ployz_sdk_types::NamespaceRemoveOperationState::Failed { .. } => "failed",
        ployz_sdk_types::NamespaceRemoveOperationState::Cancelled { .. } => "cancelled",
    }
}

const fn namespace_remove_running_stage(
    stage: ployz_sdk_types::NamespaceRemoveRunningStage,
) -> &'static str {
    match stage {
        ployz_sdk_types::NamespaceRemoveRunningStage::RemovingRouteBindings => {
            "running:removing-route-bindings"
        }
        ployz_sdk_types::NamespaceRemoveRunningStage::RemovingServingTargets => {
            "running:removing-serving-targets"
        }
        ployz_sdk_types::NamespaceRemoveRunningStage::RemovingContainers => {
            "running:removing-containers"
        }
    }
}

fn status_failure_detail(status: &OperationStatus) -> Option<String> {
    match status {
        OperationStatus::Deploy {
            service_id,
            state: DeployOperationState::Failed { failure },
            ..
        } => Some(format!(
            "failure {}",
            render_deploy_failure_detail(failure, Some(service_id))
        )),
        OperationStatus::NetworkRepair {
            state: ployz_sdk_types::NetworkRepairOperationState::Failed { failure },
            ..
        } => Some(format!(
            "failure {}",
            render_network_repair_failure(failure)
        )),
        OperationStatus::Deploy { .. }
        | OperationStatus::Cert { .. }
        | OperationStatus::MachineAdd { .. }
        | OperationStatus::MachineUpdate { .. }
        | OperationStatus::MachineLifecycle { .. }
        | OperationStatus::CoreReplace { .. }
        | OperationStatus::NetworkRepair { .. }
        | OperationStatus::ServiceRestart { .. }
        | OperationStatus::NamespaceRemove { .. } => None,
    }
}

const fn core_replace_state(state: &ployz_sdk_types::CoreReplaceOperationState) -> &'static str {
    match state {
        ployz_sdk_types::CoreReplaceOperationState::Accepted => "accepted",
        ployz_sdk_types::CoreReplaceOperationState::Completed => "completed",
        ployz_sdk_types::CoreReplaceOperationState::Failed { .. } => "failed",
        ployz_sdk_types::CoreReplaceOperationState::Cancelled { .. } => "cancelled",
    }
}

const fn gateway_role(gateway: GatewayRole) -> &'static str {
    match gateway {
        GatewayRole::Install => "install",
        GatewayRole::Skip => "skip",
    }
}

const fn dns_role(dns: DnsRole) -> &'static str {
    match dns {
        DnsRole::Install => "install",
        DnsRole::Skip => "skip",
    }
}

const fn deploy_state(state: &DeployOperationState) -> &'static str {
    match state {
        DeployOperationState::Accepted => "accepted",
        DeployOperationState::Planning => "planning",
        DeployOperationState::Running { stage } => deploy_running_stage(*stage),
        DeployOperationState::Completed { outcome } => deploy_completion_outcome(*outcome),
        DeployOperationState::Failed { .. } => "failed",
        DeployOperationState::Cancelled { .. } => "cancelled",
    }
}

const fn deploy_completion_outcome(
    outcome: ployz_core::ops::DeployCompletionOutcome,
) -> &'static str {
    match outcome {
        ployz_core::ops::DeployCompletionOutcome::Completed => "completed",
        ployz_core::ops::DeployCompletionOutcome::CompletedWithWarnings => {
            "completed-with-warnings"
        }
        ployz_core::ops::DeployCompletionOutcome::PartiallyCompleted => "partially-completed",
        ployz_core::ops::DeployCompletionOutcome::PartiallyCompletedWithWarnings => {
            "partially-completed-with-warnings"
        }
    }
}

const fn deploy_running_stage(stage: DeployRunningStage) -> &'static str {
    match stage {
        DeployRunningStage::PreparingDataplane => "running:preparing-dataplane",
        DeployRunningStage::StartingContainers => "running:starting-containers",
        DeployRunningStage::WaitingForHealth => "running:waiting-for-health",
        DeployRunningStage::RouteCutover => "running:route-cutover",
        DeployRunningStage::ServingTargetCommit => "running:active-service-commit",
        DeployRunningStage::RemovingSupersededContainers => {
            "running:removing-superseded-containers"
        }
    }
}

const fn cert_state(state: &CertOperationState) -> &'static str {
    match state {
        CertOperationState::Accepted => "accepted",
        CertOperationState::Running { stage } => cert_running_stage(*stage),
        CertOperationState::Completed => "completed",
        CertOperationState::Failed { .. } => "failed",
        CertOperationState::Cancelled { .. } => "cancelled",
    }
}

const fn cert_running_stage(stage: CertRunningStage) -> &'static str {
    match stage {
        CertRunningStage::ChallengePublished => "running:challenge-published",
        CertRunningStage::ValidationStarted => "running:validation-started",
    }
}

const fn machine_add_state(state: &MachineAddOperationState) -> &'static str {
    match state {
        MachineAddOperationState::Pending { .. } => "pending",
        MachineAddOperationState::Joining { .. } => "joining",
        MachineAddOperationState::Completed => "completed",
        MachineAddOperationState::Failed { .. } => "failed",
        MachineAddOperationState::Cancelled { .. } => "cancelled",
    }
}

const fn machine_update_state(state: &MachineUpdateOperationState) -> &'static str {
    match state {
        MachineUpdateOperationState::Accepted => "accepted",
        MachineUpdateOperationState::Running => "running",
        MachineUpdateOperationState::Completed { .. } => "completed",
        MachineUpdateOperationState::Failed { .. } => "failed",
        MachineUpdateOperationState::Cancelled { .. } => "cancelled",
    }
}

struct DeployEventRenderContext {
    service_id: Option<ServiceId>,
}

impl DeployEventRenderContext {
    fn observe(&mut self, event: &OperationEvent) {
        match event {
            OperationEvent::DeploySubmitted { target, .. } => {
                self.service_id = Some(target.status_service_id());
            }
            OperationEvent::DeployPlanningStarted { .. }
            | OperationEvent::DeployPlanCreated { .. }
            | OperationEvent::DeployRunning { .. }
            | OperationEvent::DeployContainerStarted { .. }
            | OperationEvent::DeployHealthCheckStarted { .. }
            | OperationEvent::DeployDataplanePrepared { .. }
            | OperationEvent::DeployCleanupFinished { .. }
            | OperationEvent::DeployCompleted { .. }
            | OperationEvent::DeployFailed { .. }
            | OperationEvent::CertRenewalSubmitted { .. }
            | OperationEvent::CertChallengePublished { .. }
            | OperationEvent::CertValidationStarted { .. }
            | OperationEvent::CertCompleted { .. }
            | OperationEvent::CertFailed { .. }
            | OperationEvent::MachineAddSubmitted { .. }
            | OperationEvent::MachineAddJoined { .. }
            | OperationEvent::MachineAddCredentialProvisioned { .. }
            | OperationEvent::MachineAddCompleted { .. }
            | OperationEvent::MachineAddFailed { .. }
            | OperationEvent::MachineUpdateSubmitted { .. }
            | OperationEvent::MachineUpdateRunning { .. }
            | OperationEvent::MachineUpdateCompleted { .. }
            | OperationEvent::MachineUpdateFailed { .. }
            | OperationEvent::MachineLifecycleSubmitted { .. }
            | OperationEvent::MachineLifecycleCompleted { .. }
            | OperationEvent::MachineLifecycleFailed { .. }
            | OperationEvent::CoreReplaceSubmitted { .. }
            | OperationEvent::CoreReplaceCompleted { .. }
            | OperationEvent::CoreReplaceFailed { .. }
            | OperationEvent::NetworkRepairSubmitted { .. }
            | OperationEvent::NetworkRepairRunning { .. }
            | OperationEvent::NetworkRepairCompleted { .. }
            | OperationEvent::NetworkRepairFailed { .. }
            | OperationEvent::ServiceRestartSubmitted { .. }
            | OperationEvent::ServiceRestartRunning { .. }
            | OperationEvent::ServiceRestartContainerRestarted { .. }
            | OperationEvent::ServiceRestartCompleted { .. }
            | OperationEvent::ServiceRestartFailed { .. }
            | OperationEvent::NamespaceRemoveSubmitted { .. }
            | OperationEvent::NamespaceRemoveRunning { .. }
            | OperationEvent::NamespaceRemoveRouteBindingRemoved { .. }
            | OperationEvent::NamespaceRemoveContainerRemoved { .. }
            | OperationEvent::NamespaceRemoveCompleted { .. }
            | OperationEvent::NamespaceRemoveFailed { .. }
            | OperationEvent::Cancelled { .. } => {}
        }
    }
}

fn render_replayed_event_text(
    event: &ReplayedOperationEvent,
    context: &DeployEventRenderContext,
) -> String {
    let label = operation_event_label(&event.event);
    match &event.event {
        OperationEvent::DeployFailed { failure, .. } => format!(
            "{} {} {}",
            event.sequence.get(),
            label,
            render_deploy_failure_detail(failure, context.service_id.as_ref())
        ),
        OperationEvent::NetworkRepairFailed { failure, .. } => format!(
            "{} {} {}",
            event.sequence.get(),
            label,
            render_network_repair_failure(failure)
        ),
        OperationEvent::DeploySubmitted { .. }
        | OperationEvent::DeployPlanningStarted { .. }
        | OperationEvent::DeployPlanCreated { .. }
        | OperationEvent::DeployRunning { .. }
        | OperationEvent::DeployContainerStarted { .. }
        | OperationEvent::DeployHealthCheckStarted { .. }
        | OperationEvent::DeployDataplanePrepared { .. }
        | OperationEvent::DeployCleanupFinished { .. }
        | OperationEvent::DeployCompleted { .. }
        | OperationEvent::CertRenewalSubmitted { .. }
        | OperationEvent::CertChallengePublished { .. }
        | OperationEvent::CertValidationStarted { .. }
        | OperationEvent::CertCompleted { .. }
        | OperationEvent::CertFailed { .. }
        | OperationEvent::MachineAddSubmitted { .. }
        | OperationEvent::MachineAddJoined { .. }
        | OperationEvent::MachineAddCredentialProvisioned { .. }
        | OperationEvent::MachineAddCompleted { .. }
        | OperationEvent::MachineAddFailed { .. }
        | OperationEvent::MachineUpdateSubmitted { .. }
        | OperationEvent::MachineUpdateRunning { .. }
        | OperationEvent::MachineUpdateCompleted { .. }
        | OperationEvent::MachineUpdateFailed { .. }
        | OperationEvent::MachineLifecycleSubmitted { .. }
        | OperationEvent::MachineLifecycleCompleted { .. }
        | OperationEvent::MachineLifecycleFailed { .. }
        | OperationEvent::CoreReplaceSubmitted { .. }
        | OperationEvent::CoreReplaceCompleted { .. }
        | OperationEvent::CoreReplaceFailed { .. }
        | OperationEvent::NetworkRepairSubmitted { .. }
        | OperationEvent::NetworkRepairRunning { .. }
        | OperationEvent::NetworkRepairCompleted { .. }
        | OperationEvent::ServiceRestartSubmitted { .. }
        | OperationEvent::ServiceRestartRunning { .. }
        | OperationEvent::ServiceRestartContainerRestarted { .. }
        | OperationEvent::ServiceRestartCompleted { .. }
        | OperationEvent::ServiceRestartFailed { .. }
        | OperationEvent::NamespaceRemoveSubmitted { .. }
        | OperationEvent::NamespaceRemoveRunning { .. }
        | OperationEvent::NamespaceRemoveRouteBindingRemoved { .. }
        | OperationEvent::NamespaceRemoveContainerRemoved { .. }
        | OperationEvent::NamespaceRemoveCompleted { .. }
        | OperationEvent::NamespaceRemoveFailed { .. }
        | OperationEvent::Cancelled { .. } => {
            format!("{} {}", event.sequence.get(), label)
        }
    }
}

fn render_network_repair_failure(failure: &ployz_sdk_types::NetworkRepairFailure) -> String {
    match failure {
        ployz_sdk_types::NetworkRepairFailure::NoActiveMachines => "no-active-machines".to_owned(),
        ployz_sdk_types::NetworkRepairFailure::IntentReadFailed { message } => {
            format!("intent-read-failed message={}", message.as_str())
        }
        ployz_sdk_types::NetworkRepairFailure::DataplaneConvergenceFailed {
            machine_id,
            component,
            message,
        } => format!(
            "dataplane-convergence-failed machine={} component={} message={}",
            machine_id.as_str(),
            match component {
                ployz_sdk_types::PloyzNativeMeshComponent::WireGuard => "wireguard",
                ployz_sdk_types::PloyzNativeMeshComponent::EbpfForwarding => "ebpf-forwarding",
            },
            message.as_str(),
        ),
        ployz_sdk_types::NetworkRepairFailure::DataplaneReportInvalid { message } => {
            format!("dataplane-report-invalid message={}", message.as_str())
        }
    }
}

fn render_replayed_event_json(event: &ReplayedOperationEvent) -> String {
    serde_json::to_string(event).expect("replayed operation events serialize")
}

fn operation_event_label(event: &OperationEvent) -> &'static str {
    match event {
        OperationEvent::DeploySubmitted { .. } => "deploy.submitted",
        OperationEvent::DeployPlanningStarted { .. } => "deploy.planning",
        OperationEvent::DeployPlanCreated { .. } => "deploy.plan_created",
        OperationEvent::DeployRunning { .. } => "deploy.running",
        OperationEvent::DeployContainerStarted { .. } => "deploy.container_started",
        OperationEvent::DeployHealthCheckStarted { .. } => "deploy.health_check_started",
        OperationEvent::DeployDataplanePrepared { .. } => "deploy.dataplane_prepared",
        OperationEvent::DeployCleanupFinished { .. } => "deploy.cleanup_finished",
        OperationEvent::DeployCompleted { .. } => "deploy.completed",
        OperationEvent::DeployFailed { .. } => "deploy.failed",
        OperationEvent::CertRenewalSubmitted { .. } => "cert.submitted",
        OperationEvent::CertChallengePublished { .. } => "cert.challenge_published",
        OperationEvent::CertValidationStarted { .. } => "cert.validation_started",
        OperationEvent::CertCompleted { .. } => "cert.completed",
        OperationEvent::CertFailed { .. } => "cert.failed",
        OperationEvent::MachineAddSubmitted { .. } => "machine.add.submitted",
        OperationEvent::MachineAddJoined { .. } => "machine.add.joined",
        OperationEvent::MachineAddCredentialProvisioned { step, .. } => match step {
            ployz_core::machine::MachineCredentialProvisioningStep::Minted => {
                "machine.add.credential.minted"
            }
            ployz_core::machine::MachineCredentialProvisioningStep::Rendered => {
                "machine.add.credential.rendered"
            }
            ployz_core::machine::MachineCredentialProvisioningStep::Reloaded => {
                "machine.add.credential.reloaded"
            }
            ployz_core::machine::MachineCredentialProvisioningStep::Verified => {
                "machine.add.credential.verified"
            }
            ployz_core::machine::MachineCredentialProvisioningStep::MaterialReady => {
                "machine.add.credential.material_ready"
            }
        },
        OperationEvent::MachineAddCompleted { .. } => "machine.add.completed",
        OperationEvent::MachineAddFailed { .. } => "machine.add.failed",
        OperationEvent::MachineUpdateSubmitted { .. } => "machine.update.submitted",
        OperationEvent::MachineUpdateRunning { .. } => "machine.update.running",
        OperationEvent::MachineUpdateCompleted { .. } => "machine.update.completed",
        OperationEvent::MachineUpdateFailed { .. } => "machine.update.failed",
        OperationEvent::MachineLifecycleSubmitted { .. } => "machine.lifecycle.submitted",
        OperationEvent::MachineLifecycleCompleted { .. } => "machine.lifecycle.completed",
        OperationEvent::MachineLifecycleFailed { .. } => "machine.lifecycle.failed",
        OperationEvent::CoreReplaceSubmitted { .. } => "core.replace.submitted",
        OperationEvent::CoreReplaceCompleted { .. } => "core.replace.completed",
        OperationEvent::CoreReplaceFailed { .. } => "core.replace.failed",
        OperationEvent::NetworkRepairSubmitted { .. } => "network.repair.submitted",
        OperationEvent::NetworkRepairRunning { .. } => "network.repair.running",
        OperationEvent::NetworkRepairCompleted { .. } => "network.repair.completed",
        OperationEvent::NetworkRepairFailed { .. } => "network.repair.failed",
        OperationEvent::ServiceRestartSubmitted { .. } => "service.restart.submitted",
        OperationEvent::ServiceRestartRunning { .. } => "service.restart.running",
        OperationEvent::ServiceRestartContainerRestarted { .. } => {
            "service.restart.container_restarted"
        }
        OperationEvent::ServiceRestartCompleted { .. } => "service.restart.completed",
        OperationEvent::ServiceRestartFailed { .. } => "service.restart.failed",
        OperationEvent::NamespaceRemoveSubmitted { .. } => "namespace.remove.submitted",
        OperationEvent::NamespaceRemoveRunning { .. } => "namespace.remove.running",
        OperationEvent::NamespaceRemoveRouteBindingRemoved { .. } => {
            "namespace.remove.route_binding_removed"
        }
        OperationEvent::NamespaceRemoveContainerRemoved { .. } => {
            "namespace.remove.container_removed"
        }
        OperationEvent::NamespaceRemoveCompleted { .. } => "namespace.remove.completed",
        OperationEvent::NamespaceRemoveFailed { .. } => "namespace.remove.failed",
        OperationEvent::Cancelled { .. } => "cancelled",
    }
}

fn render_deploy_failure_detail(
    failure: &DeployOperationFailure,
    service_id: Option<&ServiceId>,
) -> String {
    format!(
        "class {} service {} {} {}",
        failure.failure_class().as_str(),
        deploy_failure_service(failure, service_id),
        deploy_failure_machines(failure),
        deploy_failure_evidence(failure),
    )
}

fn deploy_failure_service(
    failure: &DeployOperationFailure,
    service_id: Option<&ServiceId>,
) -> String {
    service_id
        .or_else(|| deploy_failure_service_id(failure))
        .map(|service_id| service_id.as_str().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn deploy_failure_service_id(failure: &DeployOperationFailure) -> Option<&ServiceId> {
    match failure {
        DeployOperationFailure::PlanningFailed { service_id, .. }
        | DeployOperationFailure::ArtifactUnavailable { service_id, .. } => Some(service_id),
        DeployOperationFailure::ControlPlaneCommitFailed { scope, .. } => match scope {
            ControlPlaneCommitScope::ServiceEntry { service_id, .. } => Some(service_id),
            ControlPlaneCommitScope::Namespace { .. }
            | ControlPlaneCommitScope::VolumePin { .. } => None,
        },
        DeployOperationFailure::NoUsableMachines { .. }
        | DeployOperationFailure::DataplaneUnavailable { .. }
        | DeployOperationFailure::DataplanePrepareTimedOut { .. }
        | DeployOperationFailure::DataplanePrepareInvalidReport { .. }
        | DeployOperationFailure::RuntimeUnavailable { .. }
        | DeployOperationFailure::ContainerStartFailed { .. }
        | DeployOperationFailure::HealthCheckFailed { .. }
        | DeployOperationFailure::RouteCutoverFailed { .. } => None,
    }
}

fn deploy_failure_machines(failure: &DeployOperationFailure) -> String {
    let mut machines = Vec::new();
    match failure {
        DeployOperationFailure::NoUsableMachines { reasons } => {
            for reason in reasons {
                if !machines.contains(&&reason.machine_id) {
                    machines.push(&reason.machine_id);
                }
            }
        }
        DeployOperationFailure::DataplaneUnavailable { machine_id, .. }
        | DeployOperationFailure::RuntimeUnavailable { machine_id, .. }
        | DeployOperationFailure::ContainerStartFailed { machine_id, .. } => {
            if !machines.contains(&machine_id) {
                machines.push(machine_id);
            }
        }
        DeployOperationFailure::DataplanePrepareTimedOut {
            machines: timed_out,
            ..
        } => {
            for machine_id in timed_out {
                if !machines.contains(&machine_id) {
                    machines.push(machine_id);
                }
            }
        }
        DeployOperationFailure::HealthCheckFailed { health_check, .. } => match health_check {
            HealthCheckFailure::ProbeFailed { machine_id, .. } => {
                if !machines.contains(&machine_id) {
                    machines.push(machine_id);
                }
            }
            HealthCheckFailure::TimedOut { .. } => {}
        },
        DeployOperationFailure::RouteCutoverFailed { reason, .. } => match reason {
            RouteCutoverFailureReason::GatewayUnavailable { machine_id } => {
                if !machines.contains(&machine_id) {
                    machines.push(machine_id);
                }
            }
            RouteCutoverFailureReason::RouteRejected { .. }
            | RouteCutoverFailureReason::StateStoreFailed { .. }
            | RouteCutoverFailureReason::TimedOut { .. } => {}
        },
        DeployOperationFailure::PlanningFailed { .. }
        | DeployOperationFailure::ArtifactUnavailable { .. }
        | DeployOperationFailure::DataplanePrepareInvalidReport { .. }
        | DeployOperationFailure::ControlPlaneCommitFailed { .. } => {}
    }

    for artifact in failure.retained_artifacts() {
        match artifact {
            RetainedArtifact::CreatedContainer { machine_id, .. }
            | RetainedArtifact::StartedContainer { machine_id, .. }
            | RetainedArtifact::ContainerStopFailed { machine_id, .. } => {
                if !machines.contains(&machine_id) {
                    machines.push(machine_id);
                }
            }
        }
    }

    match machines.as_slice() {
        [] => "machine unknown".to_owned(),
        [machine_id] => format!("machine {}", machine_id.as_str()),
        many => format!(
            "machines {}",
            many.iter()
                .map(|machine_id| machine_id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

fn deploy_failure_evidence(failure: &DeployOperationFailure) -> String {
    let mut container_ids = Vec::new();
    let mut log_commands = Vec::new();
    if let DeployOperationFailure::ContainerStartFailed { container_id, .. } = failure {
        container_ids.push(container_id);
        log_commands.push(format!("ployzctl logs {}", container_id.as_str()));
    }

    for artifact in failure.retained_artifacts() {
        match artifact {
            RetainedArtifact::CreatedContainer { container_id, .. }
            | RetainedArtifact::ContainerStopFailed { container_id, .. } => {
                if !container_ids.contains(&container_id) {
                    container_ids.push(container_id);
                }
                let command = format!("ployzctl logs {}", container_id.as_str());
                if !log_commands.contains(&command) {
                    log_commands.push(command);
                }
            }
            RetainedArtifact::StartedContainer {
                container_id,
                log_hint,
                ..
            } => {
                if !container_ids.contains(&container_id) {
                    container_ids.push(container_id);
                }
                let command = log_hint.as_str().to_owned();
                if !log_commands.contains(&command) {
                    log_commands.push(command);
                }
            }
        }
    }

    match container_ids.as_slice() {
        [] => "evidence none".to_owned(),
        ids => format!(
            "evidence {} logs {}",
            ids.iter()
                .map(|container_id| container_id.as_str())
                .collect::<Vec<_>>()
                .join(","),
            log_commands.join("; ")
        ),
    }
}
