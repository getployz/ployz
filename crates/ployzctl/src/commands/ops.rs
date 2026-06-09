use ployz_core::ids::OperationId;
use ployz_core::machine::MachineAddOperationState;
use ployz_core::ops::{
    BackupOperationState, BackupRunningStage, CertOperationState, CertRunningStage,
    DeployOperationState, DeployRunningStage, EventSequence, MAX_OPERATION_EVENT_REPLAY_LIMIT,
    OperationEvent, OperationEventReplayLimit, OperationEventReplayRequest,
    OperationOwnershipStatus, OperationStatus, OperationStatusSnapshot, ReplayedOperationEvent,
};
use ployz_core::roles::FirstNodeGateway;
use ployz_sdk_types::OpsStatusRequest;

use crate::commands::PloyzctlCliError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsStatusCommand {
    pub operation_id: OperationId,
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
}

impl OpsWatchCommand {
    #[must_use]
    pub fn into_request(self) -> OperationEventReplayRequest {
        OperationEventReplayRequest {
            operation_id: self.operation_id,
            start_sequence: EventSequence::try_new(1).expect("one is a valid event sequence"),
            limit: OperationEventReplayLimit::try_new(MAX_OPERATION_EVENT_REPLAY_LIMIT)
                .expect("max replay limit is valid"),
        }
    }
}

pub fn parse_ops_status_command(args: &[String]) -> Result<OpsStatusCommand, PloyzctlCliError> {
    let operation_id = parse_operation_id_arg(args)?;

    Ok(OpsStatusCommand { operation_id })
}

pub fn parse_ops_watch_command(args: &[String]) -> Result<OpsWatchCommand, PloyzctlCliError> {
    let operation_id = parse_operation_id_arg(args)?;

    Ok(OpsWatchCommand { operation_id })
}

fn parse_operation_id_arg(args: &[String]) -> Result<OperationId, PloyzctlCliError> {
    let operation_id = match args {
        [] => {
            return Err(PloyzctlCliError::MissingRequiredArgument {
                flag: "<operation_id>",
            });
        }
        [operation_id] => operation_id,
        [_, unexpected, ..] => {
            return Err(PloyzctlCliError::UnexpectedArgument {
                value: unexpected.clone(),
            });
        }
    };

    let operation_id = OperationId::try_new(operation_id.clone()).map_err(|error| {
        PloyzctlCliError::InvalidValue {
            flag: "<operation_id>",
            message: error.to_string(),
        }
    })?;

    Ok(operation_id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusOutput {
    pub snapshot: OperationStatusSnapshot,
}

impl StatusOutput {
    #[must_use]
    pub fn new(snapshot: OperationStatusSnapshot) -> Self {
        Self { snapshot }
    }

    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "operation {}\nkind {}\n{}\nstate {}\nlast-event {}\nownership {}\n",
            operation_id(&self.snapshot.status).as_str(),
            operation_kind(&self.snapshot.status),
            operation_subject(&self.snapshot.status),
            operation_state(&self.snapshot.status),
            last_event_sequence(&self.snapshot.status).get(),
            ownership_status(&self.snapshot.ownership),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchOutput {
    pub events: Vec<ReplayedOperationEvent>,
}

impl WatchOutput {
    #[must_use]
    pub fn render(&self) -> String {
        let rendered = self
            .events
            .iter()
            .map(render_replayed_event)
            .collect::<Vec<_>>()
            .join("\n");

        if rendered.is_empty() {
            rendered
        } else {
            rendered + "\n"
        }
    }
}

fn operation_id(status: &OperationStatus) -> &OperationId {
    match status {
        OperationStatus::Deploy { id, .. }
        | OperationStatus::Cert { id, .. }
        | OperationStatus::MachineAdd { id, .. }
        | OperationStatus::Backup { id, .. } => id,
    }
}

const fn operation_kind(status: &OperationStatus) -> &'static str {
    match status {
        OperationStatus::Deploy { .. } => "deploy",
        OperationStatus::Cert { .. } => "cert",
        OperationStatus::MachineAdd { .. } => "machine-add",
        OperationStatus::Backup { .. } => "backup",
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
            node_id,
            name,
            gateway,
            ..
        } => format!(
            "node {} name {} gateway {}",
            node_id.as_str(),
            name.as_str(),
            first_node_gateway(*gateway)
        ),
        OperationStatus::Backup { .. } => "backup control-plane".to_owned(),
    }
}

fn operation_state(status: &OperationStatus) -> String {
    match status {
        OperationStatus::Deploy { state, .. } => deploy_state(state).to_owned(),
        OperationStatus::Cert { state, .. } => cert_state(state).to_owned(),
        OperationStatus::MachineAdd { state, .. } => machine_add_state(state).to_owned(),
        OperationStatus::Backup { state, .. } => backup_state(state),
    }
}

const fn last_event_sequence(status: &OperationStatus) -> EventSequence {
    match status {
        OperationStatus::Deploy {
            last_event_sequence,
            ..
        }
        | OperationStatus::Cert {
            last_event_sequence,
            ..
        }
        | OperationStatus::MachineAdd {
            last_event_sequence,
            ..
        }
        | OperationStatus::Backup {
            last_event_sequence,
            ..
        } => *last_event_sequence,
    }
}

fn ownership_status(ownership: &OperationOwnershipStatus) -> String {
    match ownership {
        OperationOwnershipStatus::Unclaimed => "unclaimed".to_owned(),
        OperationOwnershipStatus::Owned { lease } => format!(
            "owned {} expires-at {}",
            lease.owner_id.as_str(),
            lease.expires_at.unix_seconds()
        ),
        OperationOwnershipStatus::Expired { lease } => format!(
            "expired {} expired-at {}",
            lease.owner_id.as_str(),
            lease.expires_at.unix_seconds()
        ),
    }
}

const fn first_node_gateway(gateway: FirstNodeGateway) -> &'static str {
    match gateway {
        FirstNodeGateway::Install => "install",
        FirstNodeGateway::Skip => "skip",
    }
}

