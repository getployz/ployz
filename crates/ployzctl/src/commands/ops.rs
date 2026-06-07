use ployz_core::ops::{OperationEvent, ReplayedOperationEvent};

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
        OperationEvent::Cancelled { .. } => "cancelled",
    }
}
