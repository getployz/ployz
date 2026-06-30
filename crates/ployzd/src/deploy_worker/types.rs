use ployz_core::dataplane::DataplanePrepareRequest;
use ployz_core::deploy::{
    DeployCleanupContainer, DeployPlan, DeployRequest, DeployServiceRequest, ExistingServiceReplica,
};
use ployz_core::ids::{ContainerId, MachineId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::ops::{
    DeployCompletionOutcome, FailureMessage, OperatorHint, RetainedArtifact, RoutePort,
};
use ployz_core::state::{
    ActiveRouteCommitRequest, ActiveServiceCommitRequest, ExpectedActiveService,
};
use std::time::Duration;

const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionCommand {
    pub(super) operation_id: OperationId,
    pub(super) request: DeployRequest,
    pub(super) services: Vec<DeployServiceExecutionCommand>,
    pub(super) namespace_cleanup_candidates: Vec<DeployCleanupContainer>,
    pub(super) dataplane_machines: Vec<MachineId>,
    pub(super) step_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployServiceExecutionCommand {
    pub(super) request: DeployServiceRequest,
    pub(super) expected_active: ExpectedActiveService,
    pub(super) route_commit: Option<ActiveRouteCommitRequest>,
    pub(super) eligible_machines: Vec<MachineId>,
    pub(super) existing_replicas: Vec<ExistingServiceReplica>,
    pub(super) cleanup_candidates: Vec<DeployCleanupContainer>,
}

impl DeployExecutionCommand {
    #[must_use]
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }

    #[must_use]
    pub fn services(&self) -> &[DeployServiceExecutionCommand] {
        &self.services
    }

    #[must_use]
    pub fn namespace_cleanup_candidates(&self) -> &[DeployCleanupContainer] {
        &self.namespace_cleanup_candidates
    }

    #[must_use]
    pub fn expected_active(&self) -> &ExpectedActiveService {
        self.first_service().expected_active()
    }

    #[must_use]
    pub fn existing_replicas(&self) -> &[ExistingServiceReplica] {
        self.first_service().existing_replicas()
    }

    #[must_use]
    pub fn cleanup_candidates(&self) -> &[DeployCleanupContainer] {
        self.first_service().cleanup_candidates()
    }

    #[must_use]
    pub fn eligible_machines(&self) -> &[MachineId] {
        self.first_service().eligible_machines()
    }

    #[must_use]
    pub fn dataplane_machines(&self) -> &[MachineId] {
        &self.dataplane_machines
    }

    #[must_use]
    pub fn with_step_timeout(mut self, step_timeout: Duration) -> Self {
        self.step_timeout = step_timeout;
        self
    }

    #[must_use]
    pub fn step_timeout(&self) -> Duration {
        if self.step_timeout.is_zero() {
            DEFAULT_STEP_TIMEOUT
        } else {
            self.step_timeout
        }
    }

    #[must_use]
    pub fn dataplane_prepare_request(&self, plan: &DeployPlan) -> DataplanePrepareRequest {
        DataplanePrepareRequest::for_deploy_plan(
            self.operation_id.clone(),
            plan,
            &self.dataplane_machines,
        )
    }

    fn first_service(&self) -> &DeployServiceExecutionCommand {
        self.services
            .first()
            .expect("single-service command accessor requires at least one service")
    }
}

impl DeployServiceExecutionCommand {
    #[must_use]
    pub fn expected_active(&self) -> &ExpectedActiveService {
        &self.expected_active
    }

    #[must_use]
    pub fn existing_replicas(&self) -> &[ExistingServiceReplica] {
        &self.existing_replicas
    }

    #[must_use]
    pub fn cleanup_candidates(&self) -> &[DeployCleanupContainer] {
        &self.cleanup_candidates
    }

    #[must_use]
    pub fn eligible_machines(&self) -> &[MachineId] {
        &self.eligible_machines
    }

    #[must_use]
    pub fn active_service_commit_request(&self) -> ActiveServiceCommitRequest {
        ActiveServiceCommitRequest {
            service_id: self.request.service_id.clone(),
            expected_current: self.expected_active.clone(),
            target_revision: self.request.target_revision.clone(),
        }
    }

    #[must_use]
    pub fn active_route_commit_request(&self) -> Option<ActiveRouteCommitRequest> {
        self.route_commit.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionOutcome {
    pub namespace_id: ployz_core::ids::NamespaceId,
    pub target_revision: RevisionId,
    pub containers: Vec<DeployContainer>,
    pub cleanup: Vec<DeployCleanupResult>,
    pub terminal_event: DeployTerminalEvent,
}

impl DeployExecutionOutcome {
    #[must_use]
    pub fn completion_outcome(&self) -> DeployCompletionOutcome {
        DeployCleanupResult::completion_outcome(&self.cleanup)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployCleanupResult {
    Removed(DeployCleanupContainer),
    Failed {
        target: DeployCleanupContainer,
        message: FailureMessage,
    },
}

impl DeployCleanupResult {
    pub(super) fn completion_outcome(cleanup: &[Self]) -> DeployCompletionOutcome {
        if cleanup
            .iter()
            .any(|result| matches!(result, Self::Failed { .. }))
        {
            DeployCompletionOutcome::CompletedWithWarnings
        } else {
            DeployCompletionOutcome::Completed
        }
    }

    pub(super) fn has_failure(cleanup: &[Self]) -> bool {
        cleanup
            .iter()
            .any(|result| matches!(result, Self::Failed { .. }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployTerminalEvent {
    Recorded,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployContainer {
    pub service_id: ServiceId,
    pub revision_id: RevisionId,
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub step_id: StepId,
    pub required_endpoint_port: Option<RoutePort>,
}

impl DeployContainer {
    pub(super) fn retained_artifact(&self) -> RetainedArtifact {
        RetainedArtifact::StartedContainer {
            machine_id: self.machine_id.clone(),
            container_id: self.container_id.clone(),
            log_hint: OperatorHint::try_new(format!("ployz logs {}", self.container_id.as_str()))
                .expect("generated log hint is non-empty"),
        }
    }
}

pub struct DeployExecutionPorts<'a, R, D, N, H, C, A> {
    pub recorder: &'a mut R,
    pub dataplane: &'a mut D,
    pub machine_runtime: &'a mut N,
    pub health_checker: &'a mut H,
    pub route_state: &'a mut C,
    pub active_state: &'a mut A,
}
