use ployz_core::dataplane::{WireGuardEbpfPrepareRequest, WireGuardPeerEndpoint};
use ployz_core::deploy::{
    DeployCleanupContainer, DeployPlan, DeployRequest, ExistingServiceReplica,
};
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId};
use ployz_core::ops::{
    DeployCompletionOutcome, FailureMessage, OperatorHint, RetainedArtifact, RoutePort,
};
use ployz_core::state::{
    ActiveRouteCommitRequest, ActiveServiceCommitRequest, ExpectedActiveService,
};
use std::time::Duration;

const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionCommand {
    pub(super) operation_id: OperationId,
    pub(super) request: DeployRequest,
    pub(super) expected_active: ExpectedActiveService,
    pub(super) route_commit: Option<ActiveRouteCommitRequest>,
    pub(super) eligible_nodes: Vec<NodeId>,
    pub(super) existing_replicas: Vec<ExistingServiceReplica>,
    pub(super) cleanup_candidates: Vec<DeployCleanupContainer>,
    pub(super) dataplane_nodes: Vec<NodeId>,
    pub(super) wireguard_peer_endpoints: Vec<WireGuardPeerEndpoint>,
    pub(super) step_timeout: Duration,
}

impl DeployExecutionCommand {
    #[must_use]
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }

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
    pub fn wireguard_peer_endpoints(&self) -> &[WireGuardPeerEndpoint] {
        &self.wireguard_peer_endpoints
    }

    #[must_use]
    pub fn eligible_nodes(&self) -> &[NodeId] {
        &self.eligible_nodes
    }

    #[must_use]
    pub fn dataplane_nodes(&self) -> &[NodeId] {
        &self.dataplane_nodes
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

    #[must_use]
    pub fn wireguard_ebpf_prepare_request(&self, plan: &DeployPlan) -> WireGuardEbpfPrepareRequest {
        WireGuardEbpfPrepareRequest::for_deploy_plan_with_dataplane_nodes_and_peer_endpoints(
            self.operation_id.clone(),
            plan,
            &self.dataplane_nodes,
            &self.wireguard_peer_endpoints,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionOutcome {
    pub service_id: ployz_core::ids::ServiceId,
    pub target_revision: RevisionId,
    pub containers: Vec<DeployContainer>,
    pub cleanup: Vec<DeployCleanupResult>,
    pub completion_outcome: DeployCompletionOutcome,
    pub terminal_event: DeployTerminalEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployCleanupResult {
    Removed(DeployCleanupContainer),
    Failed {
        target: DeployCleanupContainer,
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployTerminalEvent {
    Recorded,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployContainer {
    pub node_id: NodeId,
    pub container_id: ContainerId,
    pub required_endpoint_port: Option<RoutePort>,
}

impl DeployContainer {
    pub(super) fn retained_artifact(&self) -> RetainedArtifact {
        RetainedArtifact::StartedContainer {
            node_id: self.node_id.clone(),
            container_id: self.container_id.clone(),
            log_hint: OperatorHint::try_new(format!("ployz logs {}", self.container_id.as_str()))
                .expect("generated log hint is non-empty"),
        }
    }
}

pub struct DeployExecutionPorts<'a, R, D, N, H, C, A> {
    pub recorder: &'a mut R,
    pub wireguard_ebpf: &'a mut D,
    pub node_runtime: &'a mut N,
    pub health_checker: &'a mut H,
    pub route_state: &'a mut C,
    pub active_state: &'a mut A,
}
