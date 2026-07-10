use clap::Args;
use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::ops::MachineAddOperationState;
use ployz_core::ops::{
    CertOperationState, CertRunningStage, DeployOperationFailure, DeployOperationState,
    DeployRunningStage, EventSequence, MAX_OPERATION_EVENT_REPLAY_LIMIT,
    MachineUpdateOperationState, OperationEvent, OperationEventReplayLimit,
    OperationEventReplayRequest, OperationKind, OperationStatus, OperationStatusSnapshot,
    ReplayedOperationEvent,
};
use ployz_core::roles::GatewayRole;
use ployz_sdk_types::{OpsListRequest, OpsListResult, OpsStatusRequest};

use crate::commands::PloyzctlCliError;
use crate::commands::deploy_failure::{DeployFailureView, certificate_provision_failure_detail};

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
    pub events: Vec<ReplayedOperationEvent>,
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
    pub fn new(snapshot: OperationStatusSnapshot, events: Vec<ReplayedOperationEvent>) -> Self {
        Self { snapshot, events }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let failure_detail = status_failure_detail(&self.snapshot.status)
            .map(|detail| format!("{detail}\n"))
            .unwrap_or_default();

        let status = format!(
            "operation {}\nkind {}\n{}\nstate {}\n{}last-event {}\n",
            self.snapshot.status.id().as_str(),
            operation_kind_name(self.snapshot.status.kind()),
            operation_subject(&self.snapshot.status),
            operation_state(&self.snapshot.status),
            failure_detail,
            self.snapshot.status.last_event_sequence().get(),
        );
        let timeline = render_watch_events(&self.events, OpsWatchOutput::Text);
        format!("{status}timeline\n{timeline}")
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
        render_watch_events(&self.events, self.output)
    }
}

fn render_watch_events(events: &[ReplayedOperationEvent], output: OpsWatchOutput) -> String {
    let rendered = match output {
        OpsWatchOutput::Text => events
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
        OpsWatchOutput::Json => events
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
        OperationKind::ServiceRestart => "service-restart",
        OperationKind::ManagedLease => "managed-lease",
        OperationKind::NamespaceRemove => "namespace-remove",
        OperationKind::VolumeRemove => "volume-remove",
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
            "machine {} name {} gateway {}",
            machine_id.as_str(),
            name.as_str(),
            gateway_role(roles.gateway)
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
        OperationStatus::ServiceRestart { service_id, .. } => {
            format!("service {}", service_id.as_str())
        }
        OperationStatus::NamespaceRemove { namespace_id, .. } => {
            format!("namespace {}", namespace_id.as_str())
        }
        OperationStatus::VolumeRemove {
            namespace_id,
            volume_name,
            ..
        } => format!("volume {}/{}", namespace_id.as_str(), volume_name.as_str()),
        OperationStatus::ManagedLease { subject, .. } => match subject {
            ployz_sdk_types::ManagedLeaseSubject::Acquire => "lease acquisition".to_owned(),
            ployz_sdk_types::ManagedLeaseSubject::DownloadBundle { lease } => {
                format!("lease {} bundle download", lease.as_str())
            }
            ployz_sdk_types::ManagedLeaseSubject::Renew { lease } => {
                format!("lease {} renewal", lease.as_str())
            }
        },
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
        OperationStatus::ServiceRestart { state, .. } => service_restart_state(state).to_owned(),
        OperationStatus::ManagedLease { state, .. } => managed_lease_state(state).to_owned(),
        OperationStatus::NamespaceRemove { state, .. } => namespace_remove_state(state).to_owned(),
        OperationStatus::VolumeRemove { state, .. } => volume_remove_state(state).to_owned(),
    }
}