const fn deploy_state(state: &DeployOperationState) -> &'static str {
    match state {
        DeployOperationState::Accepted => "accepted",
        DeployOperationState::Planning => "planning",
        DeployOperationState::Running { stage } => deploy_running_stage(*stage),
        DeployOperationState::Completed => "completed",
        DeployOperationState::Failed { .. } => "failed",
        DeployOperationState::Cancelled { .. } => "cancelled",
    }
}

const fn deploy_running_stage(stage: DeployRunningStage) -> &'static str {
    match stage {
        DeployRunningStage::PreparingWireGuardEbpf => "running:preparing-wireguard-ebpf",
        DeployRunningStage::StartingContainers => "running:starting-containers",
        DeployRunningStage::WaitingForHealth => "running:waiting-for-health",
        DeployRunningStage::RouteCutover => "running:route-cutover",
        DeployRunningStage::ActiveServiceCommit => "running:active-service-commit",
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

fn backup_state(state: &BackupOperationState) -> String {
    match state {
        BackupOperationState::Accepted => "accepted".to_owned(),
        BackupOperationState::Running { stage } => backup_running_stage(stage),
        BackupOperationState::Completed { .. } => "completed".to_owned(),
        BackupOperationState::Failed { .. } => "failed".to_owned(),
        BackupOperationState::Cancelled { .. } => "cancelled".to_owned(),
    }
}

fn backup_running_stage(stage: &BackupRunningStage) -> String {
    match stage {
        BackupRunningStage::SnapshottingControlPlane => {
            "running:snapshotting-control-plane".to_owned()
        }
        BackupRunningStage::WritingManifest { artifact } => {
            format!("running:writing-manifest:{}", artifact.object_name)
        }
    }
}

fn render_replayed_event(event: &ReplayedOperationEvent) -> String {
    format!(
        "{} {}",
        event.sequence.get(),
        operation_event_label(&event.event)
    )
}

fn operation_event_label(event: &OperationEvent) -> &'static str {
    match event {
        OperationEvent::DeploySubmitted { .. } => "deploy.submitted",
        OperationEvent::DeployPlanningStarted { .. } => "deploy.planning",
        OperationEvent::DeployPlanCreated { .. } => "deploy.plan_created",
        OperationEvent::DeployRunning { .. } => "deploy.running",
        OperationEvent::DeployContainerStarted { .. } => "deploy.container_started",
        OperationEvent::DeployHealthCheckStarted { .. } => "deploy.health_check_started",
        OperationEvent::DeployWireGuardEbpfPrepared { .. } => "deploy.wireguard_ebpf_prepared",
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
        OperationEvent::MachineAddCompleted { .. } => "machine.add.completed",
        OperationEvent::MachineAddFailed { .. } => "machine.add.failed",
        OperationEvent::BackupCreateSubmitted { .. } => "backup.submitted",
        OperationEvent::BackupRunning { .. } => "backup.running",
        OperationEvent::BackupCompleted { .. } => "backup.completed",
        OperationEvent::BackupFailed { .. } => "backup.failed",
        OperationEvent::Cancelled { .. } => "cancelled",
    }
}
