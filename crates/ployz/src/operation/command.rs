use clap::Args;
use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::operation::MachineAddOperationState;
use ployz_core::operation::{
    CertOperationState, CertRunningStage, CredentialGrantAction, CredentialGrantFailure,
    CredentialGrantOperationState, DeployOperationFailure, DeployOperationState,
    DeployRunningStage, EventSequence, MAX_OPERATION_EVENT_REPLAY_LIMIT,
    MachineUpdateOperationState, OperationEvent, OperationEventReplayLimit,
    OperationEventReplayRequest, OperationKind, OperationStatus, OperationStatusSnapshot,
    ReplayedOperationEvent,
};
use ployz_core::roles::GatewayRole;
use ployz_sdk_types::{AcceptedOperation, OpsListRequest, OpsListResult, OpsStatusRequest};

use crate::certificate::presentation::provision_failure_detail;
use crate::commands::PloyzctlCliError;
use crate::deploy::failure::DeployFailureView;

mod network_repair;

use network_repair::{network_repair_state, render_network_repair_failure};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsStatusCommand {
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsListCommand {
    pub active_only: bool,
    pub before: Option<OperationId>,
}

impl OpsListCommand {
    #[must_use]
    pub fn into_request(self) -> OpsListRequest {
        OpsListRequest {
            active_only: self.active_only,
            before: self.before,
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

pub(crate) fn ops_list_command(parsed: OpsListCli) -> Result<OpsListCommand, PloyzctlCliError> {
    let before = parsed
        .before
        .map(|operation_id| parse_operation_id_for("--before", &operation_id))
        .transpose()?;
    Ok(OpsListCommand {
        active_only: parsed.active,
        before,
    })
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
    #[arg(long)]
    before: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct OpsWatchCli {
    operation_id: String,
    #[arg(long)]
    json: bool,
}

fn parse_operation_id(operation_id: &str) -> Result<OperationId, PloyzctlCliError> {
    parse_operation_id_for("<operation_id>", operation_id)
}

fn parse_operation_id_for(
    flag: &'static str,
    operation_id: &str,
) -> Result<OperationId, PloyzctlCliError> {
    OperationId::try_new(operation_id.to_owned()).map_err(|error| PloyzctlCliError::InvalidValue {
        flag,
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
                    "{} {} {} {}{}",
                    operation.status.id().as_str(),
                    operation_kind_name(operation.status.kind()),
                    operation_subject(&operation.status),
                    operation_state(&operation.status),
                    operation_origin(&operation.status)
                        .map(|origin| format!(" origin {origin}"))
                        .unwrap_or_default(),
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

    #[must_use]
    pub fn render_more_hint(&self, active_only: bool) -> String {
        if !self.result.has_more {
            return String::new();
        }
        let Some(oldest) = self.result.operations.last() else {
            return String::new();
        };
        let active = if active_only { " --active" } else { "" };
        format!(
            "More operations available:\n  ployz ops list{active} --before {}\n",
            oldest.status.id().as_str()
        )
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
        let origin = operation_origin(&self.snapshot.status)
            .map(|origin| format!("origin {origin}\n"))
            .unwrap_or_default();

        let status = format!(
            "operation {}\nkind {}\n{}\n{}state {}\n{}last-event {}\n",
            self.snapshot.status.id().as_str(),
            operation_kind_name(self.snapshot.status.kind()),
            operation_subject(&self.snapshot.status),
            origin,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedOperationOutput {
    pub accepted: AcceptedOperation,
}

impl AcceptedOperationOutput {
    #[must_use]
    pub const fn from_accepted(accepted: AcceptedOperation) -> Self {
        Self { accepted }
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "operation {}\nwatch ployz ops watch {}\n",
            self.accepted.operation_id.as_str(),
            self.accepted.operation_id.as_str()
        )
    }
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
        OperationKind::CredentialGrant => "credential-grant",
        OperationKind::NetworkRepair => "network-repair",
        OperationKind::ServiceRestart => "service-restart",
        OperationKind::ManagedDnsReconcile => "managed-dns-reconcile",
        OperationKind::IngressConfigure => "ingress-configure",
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
        OperationStatus::CredentialGrant { action, .. } => match action {
            CredentialGrantAction::Add { grant } => format!(
                "credential {} name {} role {}",
                grant.public_key.as_str(),
                grant.name.as_str(),
                credential_role_name(grant.role)
            ),
            CredentialGrantAction::Remove { public_key } => {
                format!("credential {}", public_key.as_str())
            }
        },
        OperationStatus::NetworkRepair {
            target_machine_id, ..
        } => target_machine_id.as_ref().map_or_else(
            || "cluster".to_owned(),
            |machine_id| format!("machine {}", machine_id.as_str()),
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
        OperationStatus::ManagedDnsReconcile { subject, .. } => match subject {
            ployz_sdk_types::ManagedDnsReconcileSubject::Acquire => {
                "Ployz DNS target acquisition".to_owned()
            }
            ployz_sdk_types::ManagedDnsReconcileSubject::ProjectionApply { lease, .. } => {
                format!("Ployz DNS target {} projection apply", lease.as_str())
            }
            ployz_sdk_types::ManagedDnsReconcileSubject::AuthorizedWithdraw { lease, .. } => {
                format!("Ployz DNS target {} withdrawal", lease.as_str())
            }
            ployz_sdk_types::ManagedDnsReconcileSubject::LeaseRenewal { lease, .. } => {
                format!("Ployz DNS target {} lease renewal", lease.as_str())
            }
        },
        OperationStatus::IngressConfigure { .. } => "cluster ingress configuration".to_owned(),
    }
}

fn operation_origin(status: &OperationStatus) -> Option<&str> {
    let OperationStatus::Deploy { origin, .. } = status else {
        return None;
    };
    origin
        .as_ref()
        .map(ployz_core::deploy::DeployOrigin::as_str)
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
        ployz_sdk_types::MachineLifecycleOperationState::Interrupted { .. } => "interrupted",
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
        OperationStatus::CredentialGrant { state, .. } => credential_grant_state(state).to_owned(),
        OperationStatus::NetworkRepair { state, .. } => network_repair_state(state).to_owned(),
        OperationStatus::ServiceRestart { state, .. } => service_restart_state(state).to_owned(),
        OperationStatus::ManagedDnsReconcile { state, .. } => {
            managed_dns_reconcile_state(state).to_owned()
        }
        OperationStatus::IngressConfigure { state, .. } => {
            ingress_configure_state(state).to_owned()
        }
        OperationStatus::NamespaceRemove { state, .. } => namespace_remove_state(state).to_owned(),
        OperationStatus::VolumeRemove { state, .. } => volume_remove_state(state).to_owned(),
    }
}

const fn ingress_configure_state(
    state: &ployz_sdk_types::IngressConfigureOperationState,
) -> &'static str {
    match state {
        ployz_sdk_types::IngressConfigureOperationState::Accepted => "accepted",
        ployz_sdk_types::IngressConfigureOperationState::Completed => "completed",
        ployz_sdk_types::IngressConfigureOperationState::Failed { .. } => "failed",
        ployz_sdk_types::IngressConfigureOperationState::Interrupted { .. } => "interrupted",
    }
}

const fn managed_dns_reconcile_state(
    state: &ployz_sdk_types::ManagedDnsReconcileOperationState,
) -> &'static str {
    match state {
        ployz_sdk_types::ManagedDnsReconcileOperationState::Accepted => "accepted",
        ployz_sdk_types::ManagedDnsReconcileOperationState::Completed => "completed",
        ployz_sdk_types::ManagedDnsReconcileOperationState::Failed { .. } => "failed",
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
        ployz_sdk_types::ServiceRestartOperationState::Interrupted { .. } => "interrupted",
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
        ployz_sdk_types::NamespaceRemoveOperationState::Interrupted { .. } => "interrupted",
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
        ployz_sdk_types::VolumeRemoveOperationState::Interrupted { .. } => "interrupted",
    }
}

fn status_failure_detail(status: &OperationStatus) -> Option<String> {
    let interruption = match status {
        OperationStatus::Deploy {
            state: DeployOperationState::Interrupted { evidence },
            ..
        }
        | OperationStatus::CredentialGrant {
            state: CredentialGrantOperationState::Interrupted { evidence },
            ..
        }
        | OperationStatus::MachineUpdate {
            state: MachineUpdateOperationState::Interrupted { evidence },
            ..
        } => Some(evidence),
        OperationStatus::IngressConfigure {
            state: ployz_sdk_types::IngressConfigureOperationState::Interrupted { evidence },
            ..
        }
        | OperationStatus::MachineLifecycle {
            state: ployz_sdk_types::MachineLifecycleOperationState::Interrupted { evidence },
            ..
        }
        | OperationStatus::NetworkRepair {
            state: ployz_sdk_types::NetworkRepairOperationState::Interrupted { evidence },
            ..
        }
        | OperationStatus::ServiceRestart {
            state: ployz_sdk_types::ServiceRestartOperationState::Interrupted { evidence },
            ..
        }
        | OperationStatus::NamespaceRemove {
            state: ployz_sdk_types::NamespaceRemoveOperationState::Interrupted { evidence },
            ..
        }
        | OperationStatus::VolumeRemove {
            state: ployz_sdk_types::VolumeRemoveOperationState::Interrupted { evidence },
            ..
        } => Some(evidence),
        OperationStatus::Deploy { .. }
        | OperationStatus::Cert { .. }
        | OperationStatus::MachineAdd { .. }
        | OperationStatus::MachineUpdate { .. }
        | OperationStatus::MachineLifecycle { .. }
        | OperationStatus::CoreReplace { .. }
        | OperationStatus::CredentialGrant { .. }
        | OperationStatus::NetworkRepair { .. }
        | OperationStatus::ServiceRestart { .. }
        | OperationStatus::ManagedDnsReconcile { .. }
        | OperationStatus::IngressConfigure { .. }
        | OperationStatus::NamespaceRemove { .. }
        | OperationStatus::VolumeRemove { .. } => None,
    };
    if let Some(evidence) = interruption {
        return Some(format!(
            "interruption {}",
            serde_json::to_string(evidence).unwrap_or_else(|_| "evidence unavailable".to_owned())
        ));
    }
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
        OperationStatus::ManagedDnsReconcile {
            state: ployz_sdk_types::ManagedDnsReconcileOperationState::Failed { failure },
            ..
        } => Some(format!("failure {}", failure.message.as_str())),
        OperationStatus::CredentialGrant {
            state: CredentialGrantOperationState::Failed { failure },
            ..
        } => Some(format!("failure {}", credential_grant_failure(failure))),
        OperationStatus::Cert {
            state: CertOperationState::Failed { failure },
            ..
        } => Some(format!(
            "failure {}",
            provision_failure_detail(failure.failure(), None)
        )),
        OperationStatus::IngressConfigure {
            state: ployz_sdk_types::IngressConfigureOperationState::Failed { failure },
            ..
        } => Some(format!("failure {}", ingress_configure_failure(failure))),
        OperationStatus::Deploy { .. }
        | OperationStatus::Cert { .. }
        | OperationStatus::MachineAdd { .. }
        | OperationStatus::MachineUpdate { .. }
        | OperationStatus::MachineLifecycle { .. }
        | OperationStatus::CoreReplace { .. }
        | OperationStatus::CredentialGrant { .. }
        | OperationStatus::NetworkRepair { .. }
        | OperationStatus::ServiceRestart { .. }
        | OperationStatus::ManagedDnsReconcile { .. }
        | OperationStatus::IngressConfigure { .. }
        | OperationStatus::NamespaceRemove { .. }
        | OperationStatus::VolumeRemove { .. } => None,
    }
}

fn ingress_configure_failure(failure: &ployz_sdk_types::IngressConfigureFailure) -> &str {
    match failure {
        ployz_sdk_types::IngressConfigureFailure::InvalidConfiguration { message }
        | ployz_sdk_types::IngressConfigureFailure::IntentStoreFailed { message } => {
            message.as_str()
        }
    }
}

const fn credential_grant_state(state: &CredentialGrantOperationState) -> &'static str {
    match state {
        CredentialGrantOperationState::Accepted => "accepted",
        CredentialGrantOperationState::Completed => "completed",
        CredentialGrantOperationState::Failed { .. } => "failed",
        CredentialGrantOperationState::Cancelled { .. } => "cancelled",
        CredentialGrantOperationState::Interrupted { .. } => "interrupted",
    }
}

fn credential_grant_failure(failure: &CredentialGrantFailure) -> String {
    match failure {
        CredentialGrantFailure::LastOperator => {
            "the final Operator credential cannot be removed".to_owned()
        }
        CredentialGrantFailure::RoleChangeRequiresExplicitOperation { current, requested } => {
            format!(
                "role change from {} to {} requires an explicit role-change operation",
                credential_role_name(*current),
                credential_role_name(*requested)
            )
        }
        CredentialGrantFailure::IntentStoreFailed {
            message,
            intent_committed,
        } => credential_mutation_failure("intent store", message.as_str(), *intent_committed),
        CredentialGrantFailure::AuthorizationRenderFailed {
            message,
            intent_committed,
        } => {
            credential_mutation_failure("authorization render", message.as_str(), *intent_committed)
        }
        CredentialGrantFailure::NatsReloadFailed {
            message,
            intent_committed,
        } => credential_mutation_failure("NATS reload", message.as_str(), *intent_committed),
    }
}

fn credential_mutation_failure(stage: &str, message: &str, intent_committed: bool) -> String {
    format!("{stage}: {message} (intent committed: {intent_committed})")
}

const fn credential_role_name(role: ployz_sdk_types::CredentialRole) -> &'static str {
    match role {
        ployz_sdk_types::CredentialRole::Operator => "operator",
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
        DeployOperationState::Interrupted { .. } => "interrupted",
    }
}

const fn deploy_completion_outcome(
    outcome: ployz_core::operation::DeployCompletionOutcome,
) -> &'static str {
    match outcome {
        ployz_core::operation::DeployCompletionOutcome::Completed => "completed",
        ployz_core::operation::DeployCompletionOutcome::CompletedWithWarnings => {
            "completed-with-warnings"
        }
        ployz_core::operation::DeployCompletionOutcome::PartiallyCompleted => "partially-completed",
        ployz_core::operation::DeployCompletionOutcome::PartiallyCompletedWithWarnings => {
            "partially-completed-with-warnings"
        }
    }
}

const fn deploy_running_stage(stage: DeployRunningStage) -> &'static str {
    match stage {
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
        MachineUpdateOperationState::Interrupted { .. } => "interrupted",
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
            | OperationEvent::DeployPhaseStarted { .. }
            | OperationEvent::DeployPhaseFinished { .. }
            | OperationEvent::DeployImageAvailabilityVerified { .. }
            | OperationEvent::DeployCleanupFinished { .. }
            | OperationEvent::DeployCompleted { .. }
            | OperationEvent::DeployFailed { .. }
            | OperationEvent::CertProvisionSubmitted { .. }
            | OperationEvent::CertChallengePublished { .. }
            | OperationEvent::CertValidationStarted { .. }
            | OperationEvent::CertWarning { .. }
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
            | OperationEvent::CredentialGrantSubmitted { .. }
            | OperationEvent::CredentialGrantCompleted { .. }
            | OperationEvent::CredentialGrantFailed { .. }
            | OperationEvent::NetworkRepairSubmitted { .. }
            | OperationEvent::NetworkRepairRunning { .. }
            | OperationEvent::NetworkRepairDataplaneConverged { .. }
            | OperationEvent::NetworkRepairMachineFactsRefreshed { .. }
            | OperationEvent::NetworkRepairDnsRefreshConfirmed { .. }
            | OperationEvent::NetworkRepairCompleted { .. }
            | OperationEvent::NetworkRepairFailed { .. }
            | OperationEvent::ServiceRestartSubmitted { .. }
            | OperationEvent::ServiceRestartRunning { .. }
            | OperationEvent::ServiceRestartContainerRestarted { .. }
            | OperationEvent::ServiceRestartCompleted { .. }
            | OperationEvent::ServiceRestartFailed { .. }
            | OperationEvent::ManagedDnsReconcileSubmitted { .. }
            | OperationEvent::ManagedDnsReconcileCompleted { .. }
            | OperationEvent::ManagedDnsReconcileFailed { .. }
            | OperationEvent::IngressConfigureSubmitted { .. }
            | OperationEvent::IngressConfigureCompleted { .. }
            | OperationEvent::IngressConfigureFailed { .. }
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
            | OperationEvent::OperationInterrupted { .. }
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
        OperationEvent::DeploySubmitted { target, .. } => target.origin.as_ref().map_or_else(
            || format!("{} {}", event.sequence.get(), label),
            |origin| {
                format!(
                    "{} {} origin {}",
                    event.sequence.get(),
                    label,
                    origin.as_str()
                )
            },
        ),
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
        OperationEvent::OperationInterrupted { evidence, .. } => format!(
            "{} {} {}",
            event.sequence.get(),
            label,
            serde_json::to_string(evidence)
                .unwrap_or_else(|_| "interruption evidence unavailable".to_owned())
        ),
        OperationEvent::CredentialGrantFailed { failure, .. } => format!(
            "{} {} {}",
            event.sequence.get(),
            label,
            credential_grant_failure(failure)
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
        OperationEvent::DeployPlanningStarted { .. }
        | OperationEvent::DeployPlanCreated { .. }
        | OperationEvent::DeployRunning { .. }
        | OperationEvent::DeployContainerStarted { .. }
        | OperationEvent::DeployHealthCheckStarted { .. }
        | OperationEvent::DeployPhaseStarted { .. }
        | OperationEvent::DeployPhaseFinished { .. }
        | OperationEvent::DeployImageAvailabilityVerified { .. }
        | OperationEvent::DeployCleanupFinished { .. }
        | OperationEvent::DeployCompleted { .. }
        | OperationEvent::CertProvisionSubmitted { .. }
        | OperationEvent::CertChallengePublished { .. }
        | OperationEvent::CertValidationStarted { .. }
        | OperationEvent::CertWarning { .. }
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
        | OperationEvent::CredentialGrantSubmitted { .. }
        | OperationEvent::CredentialGrantCompleted { .. }
        | OperationEvent::NetworkRepairSubmitted { .. }
        | OperationEvent::NetworkRepairRunning { .. }
        | OperationEvent::NetworkRepairDataplaneConverged { .. }
        | OperationEvent::NetworkRepairMachineFactsRefreshed { .. }
        | OperationEvent::NetworkRepairDnsRefreshConfirmed { .. }
        | OperationEvent::NetworkRepairCompleted { .. }
        | OperationEvent::ServiceRestartSubmitted { .. }
        | OperationEvent::ServiceRestartRunning { .. }
        | OperationEvent::ServiceRestartContainerRestarted { .. }
        | OperationEvent::ServiceRestartCompleted { .. }
        | OperationEvent::ServiceRestartFailed { .. }
        | OperationEvent::ManagedDnsReconcileSubmitted { .. }
        | OperationEvent::ManagedDnsReconcileCompleted { .. }
        | OperationEvent::ManagedDnsReconcileFailed { .. }
        | OperationEvent::IngressConfigureSubmitted { .. }
        | OperationEvent::IngressConfigureCompleted { .. }
        | OperationEvent::IngressConfigureFailed { .. }
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
        OperationEvent::DeployPhaseStarted { .. } => "deploy.phase_started",
        OperationEvent::DeployPhaseFinished { .. } => "deploy.phase_finished",
        OperationEvent::DeployImageAvailabilityVerified { .. } => {
            "deploy.image_availability_verified"
        }
        OperationEvent::DeployCleanupFinished { .. } => "deploy.cleanup_finished",
        OperationEvent::DeployCompleted { .. } => "deploy.completed",
        OperationEvent::DeployFailed { .. } => "deploy.failed",
        OperationEvent::CertProvisionSubmitted { .. } => "cert.submitted",
        OperationEvent::CertChallengePublished { .. } => "cert.challenge_published",
        OperationEvent::CertValidationStarted { .. } => "cert.validation_started",
        OperationEvent::CertWarning { .. } => "cert.warning",
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
        OperationEvent::CredentialGrantSubmitted { action, .. } => match action {
            CredentialGrantAction::Add { .. } => "credential.add.submitted",
            CredentialGrantAction::Remove { .. } => "credential.remove.submitted",
        },
        OperationEvent::CredentialGrantCompleted { .. } => "credential.grant.completed",
        OperationEvent::CredentialGrantFailed { .. } => "credential.grant.failed",
        OperationEvent::NetworkRepairSubmitted { .. } => "network.repair.submitted",
        OperationEvent::NetworkRepairRunning { .. } => "network.repair.running",
        OperationEvent::NetworkRepairDataplaneConverged { .. } => {
            "network.repair.dataplane_converged"
        }
        OperationEvent::NetworkRepairMachineFactsRefreshed { .. } => {
            "network.repair.machine_facts_refreshed"
        }
        OperationEvent::NetworkRepairDnsRefreshConfirmed { .. } => {
            "network.repair.dns_refresh_confirmed"
        }
        OperationEvent::NetworkRepairCompleted { .. } => "network.repair.completed",
        OperationEvent::NetworkRepairFailed { .. } => "network.repair.failed",
        OperationEvent::ServiceRestartSubmitted { .. } => "service.restart.submitted",
        OperationEvent::ServiceRestartRunning { .. } => "service.restart.running",
        OperationEvent::ServiceRestartContainerRestarted { .. } => {
            "service.restart.container_restarted"
        }
        OperationEvent::ServiceRestartCompleted { .. } => "service.restart.completed",
        OperationEvent::ServiceRestartFailed { .. } => "service.restart.failed",
        OperationEvent::ManagedDnsReconcileSubmitted { .. } => "managed.dns.reconcile.submitted",
        OperationEvent::ManagedDnsReconcileCompleted { .. } => "managed.dns.reconcile.completed",
        OperationEvent::ManagedDnsReconcileFailed { .. } => "managed.dns.reconcile.failed",
        OperationEvent::IngressConfigureSubmitted { .. } => "ingress.configure.submitted",
        OperationEvent::IngressConfigureCompleted { .. } => "ingress.configure.completed",
        OperationEvent::IngressConfigureFailed { .. } => "ingress.configure.failed",
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
        OperationEvent::OperationInterrupted { .. } => "operation.interrupted",
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
    use ployz_core::certificate::{
        ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow,
        CertificateProvisionFailure,
    };
    use ployz_core::ids::{CertId, MachineId, OperationId};
    use ployz_core::operation::{
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
