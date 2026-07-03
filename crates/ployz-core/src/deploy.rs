//! Deploy policy and planning models.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::num::NonZeroU16;

use crate::ids::{
    ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, NamespaceRevisionId,
    OperationId, ServiceId, StepId,
};
use crate::machine_runtime::{MachineContainerObservationSnapshot, ManagedContainerKind};
use crate::ops::{RoutePort, RouteTarget};
use crate::state::{RouteBindingState, ServingTargetEntry};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployRequest {
    pub namespace_id: NamespaceId,
    pub namespace_revision_id: NamespaceRevisionId,
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
                namespace_revision_id: self.namespace_revision_id.clone(),
                namespace_revision_entry_id: service.namespace_revision_entry_id(),
                image: service.image.clone(),
                replicas: service.replicas,
                routes: service.routes.clone(),
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<DeployRoute>,
}

impl DeployServiceSpec {
    const NAMESPACE_REVISION_ENTRY_ENCODING_VERSION: &'static str =
        "ployz.namespace_revision_entry.v1";

    #[must_use]
    pub fn namespace_revision_entry_id(&self) -> NamespaceRevisionEntryId {
        namespace_revision_entry_id_for(&self.service_id, &self.image)
    }
}

#[must_use]
pub fn namespace_revision_entry_id_for(
    service_id: &ServiceId,
    image: &ImageReference,
) -> NamespaceRevisionEntryId {
    let mut hasher = Sha256::new();
    hasher.update(DeployServiceSpec::NAMESPACE_REVISION_ENTRY_ENCODING_VERSION);
    hasher.update(b"\nservice_id=");
    hasher.update(service_id.as_str());
    hasher.update(b"\nimage=");
    hasher.update(image.as_str());
    hasher.update(b"\n");
    let digest = hasher.finalize();
    NamespaceRevisionEntryId::try_new(format!("{digest:x}"))
        .expect("sha256 hex digest is a subject token")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployServiceRequest {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_id: NamespaceRevisionId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub image: ImageReference,
    pub replicas: ReplicaCount,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<DeployRoute>,
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
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub operation_id: OperationId,
    pub step_id: StepId,
    pub kind: ManagedContainerKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPreparationInput {
    pub request: DeployServiceRequest,
    pub serving_target_entry: Option<ServingTargetEntry>,
    pub eligible_machines: Vec<MachineId>,
    pub observed_machines: Vec<MachineContainerObservationSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedDeploy {
    pub request: DeployServiceRequest,
    pub route_commits: Vec<RouteBindingState>,
    pub eligible_machines: Vec<MachineId>,
    pub existing_replicas: Vec<ExistingServiceReplica>,
    pub cleanup_candidates: Vec<DeployCleanupContainer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployPlan {
    pub namespace_id: NamespaceId,
    pub namespace_revision_id: NamespaceRevisionId,
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

#[must_use]
pub fn prepare_deploy(input: DeployPreparationInput) -> PreparedDeploy {
    let route_commits = route_binding_commits(&input.request);
    let existing_replicas = existing_replicas(&input.request, &input.observed_machines);
    let cleanup_candidates = cleanup_candidates(&input.request, &input.observed_machines);

    PreparedDeploy {
        request: input.request,
        route_commits,
        eligible_machines: input.eligible_machines,
        existing_replicas,
        cleanup_candidates,
    }
}

/// Route binding removals are a namespace-level decision: the manifest is
/// the full desired state, so any stored binding whose target no service in
/// the manifest declares is detached - including bindings owned by services
/// the manifest omits entirely.
#[must_use]
pub fn namespace_route_binding_removals(
    namespace_id: &NamespaceId,
    declared_targets: &[RouteTarget],
    stored_bindings: &[RouteBindingState],
) -> Vec<RouteTarget> {
    stored_bindings
        .iter()
        .filter(|binding| {
            binding.namespace_id == *namespace_id && !declared_targets.contains(&binding.target)
        })
        .map(|binding| binding.target.clone())
        .collect()
}

/// Serving target entries of services the manifest omits are unpublished:
/// manifest omission removes a service from the namespace, so its entry
/// must not stay serveable in stored state.
#[must_use]
pub fn namespace_serving_target_removals(
    namespace_id: &NamespaceId,
    declared_services: &[ServiceId],
    stored_entries: &[ServingTargetEntry],
) -> Vec<ServingTargetEntry> {
    stored_entries
        .iter()
        .filter(|entry| {
            entry.namespace_id == *namespace_id && !declared_services.contains(&entry.service_id)
        })
        .cloned()
        .collect()
}

fn existing_replicas(
    request: &DeployServiceRequest,
    observed_machines: &[MachineContainerObservationSnapshot],
) -> Vec<ExistingServiceReplica> {
    observed_machines
        .iter()
        .flat_map(MachineContainerObservationSnapshot::containers)
        .filter(|container| {
            container.is_running_service_entry(
                &request.service_id,
                &request.namespace_revision_entry_id,
            )
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
            namespace_revision_entry_id: container.namespace_revision_entry_id.clone(),
            operation_id: container.operation_id.clone(),
            step_id: container.step_id.clone(),
            kind: container.kind,
        })
        .collect()
}

fn route_binding_commits(request: &DeployServiceRequest) -> Vec<RouteBindingState> {
    request
        .routes
        .iter()
        .cloned()
        .map(|route| RouteBindingState {
            namespace_id: request.namespace_id.clone(),
            target: route.target,
            endpoint_port: route.endpoint_port,
            service_id: request.service_id.clone(),
        })
        .collect()
}

pub fn plan_service_deploy(input: DeployPlanningInput) -> Result<DeployPlan, DeployPlanError> {
    let service_plan = plan_deploy_service(input)?;
    let namespace_revision_id = service_plan.namespace_revision_id;
    let cleanup_containers = service_plan.cleanup_containers;
    Ok(DeployPlan {
        namespace_id: NamespaceId::try_new(service_plan.service_id.as_str().to_owned())
            .expect("service id is a valid namespace id"),
        namespace_revision_id,
        services: vec![DeployServicePlan {
            service_id: service_plan.service_id,
            steps: service_plan.steps,
        }],
        cleanup_containers,
    })
}

pub fn plan_namespace_deploy(
    namespace_id: NamespaceId,
    namespace_revision_id: NamespaceRevisionId,
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
        namespace_revision_id,
        services: service_plans,
        cleanup_containers,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploySingleServicePlan {
    pub service_id: ServiceId,
    pub namespace_revision_id: NamespaceRevisionId,
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
        namespace_revision_id: input.request.namespace_revision_id,
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
