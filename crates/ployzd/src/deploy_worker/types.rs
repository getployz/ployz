use ployz_core::deploy::{DeployRequest, ExistingServiceReplica, ImageReference};
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId};
use ployz_core::ops::{OperatorHint, RetainedArtifact};
use ployz_core::state::{ActiveServiceCommitRequest, ExpectedActiveService};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::docker::labels::ManagedContainerLabels;

const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionCommand {
    pub(super) operation_id: OperationId,
    pub(super) request: DeployRequest,
    pub(super) expected_active: ExpectedActiveService,
    pub(super) eligible_nodes: Vec<NodeId>,
    pub(super) existing_replicas: Vec<ExistingServiceReplica>,
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
    pub fn eligible_nodes(&self) -> &[NodeId] {
        &self.eligible_nodes
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeRunContainerRequest {
    pub node_id: NodeId,
    pub image: ImageReference,
    pub labels: ManagedContainerLabels,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
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
