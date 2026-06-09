//! Deploy policy and planning models.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::num::NonZeroU16;

use crate::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use crate::node::{ManagedContainerKind, NodeContainerObservationSnapshot};
use crate::ops::{RoutePort, RouteTarget};
use crate::state::{
    ActiveRouteCommitRequest, ActiveRouteState, ActiveServiceState, ExpectedActiveRoute,
    ExpectedActiveRouteRevision, ExpectedActiveService,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployRequest {
    pub service_id: ServiceId,
    pub target_revision: RevisionId,
    pub image: ImageReference,
    pub replicas: ReplicaCount,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<DeployRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployRoute {
    pub target: RouteTarget,
    pub endpoint_port: RoutePort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPlanningInput {
    pub request: DeployRequest,
    pub eligible_nodes: Vec<NodeId>,
    pub existing_replicas: Vec<ExistingServiceReplica>,
    pub cleanup_candidates: Vec<DeployCleanupContainer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingServiceReplica {
    pub node_id: NodeId,
    pub container_id: ContainerId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployCleanupContainer {
    pub node_id: NodeId,
    pub container_id: ContainerId,
    pub service_id: ServiceId,
    pub revision_id: RevisionId,
    pub operation_id: OperationId,
    pub step_id: StepId,
    pub kind: ManagedContainerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_port: Option<RoutePort>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPreparationInput {
    pub request: DeployRequest,
    pub active_service: Option<ActiveServiceState>,
    pub active_route: Option<ActiveRouteState>,
    pub eligible_nodes: Vec<NodeId>,
    pub observed_nodes: Vec<NodeContainerObservationSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedDeploy {
    pub request: DeployRequest,
    pub expected_active: ExpectedActiveService,
    pub route_commit: Option<ActiveRouteCommitRequest>,
    pub eligible_nodes: Vec<NodeId>,
    pub existing_replicas: Vec<ExistingServiceReplica>,
    pub cleanup_candidates: Vec<DeployCleanupContainer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployPlan {
    pub service_id: ServiceId,
    pub target_revision: RevisionId,
    pub steps: Vec<DeployPlanStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup_containers: Vec<DeployCleanupContainer>,
}

impl DeployPlan {
    #[must_use]
    pub fn target_nodes(&self) -> Vec<NodeId> {
        let mut nodes = self
            .steps
            .iter()
            .map(|step| match step {
                DeployPlanStep::UseExistingContainer { node_id, .. }
                | DeployPlanStep::RunContainer { node_id, .. } => node_id.clone(),
            })
            .collect::<Vec<_>>();
        nodes.sort();
        nodes.dedup();
        nodes
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployPlanStep {
    UseExistingContainer {
        node_id: NodeId,
        container_id: ContainerId,
        slot: ReplicaSlot,
    },
    RunContainer {
        node_id: NodeId,
        slot: ReplicaSlot,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "SafeInteger<\"ReplicaSlot\">"))]
#[serde(try_from = "u16", into = "u16")]
pub struct ReplicaSlot(u16);

impl ReplicaSlot {
    pub fn try_new(value: u16) -> Result<Self, ReplicaSlotError> {
        if value == 0 {
            return Err(ReplicaSlotError::Zero);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for ReplicaSlot {
    type Error = ReplicaSlotError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ReplicaSlot> for u16 {
    fn from(value: ReplicaSlot) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicaSlotError {
    Zero,
}

impl fmt::Display for ReplicaSlotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("replica slot must be greater than zero"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployPlanError {
    NoEligibleNodes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployPreparationError {
    ActiveServiceMismatch {
        expected_service_id: ServiceId,
        actual_service_id: ServiceId,
    },
    ActiveRouteMismatch {
        expected_route: RouteTarget,
        actual_route: RouteTarget,
    },
}

impl fmt::Display for DeployPreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveServiceMismatch {
                expected_service_id,
                actual_service_id,
            } => write!(
                formatter,
                "active service state belongs to {}, not {}",
                actual_service_id.as_str(),
                expected_service_id.as_str()
            ),
            Self::ActiveRouteMismatch {
                expected_route,
                actual_route,
            } => write!(
                formatter,
                "active route state belongs to {:?}, not {:?}",
                actual_route, expected_route
            ),
        }
    }
}

pub fn prepare_deploy(
    input: DeployPreparationInput,
) -> Result<PreparedDeploy, DeployPreparationError> {
    let expected_active = expected_active_service(&input.request, input.active_service)?;
    let route_commit = active_route_commit_request(&input.request, input.active_route)?;
    let existing_replicas = existing_replicas(&input.request, &input.observed_nodes);
    let cleanup_candidates = cleanup_candidates(&input.request, &input.observed_nodes);

    Ok(PreparedDeploy {
        request: input.request,
        expected_active,
        route_commit,
        eligible_nodes: input.eligible_nodes,
        existing_replicas,
        cleanup_candidates,
    })
}

fn expected_active_service(
    request: &DeployRequest,
    active_service: Option<ActiveServiceState>,
) -> Result<ExpectedActiveService, DeployPreparationError> {
    let Some(active_service) = active_service else {
        return Ok(ExpectedActiveService::Absent);
    };

    if active_service.service_id != request.service_id {
        return Err(DeployPreparationError::ActiveServiceMismatch {
            expected_service_id: request.service_id.clone(),
            actual_service_id: active_service.service_id,
        });
    }

    Ok(ExpectedActiveService::Revision(
        active_service.active_revision,
    ))
}

fn existing_replicas(
    request: &DeployRequest,
    observed_nodes: &[NodeContainerObservationSnapshot],
) -> Vec<ExistingServiceReplica> {
    observed_nodes
        .iter()
        .flat_map(NodeContainerObservationSnapshot::containers)
        .filter(|container| {
            container.is_running_service_revision(&request.service_id, &request.target_revision)
                && reusable_for_route(container, request.route.as_ref())
        })
        .map(|container| ExistingServiceReplica {
            node_id: container.node_id.clone(),
            container_id: container.container_id.clone(),
        })
        .collect()
}

fn cleanup_candidates(
    request: &DeployRequest,
    observed_nodes: &[NodeContainerObservationSnapshot],
) -> Vec<DeployCleanupContainer> {
    observed_nodes
        .iter()
        .flat_map(NodeContainerObservationSnapshot::containers)
        .filter(|container| {
            container.is_running_service() && container.service_id == request.service_id
        })
        .map(|container| DeployCleanupContainer {
            node_id: container.node_id.clone(),
            container_id: container.container_id.clone(),
            service_id: container.service_id.clone(),
            revision_id: container.revision_id.clone(),
            operation_id: container.operation_id.clone(),
            step_id: container.step_id.clone(),
            kind: container.kind,
            endpoint_port: container
                .running_service_endpoint()
                .map(|endpoint| endpoint.port),
        })
        .collect()
}

fn reusable_for_route(
    container: &crate::node::ManagedContainerObservation,
    route: Option<&DeployRoute>,
) -> bool {
    let Some(route) = route else {
        return true;
    };

    container
        .running_service_endpoint()
        .is_some_and(|endpoint| endpoint.port == route.endpoint_port)
}

fn active_route_commit_request(
    request: &DeployRequest,
    active_route: Option<ActiveRouteState>,
) -> Result<Option<ActiveRouteCommitRequest>, DeployPreparationError> {
    let Some(route) = request.route.clone() else {
        return Ok(None);
    };
    let target = route.target;
    let endpoint_port = route.endpoint_port;

    let expected_current = match active_route {
        Some(route) => {
            if route.target != target {
                return Err(DeployPreparationError::ActiveRouteMismatch {
                    expected_route: target,
                    actual_route: route.target,
                });
            }
            ExpectedActiveRoute::ServiceRevision(ExpectedActiveRouteRevision {
                service_id: route.service_id,
                revision_id: route.revision_id,
                endpoint_port: route.endpoint_port,
            })
        }
        None => ExpectedActiveRoute::Absent,
    };

    Ok(Some(ActiveRouteCommitRequest {
        target,
        endpoint_port,
        expected_current,
        service_id: request.service_id.clone(),
        revision_id: request.target_revision.clone(),
    }))
}

pub fn plan_service_deploy(input: DeployPlanningInput) -> Result<DeployPlan, DeployPlanError> {
    let target_replicas = usize::from(input.request.replicas.get());
    let mut existing_replicas = input.existing_replicas;
    existing_replicas.sort_by(|left, right| {
        left.node_id
            .cmp(&right.node_id)
            .then_with(|| left.container_id.cmp(&right.container_id))
    });
    existing_replicas.dedup_by(|left, right| {
        left.node_id == right.node_id && left.container_id == right.container_id
    });
    let mut steps = existing_replicas
        .into_iter()
        .take(target_replicas)
        .enumerate()
        .map(|(index, replica)| DeployPlanStep::UseExistingContainer {
            node_id: replica.node_id,
            container_id: replica.container_id,
            slot: ReplicaSlot((index + 1) as u16),
        })
        .collect::<Vec<_>>();
    let missing_replicas = target_replicas.saturating_sub(steps.len());
    if missing_replicas > 0 && input.eligible_nodes.is_empty() {
        return Err(DeployPlanError::NoEligibleNodes);
    }

    let existing_replicas = steps.len();
    steps.extend(
        input
            .eligible_nodes
            .iter()
            .cycle()
            .take(missing_replicas)
            .enumerate()
            .map(|(index, node_id)| {
                let slot = ReplicaSlot((existing_replicas + index + 1) as u16);
                DeployPlanStep::RunContainer {
                    node_id: node_id.clone(),
                    slot,
                }
            }),
    );
    let selected_containers = steps
        .iter()
        .filter_map(|step| match step {
            DeployPlanStep::UseExistingContainer { container_id, .. } => Some(container_id),
            DeployPlanStep::RunContainer { .. } => None,
        })
        .collect::<Vec<_>>();
    let mut cleanup_containers = input.cleanup_candidates;
    cleanup_containers.retain(|candidate| !selected_containers.contains(&&candidate.container_id));
    cleanup_containers.sort_by(|left, right| {
        left.node_id
            .cmp(&right.node_id)
            .then_with(|| left.container_id.cmp(&right.container_id))
    });
    cleanup_containers.dedup_by(|left, right| {
        left.node_id == right.node_id && left.container_id == right.container_id
    });

    Ok(DeployPlan {
        service_id: input.request.service_id,
        target_revision: input.request.target_revision,
        steps,
        cleanup_containers,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"ImageReference\">"))]
#[serde(try_from = "String", into = "String")]
pub struct ImageReference(String);

impl ImageReference {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ImageReferenceError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ImageReferenceError::Empty);
        }

        if value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(ImageReferenceError::InvalidCharacter { value });
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ImageReference {
    type Error = ImageReferenceError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ImageReference> for String {
    fn from(value: ImageReference) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageReferenceError {
    Empty,
    InvalidCharacter { value: String },
}

impl fmt::Display for ImageReferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("image reference is empty"),
            Self::InvalidCharacter { value } => {
                write!(
                    formatter,
                    "image reference contains invalid characters: {value}"
                )
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "SafeInteger<\"ReplicaCount\">"))]
#[serde(try_from = "u16", into = "u16")]
pub struct ReplicaCount(NonZeroU16);

impl ReplicaCount {
    pub fn try_new(value: u16) -> Result<Self, ReplicaCountError> {
        let Some(value) = NonZeroU16::new(value) else {
            return Err(ReplicaCountError::Zero);
        };

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl TryFrom<u16> for ReplicaCount {
    type Error = ReplicaCountError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ReplicaCount> for u16 {
    fn from(value: ReplicaCount) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicaCountError {
    Zero,
}

impl fmt::Display for ReplicaCountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("replica count must be greater than zero"),
        }
    }
}
