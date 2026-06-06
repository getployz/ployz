use ployz_core::deploy::{DeployRequest, ImageReference};
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId};
use ployz_core::node::ManagedContainerObservation;
use ployz_core::ops::{OperatorHint, RetainedArtifact};
use ployz_core::state::{ActiveServiceCommitRequest, ExpectedActiveService};
use std::time::Duration;

use crate::docker::labels::ManagedContainerLabels;

const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionCommand {
    pub operation_id: OperationId,
    pub request: DeployRequest,
    pub expected_active: ExpectedActiveService,
    pub eligible_nodes: Vec<NodeId>,
    pub observed_containers: Vec<ManagedContainerObservation>,
    pub step_timeout: Duration,
}

impl DeployExecutionCommand {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionOutcome {
    pub service_id: ployz_core::ids::ServiceId,
    pub target_revision: RevisionId,
    pub containers: Vec<DeployContainer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployContainer {
    pub node_id: NodeId,
    pub container_id: ContainerId,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRunContainerRequest {
    pub node_id: NodeId,
    pub image: ImageReference,
    pub labels: ManagedContainerLabels,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeRunContainerOutcome {
    Created { container_id: ContainerId },
    Reused { container_id: ContainerId },
}

impl NodeRunContainerOutcome {
    #[must_use]
    pub fn container_id(&self) -> &ContainerId {
        match self {
            Self::Created { container_id } | Self::Reused { container_id } => container_id,
        }
    }
}

pub struct DeployExecutionPorts<'a, R, N, H, A> {
    pub recorder: &'a mut R,
    pub node_runtime: &'a mut N,
    pub health_checker: &'a mut H,
    pub active_state: &'a mut A,
}