const fn managed_lease_state(state: &ployz_sdk_types::ManagedLeaseOperationState) -> &'static str {
    match state {
        ployz_sdk_types::ManagedLeaseOperationState::Accepted => "accepted",
        ployz_sdk_types::ManagedLeaseOperationState::Completed => "completed",
        ployz_sdk_types::ManagedLeaseOperationState::Failed { .. } => "failed",
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

const fn volume_remove_state(state: &ployz_sdk_types::VolumeRemoveOperationState) -> &'static str {
    match state {
        ployz_sdk_types::VolumeRemoveOperationState::Accepted => "accepted",
        ployz_sdk_types::VolumeRemoveOperationState::Running { stage } => match stage {
            ployz_sdk_types::VolumeRemoveRunningStage::RemovingVolumeData => {
                "running:removing-volume-data"
            }
        },
        ployz_sdk_types::VolumeRemoveOperationState::Completed => "completed",
        ployz_sdk_types::VolumeRemoveOperationState::Failed { .. } => "failed",
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
        OperationStatus::ManagedLease {
            state: ployz_sdk_types::ManagedLeaseOperationState::Failed { failure },
            ..
        } => Some(format!("failure {}", failure.message.as_str())),
        OperationStatus::Cert {
            state: CertOperationState::Failed { failure },
            ..
        } => Some(format!(
            "failure {}",
            certificate_provision_failure_detail(failure.failure(), None)
        )),
        OperationStatus::Deploy { .. }
        | OperationStatus::Cert { .. }
        | OperationStatus::MachineAdd { .. }
        | OperationStatus::MachineUpdate { .. }
        | OperationStatus::MachineLifecycle { .. }
        | OperationStatus::CoreReplace { .. }
        | OperationStatus::ServiceRestart { .. }
        | OperationStatus::ManagedLease { .. }
        | OperationStatus::NamespaceRemove { .. }
        | OperationStatus::VolumeRemove { .. } => None,
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
        DeployRunningStage::EnsuringImages => "running:ensuring-images",
        DeployRunningStage::StartingContainers => "running:starting-containers",
        DeployRunningStage::WaitingForHealth => "running:waiting-for-health",
        DeployRunningStage::EnsuringCertificates => "running:ensuring-certificates",
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
            | OperationEvent::DeployImageResolved { .. }
            | OperationEvent::DeployPlanCreated { .. }
            | OperationEvent::DeployRunning { .. }
            | OperationEvent::DeployContainerStarted { .. }
            | OperationEvent::DeployHealthCheckStarted { .. }
            | OperationEvent::DeployDataplanePrepared { .. }
            | OperationEvent::DeployImageAvailabilityVerified { .. }
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
            | OperationEvent::ServiceRestartSubmitted { .. }
            | OperationEvent::ServiceRestartRunning { .. }
            | OperationEvent::ServiceRestartContainerRestarted { .. }
            | OperationEvent::ServiceRestartCompleted { .. }
            | OperationEvent::ServiceRestartFailed { .. }
            | OperationEvent::ManagedLeaseSubmitted { .. }
            | OperationEvent::ManagedLeaseCompleted { .. }
            | OperationEvent::ManagedLeaseFailed { .. }
            | OperationEvent::NamespaceRemoveSubmitted { .. }
            | OperationEvent::NamespaceRemoveRunning { .. }
            | OperationEvent::NamespaceRemoveRouteBindingRemoved { .. }
            | OperationEvent::NamespaceRemoveContainerRemoved { .. }
            | OperationEvent::NamespaceRemoveCompleted { .. }
            | OperationEvent::NamespaceRemoveFailed { .. }
            | OperationEvent::VolumeRemoveSubmitted { .. }
            | OperationEvent::VolumeRemoveRunning { .. }
            | OperationEvent::VolumeRemoveCompleted { .. }
            | OperationEvent::VolumeRemoveFailed { .. }
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
        OperationEvent::DeployImageResolved {
            service_id,
            machine_id,
            requested,
            resolved,
            credential_supplied,
            ..
        } => format!(
            "{} {} service {} machine {} {} -> {} credential {}",
            event.sequence.get(),
            label,
            service_id.as_str(),
            machine_id.as_str(),
            requested.as_str(),
            resolved.as_str(),
            if *credential_supplied {
                "supplied"
            } else {
                "absent"
            }
        ),
        OperationEvent::DeploySubmitted { .. }
        | OperationEvent::DeployPlanningStarted { .. }
        | OperationEvent::DeployPlanCreated { .. }
        | OperationEvent::DeployRunning { .. }
        | OperationEvent::DeployContainerStarted { .. }
        | OperationEvent::DeployHealthCheckStarted { .. }
        | OperationEvent::DeployDataplanePrepared { .. }
        | OperationEvent::DeployImageAvailabilityVerified { .. }
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
        | OperationEvent::ServiceRestartSubmitted { .. }
        | OperationEvent::ServiceRestartRunning { .. }
        | OperationEvent::ServiceRestartContainerRestarted { .. }
        | OperationEvent::ServiceRestartCompleted { .. }
        | OperationEvent::ServiceRestartFailed { .. }
        | OperationEvent::ManagedLeaseSubmitted { .. }
        | OperationEvent::ManagedLeaseCompleted { .. }
        | OperationEvent::ManagedLeaseFailed { .. }
        | OperationEvent::NamespaceRemoveSubmitted { .. }
        | OperationEvent::NamespaceRemoveRunning { .. }
        | OperationEvent::NamespaceRemoveRouteBindingRemoved { .. }
        | OperationEvent::NamespaceRemoveContainerRemoved { .. }
        | OperationEvent::NamespaceRemoveCompleted { .. }
        | OperationEvent::NamespaceRemoveFailed { .. }
        | OperationEvent::VolumeRemoveSubmitted { .. }
        | OperationEvent::VolumeRemoveRunning { .. }
        | OperationEvent::VolumeRemoveCompleted { .. }
        | OperationEvent::VolumeRemoveFailed { .. }
        | OperationEvent::Cancelled { .. } => {
            format!("{} {}", event.sequence.get(), label)
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
        OperationEvent::DeployImageResolved { .. } => "deploy.image_resolved",
        OperationEvent::DeployPlanCreated { .. } => "deploy.plan_created",
        OperationEvent::DeployRunning { .. } => "deploy.running",
        OperationEvent::DeployContainerStarted { .. } => "deploy.container_started",
        OperationEvent::DeployHealthCheckStarted { .. } => "deploy.health_check_started",
        OperationEvent::DeployDataplanePrepared { .. } => "deploy.dataplane_prepared",
        OperationEvent::DeployImageAvailabilityVerified { .. } => {
            "deploy.image_availability_verified"
        }
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
        OperationEvent::ServiceRestartSubmitted { .. } => "service.restart.submitted",
        OperationEvent::ServiceRestartRunning { .. } => "service.restart.running",
        OperationEvent::ServiceRestartContainerRestarted { .. } => {
            "service.restart.container_restarted"
        }
        OperationEvent::ServiceRestartCompleted { .. } => "service.restart.completed",
        OperationEvent::ServiceRestartFailed { .. } => "service.restart.failed",
        OperationEvent::ManagedLeaseSubmitted { .. } => "managed.lease.submitted",
        OperationEvent::ManagedLeaseCompleted { .. } => "managed.lease.completed",
        OperationEvent::ManagedLeaseFailed { .. } => "managed.lease.failed",
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
        OperationEvent::VolumeRemoveSubmitted { .. } => "volume.remove.submitted",
        OperationEvent::VolumeRemoveRunning { .. } => "volume.remove.running",
        OperationEvent::VolumeRemoveCompleted { .. } => "volume.remove.completed",
        OperationEvent::VolumeRemoveFailed { .. } => "volume.remove.failed",
        OperationEvent::Cancelled { .. } => "cancelled",
    }
}

fn render_deploy_failure_detail(
    failure: &DeployOperationFailure,
    service_id: Option<&ServiceId>,
) -> String {
    let failure_view = DeployFailureView::new(failure, service_id);
    let detail = format!(
        "class {} service {} {} {}",
        failure.failure_class().as_str(),
        failure_view.service(),
        failure_view.render_machines(),
        failure_view.evidence(),
    );
    let Some(guidance) = failure_view.guidance() else {
        return detail;
    };
    format!("{detail} guidance {guidance}")
}

#[cfg(test)]
mod tests {
    use super::status_failure_detail;
    use ployz_core::cert::{
        ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow,
        CertificateProvisionFailure,
    };
    use ployz_core::ids::{CertId, MachineId, OperationId};
    use ployz_core::ops::{
        CertOperationFailure, CertOperationState, EventSequence, FailureMessage, OperationStatus,
        RouteHostname,
    };

    #[test]
    fn every_certificate_failure_renders_in_cert_status() {
        let message = || FailureMessage::try_new("failed evidence").expect("valid message");
        let cases = [
            (
                CertificateProvisionFailure::OperationEvidenceWrite { message: message() },
                "operation evidence write",
            ),
            (
                CertificateProvisionFailure::DnsPreflight { message: message() },
                "DNS preflight",
            ),
            (
                CertificateProvisionFailure::ChallengePublish { message: message() },
                "challenge publish",
            ),
            (
                CertificateProvisionFailure::ChallengeReadiness {
                    missing_machine_ids: vec![machine_id("gateway_a")],
                },
                "missing gateway acknowledgements from gateway_a",
            ),
            (
                CertificateProvisionFailure::AcmeValidation { message: message() },
                "ACME validation",
            ),
            (
                CertificateProvisionFailure::GatewayArtifactPush {
                    machine_id: machine_id("gateway_a"),
                    message: message(),
                },
                "gateway artifact push",
            ),
            (
                CertificateProvisionFailure::ActiveCertCommit {
                    attempted_active_cert: active_certificate(),
                    message: message(),
                },
                "active certificate commit",
            ),
        ];

        for (failure, expected) in cases {
            let cert_id = cert_id();
            let failure = CertOperationFailure::try_new(cert_id.clone(), failure, None)
                .expect("matching certificate evidence");
            let status = OperationStatus::Cert {
                id: OperationId::try_new("op_cert").expect("valid operation id"),
                cert_id,
                state: CertOperationState::Failed { failure },
                last_event_sequence: EventSequence::try_new(2).expect("valid event sequence"),
            };

            assert!(
                status_failure_detail(&status)
                    .expect("failed cert has detail")
                    .contains(expected),
                "missing typed failure detail: {expected}"
            );
        }
    }

    fn active_certificate() -> ActiveCertState {
        ActiveCertState {
            cert_id: cert_id(),
            hostname: RouteHostname::try_new("api.example.com").expect("valid hostname"),
            bundle_ref: CertBundleRef::try_new(format!(
                "sha256:{}:/var/lib/ployz/certificates/cert_api.bundle",
                "a".repeat(64)
            ))
            .expect("valid bundle ref"),
            validity: CertValidityWindow::try_new(
                CertValidAt::try_new(1).expect("valid not-before"),
                CertValidAt::try_new(2).expect("valid not-after"),
            )
            .expect("valid validity"),
        }
    }

    fn cert_id() -> CertId {
        CertId::try_new("cert_api").expect("valid cert id")
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }
}
