use ployz_core::ids::OperationId;
use ployz_core::ops::{
    EventSequence, MAX_OPERATION_EVENT_REPLAY_LIMIT, OperationEvent, OperationEventReplayLimit,
    OperationEventReplayRequest, ReplayedOperationEvent,
};

use crate::commands::PloyzctlCliError;

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

pub fn parse_ops_watch_command(args: &[String]) -> Result<OpsWatchCommand, PloyzctlCliError> {
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

    Ok(OpsWatchCommand { operation_id })
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
