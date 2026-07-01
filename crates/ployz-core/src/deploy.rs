//! Deploy policy and planning models.

use serde::{Deserialize, Serialize};
use std::num::NonZeroU16;

use crate::ids::{ContainerId, MachineId, NamespaceId, OperationId, RevisionId, ServiceId, StepId};
use crate::machine_runtime::{MachineContainerObservationSnapshot, ManagedContainerKind};
use crate::ops::{RoutePort, RouteTarget};
use crate::state::{
    ActiveRouteCommitRequest, ActiveRouteState, ActiveServiceState, ExpectedActiveRoute,
    ExpectedActiveRouteRevision, ExpectedActiveService,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployRequest {
    pub namespace_id: NamespaceId,
    pub target_revision: RevisionId,
    pub services: Vec<DeployServiceSpec>,
}

impl DeployRequest {
    #[must_use]
    pub fn primary_service(&self) -> Option<&DeployServiceSpec> {
        self.services.first()
    }

    #[must_use]
    pub fn primary_service_id(&self) -> Option<&ServiceId> {
        self.primary_service().map(|service| &service.service_id)
    }

    #[must_use]
    pub fn status_service_id(&self) -> ServiceId {
        self.primary_service_id().cloned().unwrap_or_else(|| {
            ServiceId::try_new(self.namespace_id.as_str().to_owned())
                .expect("namespace id is a valid service id fallback")
        })
    }

    #[must_use]
    pub fn service_requests(&self) -> Vec<DeployServiceRequest> {
        self.services
            .iter()
            .map(|service| DeployServiceRequest {
                namespace_id: self.namespace_id.clone(),
                service_id: service.service_id.clone(),
                target_revision: self.target_revision.clone(),
                image: service.image.clone(),
                replicas: service.replicas,
                route: service.route.clone(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployServiceSpec {
    pub service_id: ServiceId,
    pub image: ImageReference,
    pub replicas: ReplicaCount,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<DeployRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployServiceRequest {
    pub namespace_id: NamespaceId,
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
    pub request: DeployServiceRequest,
    pub eligible_machines: Vec<MachineId>,
    pub existing_replicas: Vec<ExistingServiceReplica>,
    pub cleanup_candidates: Vec<DeployCleanupContainer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingServiceReplica {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployCleanupContainer {
    pub machine_id: MachineId,
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
    pub request: DeployServiceRequest,
    pub active_service: Option<ActiveServiceState>,
    pub active_route: Option<ActiveRouteState>,
    pub eligible_machines: Vec<MachineId>,
    pub observed_machines: Vec<MachineContainerObservationSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedDeploy {
    pub request: DeployServiceRequest,
    pub expected_active: ExpectedActiveService,
    pub route_commit: Option<ActiveRouteCommitRequest>,
    pub eligible_machines: Vec<MachineId>,
    pub existing_replicas: Vec<ExistingServiceReplica>,
    pub cleanup_candidates: Vec<DeployCleanupContainer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployPlan {
    pub namespace_id: NamespaceId,
    pub target_revision: RevisionId,
    pub services: Vec<DeployServicePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup_containers: Vec<DeployCleanupContainer>,
}

impl DeployPlan {
    #[must_use]
    pub fn target_machines(&self) -> Vec<MachineId> {
        let mut machines = self
            .services
            .iter()
            .flat_map(|service| service.steps.iter())
            .map(|step| match step {
                DeployPlanStep::UseExistingContainer { machine_id, .. }
                | DeployPlanStep::RunContainer { machine_id, .. } => machine_id.clone(),
            })
            .collect::<Vec<_>>();
        machines.sort();
        machines.dedup();
        machines
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployServicePlan {
    pub service_id: ServiceId,
    pub steps: Vec<DeployPlanStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployPlanStep {
    UseExistingContainer {
        machine_id: MachineId,
        container_id: ContainerId,
        slot: ReplicaSlot,
    },
    RunContainer {
        machine_id: MachineId,
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplicaSlotError {
    #[error("replica slot must be greater than zero")]
    Zero,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployPlanError {
    NoEligibleMachines,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployPreparationError {
    #[error(
        "active service state belongs to {}, not {}",
        actual_service_id.as_str(),
        expected_service_id.as_str()
    )]
    ActiveServiceMismatch {
        expected_service_id: ServiceId,
        actual_service_id: ServiceId,
    },
    #[error("active route state belongs to {actual_route:?}, not {expected_route:?}")]
    ActiveRouteMismatch {
        expected_route: RouteTarget,
        actual_route: RouteTarget,
    },
}

pub fn prepare_deploy(
    input: DeployPreparationInput,
) -> Result<PreparedDeploy, DeployPreparationError> {
    let expected_active = expected_active_service(&input.request, input.active_service)?;
    let route_commit = active_route_commit_request(&input.request, input.active_route)?;
    let existing_replicas = existing_replicas(&input.request, &input.observed_machines);
    let cleanup_candidates = cleanup_candidates(&input.request, &input.observed_machines);

    Ok(PreparedDeploy {
        request: input.request,
        expected_active,
        route_commit,
        eligible_machines: input.eligible_machines,
        existing_replicas,
        cleanup_candidates,
    })
}

fn expected_active_service(
    request: &DeployServiceRequest,
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
    request: &DeployServiceRequest,
    observed_machines: &[MachineContainerObservationSnapshot],
) -> Vec<ExistingServiceReplica> {
    observed_machines
        .iter()
        .flat_map(MachineContainerObservationSnapshot::containers)
        .filter(|container| {
            container.is_running_service_revision(&request.service_id, &request.target_revision)
                && reusable_for_route(container, request.route.as_ref())
        })
        .map(|container| ExistingServiceReplica {
            machine_id: container.machine_id.clone(),
            container_id: container.container_id.clone(),
        })
        .collect()
}

fn cleanup_candidates(
    request: &DeployServiceRequest,
    observed_machines: &[MachineContainerObservationSnapshot],
) -> Vec<DeployCleanupContainer> {
    observed_machines
        .iter()
        .flat_map(MachineContainerObservationSnapshot::containers)
        .filter(|container| {
            container.is_running_service() && container.service_id == request.service_id
        })
        .map(|container| DeployCleanupContainer {
            machine_id: container.machine_id.clone(),
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
    container: &crate::machine_runtime::ManagedContainerObservation,
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
    request: &DeployServiceRequest,
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
        namespace_id: request.namespace_id.clone(),
        target,
        endpoint_port,
        expected_current,
        service_id: request.service_id.clone(),
        revision_id: request.target_revision.clone(),
    }))
}

pub fn plan_service_deploy(input: DeployPlanningInput) -> Result<DeployPlan, DeployPlanError> {
    let service_plan = plan_deploy_service(input)?;
    let target_revision = service_plan.target_revision;
    let cleanup_containers = service_plan.cleanup_containers;
    Ok(DeployPlan {
        namespace_id: NamespaceId::try_new(service_plan.service_id.as_str().to_owned())
            .expect("service id is a valid namespace id"),
        target_revision,
        services: vec![DeployServicePlan {
            service_id: service_plan.service_id,
            steps: service_plan.steps,
        }],
        cleanup_containers,
    })
}

pub fn plan_namespace_deploy(
    namespace_id: NamespaceId,
    target_revision: RevisionId,
    services: Vec<DeployPlanningInput>,
    cleanup_containers: Vec<DeployCleanupContainer>,
) -> Result<DeployPlan, DeployPlanError> {
    let mut service_plans = Vec::new();
    let mut cleanup_containers = cleanup_containers;
    for input in services {
        let plan = plan_deploy_service(input)?;
        service_plans.push(DeployServicePlan {
            service_id: plan.service_id,
            steps: plan.steps,
        });
        cleanup_containers.extend(plan.cleanup_containers);
    }

    Ok(DeployPlan {
        namespace_id,
        target_revision,
        services: service_plans,
        cleanup_containers,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploySingleServicePlan {
    pub service_id: ServiceId,
    pub target_revision: RevisionId,
    pub steps: Vec<DeployPlanStep>,
    pub cleanup_containers: Vec<DeployCleanupContainer>,
}

fn plan_deploy_service(
    input: DeployPlanningInput,
) -> Result<DeploySingleServicePlan, DeployPlanError> {
    let target_replicas = usize::from(input.request.replicas.get());
    let mut existing_replicas = input.existing_replicas;
    existing_replicas.sort_by(|left, right| {
        left.machine_id
            .cmp(&right.machine_id)
            .then_with(|| left.container_id.cmp(&right.container_id))
    });
    existing_replicas.dedup_by(|left, right| {
        left.machine_id == right.machine_id && left.container_id == right.container_id
    });
    let mut steps = existing_replicas
        .into_iter()
        .take(target_replicas)
        .enumerate()
        .map(|(index, replica)| DeployPlanStep::UseExistingContainer {
            machine_id: replica.machine_id,
            container_id: replica.container_id,
            slot: ReplicaSlot((index + 1) as u16),
        })
        .collect::<Vec<_>>();
    let missing_replicas = target_replicas.saturating_sub(steps.len());
    if missing_replicas > 0 && input.eligible_machines.is_empty() {
        return Err(DeployPlanError::NoEligibleMachines);
    }

    let existing_replicas = steps.len();
    steps.extend(
        input
            .eligible_machines
            .iter()
            .cycle()
            .take(missing_replicas)
            .enumerate()
            .map(|(index, machine_id)| {
                let slot = ReplicaSlot((existing_replicas + index + 1) as u16);
                DeployPlanStep::RunContainer {
                    machine_id: machine_id.clone(),
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
        left.machine_id
            .cmp(&right.machine_id)
            .then_with(|| left.container_id.cmp(&right.container_id))
    });
    cleanup_containers.dedup_by(|left, right| {
        left.machine_id == right.machine_id && left.container_id == right.container_id
    });

    Ok(DeploySingleServicePlan {
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageReferenceError {
    #[error("image reference is empty")]
    Empty,
    #[error("image reference contains invalid characters: {value}")]
    InvalidCharacter { value: String },
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplicaCountError {
    #[error("replica count must be greater than zero")]
    Zero,
}
