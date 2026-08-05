//! Pure deploy preparation and placement planning.

mod service_planning;
mod volume_handoff;

use super::volume_planning::{VolumePlan, build_namespace_volume_plan};
use super::*;
use crate::ids::OperationId;
use service_planning::plan_deploy_service;
#[cfg(test)]
use service_planning::replicated_slot;
pub use volume_handoff::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPlanningInput {
    pub service_id: ServiceId,
    pub placement: DeployPlanningPlacementInput,
    pub existing_replicas: Vec<ExistingServiceReplica>,
    pub cleanup_candidates: Vec<ObservedCleanupCandidate>,
    pub volume_pins: Vec<VolumePinState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployPlanningPlacementInput {
    Replicated { eligible_machines: Vec<MachineId> },
    Global(GlobalPlanningInput),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalPlanningInput {
    candidates: BTreeMap<MachineId, GlobalCandidateDisposition>,
    empty_selection_policy: EmptyGlobalSelectionPolicy,
}

impl GlobalPlanningInput {
    #[must_use]
    pub fn new(
        candidates: BTreeMap<MachineId, GlobalCandidateDisposition>,
        empty_selection_policy: EmptyGlobalSelectionPolicy,
    ) -> Self {
        Self {
            candidates,
            empty_selection_policy,
        }
    }

    fn candidates(&self) -> &BTreeMap<MachineId, GlobalCandidateDisposition> {
        &self.candidates
    }

    const fn empty_selection_policy(&self) -> EmptyGlobalSelectionPolicy {
        self.empty_selection_policy
    }

    #[must_use]
    pub fn selected_machines(&self) -> Vec<MachineId> {
        self.candidates
            .iter()
            .filter_map(|(machine_id, disposition)| {
                matches!(disposition, GlobalCandidateDisposition::Selected)
                    .then_some(machine_id.clone())
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalCandidateDisposition {
    Selected,
    Deferred {
        reason: crate::machine::MachineUsabilityReason,
    },
    Draining,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyGlobalSelectionPolicy {
    RequireSelected,
    PreserveEquivalentGlobalTarget,
}

impl DeployPlanningInput {
    pub(crate) fn eligible_machines(&self) -> &[MachineId] {
        match &self.placement {
            DeployPlanningPlacementInput::Replicated { eligible_machines } => eligible_machines,
            // Global services cannot mount volumes; volume planning never
            // consumes their selected machines.
            DeployPlanningPlacementInput::Global(_) => &[],
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DeployPlanningContext<'a> {
    pub storage_testimony:
        &'a std::collections::BTreeMap<MachineId, Option<crate::machine::StorageCapability>>,
    pub placement_load: &'a MachinePlacementLoad,
}

/// Service containers placed per machine: seeded from observed reality and
/// advanced as a plan assigns new containers, so a machine's share is read
/// from what already runs there rather than from its position in a candidate
/// list. Placement counts containers alone and never load or capacity, so the
/// same cluster state always yields the same plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachinePlacementLoad {
    counts: std::collections::BTreeMap<MachineId, usize>,
}

impl MachinePlacementLoad {
    #[must_use]
    pub const fn new(counts: std::collections::BTreeMap<MachineId, usize>) -> Self {
        Self { counts }
    }

    fn record(&mut self, machine_id: &MachineId) {
        *self.counts.entry(machine_id.clone()).or_insert(0) += 1;
    }

    /// Drop a container the plan supersedes. Saturating, because the observed
    /// load and the cleanup set are gathered separately and a container may be
    /// named by one without appearing in the other.
    fn retire(&mut self, machine_id: &MachineId) {
        self.counts
            .entry(machine_id.clone())
            .and_modify(|count| *count = count.saturating_sub(1));
    }

    /// The candidate carrying the fewest placed containers, breaking ties on
    /// machine id so a plan does not depend on candidate order.
    fn least_loaded(&self, candidates: &[MachineId]) -> Option<MachineId> {
        candidates
            .iter()
            .min_by_key(|machine_id| {
                (
                    self.counts.get(*machine_id).copied().unwrap_or(0),
                    *machine_id,
                )
            })
            .cloned()
    }
}

/// The placement load a namespace deploy starts from. Running service
/// containers across every namespace count, because a machine's share of the
/// cluster is what it is running, not what one namespace put there.
#[must_use]
pub fn observed_placement_load(
    observed_machines: &[crate::machine::runtime::MachineContainerObservationSnapshot],
) -> MachinePlacementLoad {
    let mut counts = std::collections::BTreeMap::new();
    for container in observed_machines
        .iter()
        .flat_map(crate::machine::runtime::MachineContainerObservationSnapshot::containers)
        .filter(|container| container.is_service() && container.state.is_running())
    {
        *counts.entry(container.machine_id.clone()).or_insert(0) += 1;
    }
    MachinePlacementLoad::new(counts)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPlanningTarget {
    namespace_id: NamespaceId,
    volumes: BTreeMap<VolumeName, VolumeSpec>,
    services: Vec<DeployPlanningService>,
    revision_scope: DeployRevisionScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DeployRevisionScope {
    Validation,
    Keyed(EnvironmentRevisionKey),
}

impl DeployPlanningTarget {
    pub fn try_from_deploy(request: &DeployRequest) -> Result<Self, DeployTargetValidationError> {
        let services = request
            .services
            .iter()
            .map(DeployPlanningService::from_deploy)
            .collect::<Vec<_>>();
        Self::try_new(
            request.namespace_id.clone(),
            request.volumes.clone(),
            services,
            DeployRevisionScope::Validation,
        )
    }

    pub fn try_from_operation(
        request: &DeployRequest,
        environment_key: &EnvironmentRevisionKey,
    ) -> Result<Self, DeployTargetValidationError> {
        let services = request
            .services
            .iter()
            .map(DeployPlanningService::from_deploy)
            .collect::<Vec<_>>();
        Self::try_new(
            request.namespace_id.clone(),
            request.volumes.clone(),
            services,
            DeployRevisionScope::Keyed(environment_key.clone()),
        )
    }

    pub fn try_from_preview(
        target: &DeployPreviewTarget,
        environment_key: &EnvironmentRevisionKey,
    ) -> Result<Self, DeployTargetValidationError> {
        let services = target
            .services
            .iter()
            .map(DeployPlanningService::from_preview)
            .collect::<Vec<_>>();
        Self::try_new(
            target.namespace_id.clone(),
            target.volumes.clone(),
            services,
            DeployRevisionScope::Keyed(environment_key.clone()),
        )
    }

    fn try_new(
        namespace_id: NamespaceId,
        volumes: BTreeMap<VolumeName, VolumeSpec>,
        services: Vec<DeployPlanningService>,
        revision_scope: DeployRevisionScope,
    ) -> Result<Self, DeployTargetValidationError> {
        let mut service_ids = BTreeSet::new();
        for service in &services {
            if !service_ids.insert(service.service_id.clone()) {
                return Err(DeployTargetValidationError::DuplicateServiceId {
                    service_id: service.service_id.clone(),
                });
            }
        }
        super::request::validate_deploy_target(
            &namespace_id,
            &volumes,
            services
                .iter()
                .map(|service| (&service.service_id, &service.mode, &service.runtime)),
        )?;
        for service in &services {
            if let DeployPlanningImage::Concrete {
                image,
                image_source,
            } = &service.image
            {
                validate_pushed_image_reference(image, image_source).map_err(|source| {
                    DeployTargetValidationError::InvalidPushedImage {
                        service_id: service.service_id.clone(),
                        source,
                    }
                })?;
            }
        }
        Ok(Self {
            namespace_id,
            volumes,
            services,
            revision_scope,
        })
    }

    #[must_use]
    pub fn namespace_id(&self) -> &NamespaceId {
        &self.namespace_id
    }

    #[must_use]
    pub fn status_service_id(&self) -> ServiceId {
        self.services.first().map_or_else(
            || {
                ServiceId::try_new(self.namespace_id.as_str().to_owned())
                    .expect("namespace id is a valid service id fallback")
            },
            |service| service.service_id.clone(),
        )
    }

    #[must_use]
    pub fn volumes(&self) -> &BTreeMap<VolumeName, VolumeSpec> {
        &self.volumes
    }

    #[must_use]
    pub fn service(&self, service_id: &ServiceId) -> Option<&DeployPlanningService> {
        self.services
            .iter()
            .find(|service| service.service_id == *service_id)
    }

    #[must_use]
    pub fn services(&self) -> &[DeployPlanningService] {
        &self.services
    }

    #[must_use]
    pub fn namespace_revision_id(&self, request: &DeployRequest) -> Option<NamespaceRevisionId> {
        match &self.revision_scope {
            DeployRevisionScope::Validation => None,
            DeployRevisionScope::Keyed(environment_key) => {
                Some(request.namespace_revision_id(environment_key))
            }
        }
    }

    pub fn validate_registry_credential_service_ids<'a>(
        &self,
        service_ids: impl IntoIterator<Item = &'a ServiceId>,
    ) -> Result<(), DeployRegistryCredentialTargetError> {
        for service_id in service_ids {
            let Some(service) = self.service(service_id) else {
                return Err(DeployRegistryCredentialTargetError::UnknownService {
                    service_id: service_id.clone(),
                });
            };
            match &service.image {
                DeployPlanningImage::Concrete {
                    image_source: ImageSource::Registry,
                    ..
                } => {}
                DeployPlanningImage::PendingBuild => {
                    return Err(DeployRegistryCredentialTargetError::PendingBuild {
                        service_id: service_id.clone(),
                    });
                }
                DeployPlanningImage::Concrete {
                    image_source: ImageSource::PushedToSeed(_),
                    ..
                } => {
                    return Err(DeployRegistryCredentialTargetError::PushedImage {
                        service_id: service_id.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployRegistryCredentialTargetError {
    #[error(
        "registry credential for service {} does not name a service in the deploy target",
        .service_id.as_str()
    )]
    UnknownService { service_id: ServiceId },
    #[error(
        "registry credential for service {} belongs to a pending build",
        .service_id.as_str()
    )]
    PendingBuild { service_id: ServiceId },
    #[error(
        "registry credential for service {} belongs to a pushed image",
        .service_id.as_str()
    )]
    PushedImage { service_id: ServiceId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPlanningService {
    service_id: ServiceId,
    image: DeployPlanningImage,
    mode: ServiceMode,
    keep: Option<ContainerRetentionCount>,
    runtime: ContainerRuntimeSpec,
    pre_start: Option<PreStartHook>,
    dependencies: Vec<ServiceDependency>,
    routes: Vec<DeployRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployPlanningImage {
    Concrete {
        image: ImageReference,
        image_source: ImageSource,
    },
    PendingBuild,
}

impl DeployPlanningService {
    fn from_deploy(service: &DeployServiceSpec) -> Self {
        Self {
            service_id: service.service_id.clone(),
            image: DeployPlanningImage::Concrete {
                image: service.image.clone(),
                image_source: service.image_source.clone(),
            },
            mode: service.mode,
            keep: service.keep,
            runtime: service.runtime.clone(),
            pre_start: service.pre_start.clone(),
            dependencies: service.depends_on.clone(),
            routes: service.routes.clone(),
        }
    }

    fn from_preview(service: &DeployPreviewService) -> Self {
        let image = match &service.image {
            DeployPreviewImage::Concrete {
                image,
                image_source,
            } => DeployPlanningImage::Concrete {
                image: image.clone(),
                image_source: image_source.clone(),
            },
            DeployPreviewImage::PendingBuild => DeployPlanningImage::PendingBuild,
        };
        Self {
            service_id: service.service_id.clone(),
            image,
            mode: service.mode,
            keep: service.keep,
            runtime: service.runtime.clone(),
            pre_start: service.pre_start.clone(),
            dependencies: service.depends_on.clone(),
            routes: service.routes.clone(),
        }
    }

    #[must_use]
    pub fn service_id(&self) -> &ServiceId {
        &self.service_id
    }

    #[must_use]
    pub fn mode(&self) -> ServiceMode {
        self.mode
    }

    #[must_use]
    pub fn keep(&self) -> Option<ContainerRetentionCount> {
        self.keep
    }

    #[must_use]
    pub fn runtime(&self) -> &ContainerRuntimeSpec {
        &self.runtime
    }

    #[must_use]
    pub fn pre_start(&self) -> Option<&PreStartHook> {
        self.pre_start.as_ref()
    }

    #[must_use]
    pub fn dependencies(&self) -> &[ServiceDependency] {
        &self.dependencies
    }

    #[must_use]
    pub fn routes(&self) -> &[DeployRoute] {
        &self.routes
    }

    #[must_use]
    pub fn concrete_image(&self) -> Option<(&ImageReference, &ImageSource)> {
        match &self.image {
            DeployPlanningImage::Concrete {
                image,
                image_source,
            } => Some((image, image_source)),
            DeployPlanningImage::PendingBuild => None,
        }
    }

    #[must_use]
    pub fn namespace_revision_entry_id(
        &self,
        namespace_id: &NamespaceId,
        environment_key: &EnvironmentRevisionKey,
    ) -> Option<NamespaceRevisionEntryId> {
        match &self.image {
            DeployPlanningImage::Concrete {
                image,
                image_source,
            } => Some(namespace_revision_entry_id_for(
                namespace_id,
                &self.service_id,
                image,
                image_source,
                &self.runtime,
                environment_key,
            )),
            DeployPlanningImage::PendingBuild => None,
        }
    }

    #[must_use]
    pub fn namespace_revision_entry_id_for_target(
        &self,
        target: &DeployPlanningTarget,
    ) -> Option<NamespaceRevisionEntryId> {
        let (image, image_source) = self.concrete_image()?;
        Some(match &target.revision_scope {
            DeployRevisionScope::Validation => return None,
            DeployRevisionScope::Keyed(environment_key) => namespace_revision_entry_id_for(
                target.namespace_id(),
                &self.service_id,
                image,
                image_source,
                &self.runtime,
                environment_key,
            ),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingServiceReplica {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub creation_gate: ExistingReplicaCreationGate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingReplicaCreationGate {
    /// Current serving intent proves a prior deploy completed the creation gate.
    AlreadyPassed,
    /// Durable interruption provenance permits adoption, but cannot prove the
    /// container passed its first-creation gate.
    RequiredAfterInterruption,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExistingReplicaPolicy {
    /// Matching observed replicas belong to the current serving target.
    Promoted {
        /// Containers created by an interrupted attempt against this same
        /// entry still require their own creation gate.
        interrupted_operation_ids: std::collections::BTreeSet<OperationId>,
    },
    /// Only replicas created by one of these durably interrupted deploys may
    /// be adopted, and each adopted replica must pass its creation gate.
    RecoverInterrupted {
        operation_ids: std::collections::BTreeSet<OperationId>,
    },
    /// Matching observations have neither serving intent nor recoverable
    /// interruption provenance.
    ExcludeUnpromoted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPreparationInput<'a> {
    pub target: &'a DeployPlanningTarget,
    pub service_id: ServiceId,
    pub occupied_route_bindings: Vec<RouteBindingState>,
    pub eligible_machines: Vec<MachineId>,
    pub machine_platforms: BTreeMap<MachineId, OciPlatform>,
    /// Machines under drain intent: their running replicas do not count as
    /// existing capacity, so the plan replaces them elsewhere and the
    /// unselected containers fall through to cleanup. This is how drain
    /// evacuates on the next deploy (ADR 0026). A separate list rather than
    /// filtering by `eligible_machines`: replica reuse is reality-driven and
    /// deliberately independent of placement scope, so only explicit drain
    /// intent may disqualify observed capacity.
    pub draining_machines: Vec<MachineId>,
    pub observed_machines: Vec<MachineContainerObservationSnapshot>,
    pub existing_replica_policy: ExistingReplicaPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedDeploy {
    pub route_additions: Vec<DeployRouteBindingAddition>,
    pub eligible_machines: Vec<MachineId>,
    pub unusable_machines: Vec<crate::operation::UnusableMachine>,
    pub existing_replicas: Vec<ExistingServiceReplica>,
    pub cleanup_candidates: Vec<ObservedCleanupCandidate>,
}

/// A validated route addition before an authoritative operation assigns or
/// preserves its durable binding identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployRouteBindingAddition {
    pub namespace_id: NamespaceId,
    pub target: RouteTarget,
    pub endpoint_port: RoutePort,
    pub service_id: ServiceId,
    pub origin: RouteBindingOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployPlacementPlan {
    pub namespace_id: NamespaceId,
    pub phases: Vec<DeployPhasePlan>,
    pub volume_pin_commits: Vec<VolumePinState>,
    pub volume_ensures: Vec<VolumePinState>,
    pub cleanup_actions: Vec<DeployCleanupAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployPlan {
    pub namespace_id: NamespaceId,
    pub namespace_revision_id: NamespaceRevisionId,
    pub phases: Vec<DeployPhasePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    /// Desired pins committed after image availability is ensured and before
    /// the corresponding machine-local volume effects begin.
    pub volume_pin_commits: Vec<VolumePinState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volume_ensures: Vec<VolumePinState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup_actions: Vec<DeployCleanupAction>,
}

impl DeployPlan {
    #[must_use]
    pub fn target_machines(&self) -> Vec<MachineId> {
        target_machines(&self.phases)
    }

    #[must_use]
    pub fn service_target_machines(&self, service_id: &ServiceId) -> Option<Vec<MachineId>> {
        service_target_machines(&self.phases, service_id)
    }
}

impl DeployPlacementPlan {
    #[must_use]
    pub fn with_revision(self, namespace_revision_id: NamespaceRevisionId) -> DeployPlan {
        DeployPlan {
            namespace_id: self.namespace_id,
            namespace_revision_id,
            phases: self.phases,
            volume_pin_commits: self.volume_pin_commits,
            volume_ensures: self.volume_ensures,
            cleanup_actions: self.cleanup_actions,
        }
    }

    #[must_use]
    pub fn target_machines(&self) -> Vec<MachineId> {
        target_machines(&self.phases)
    }

    #[must_use]
    pub fn service_target_machines(&self, service_id: &ServiceId) -> Option<Vec<MachineId>> {
        service_target_machines(&self.phases, service_id)
    }
}

fn target_machines(phases: &[DeployPhasePlan]) -> Vec<MachineId> {
    let mut machines = phases
        .iter()
        .flat_map(|phase| &phase.services)
        .flat_map(|service| service.work.steps())
        .map(|step| step.machine_id().clone())
        .collect::<Vec<_>>();
    machines.sort();
    machines.dedup();
    machines
}

fn service_target_machines(
    phases: &[DeployPhasePlan],
    service_id: &ServiceId,
) -> Option<Vec<MachineId>> {
    let service = phases
        .iter()
        .flat_map(|phase| &phase.services)
        .find(|service| &service.service_id == service_id)?;
    let mut machines = service
        .work
        .steps()
        .map(|step| step.machine_id().clone())
        .chain(service.pre_start.iter().map(|step| step.machine_id.clone()))
        .collect::<Vec<_>>();
    machines.sort();
    machines.dedup();
    Some(machines)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployPhasePlan {
    pub services: Vec<DeployServicePlan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployServicePlan {
    pub service_id: ServiceId,
    pub placement: DeployServicePlacement,
    pub work: DeployServiceWork,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_start: Option<PreStartHookStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployServiceWork {
    Ordinary {
        steps: Vec<DeployPlanStep>,
    },
    VolumeHandoff {
        replacement: DeployRunContainerStep,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remaining_steps: Vec<DeployPlanStep>,
        participants: NonEmptyVolumeHandoffParticipants,
    },
}

impl DeployServiceWork {
    pub fn steps(&self) -> impl Iterator<Item = DeployPlanStepRef<'_>> {
        let (replacement, remaining): (Option<&DeployRunContainerStep>, &[DeployPlanStep]) =
            match self {
                Self::Ordinary { steps } => (None, steps),
                Self::VolumeHandoff {
                    replacement,
                    remaining_steps,
                    participants: _,
                } => (Some(replacement), remaining_steps),
            };
        replacement
            .into_iter()
            .map(|step| DeployPlanStepRef::RunContainer {
                machine_id: &step.machine_id,
                slot: &step.slot,
            })
            .chain(remaining.iter().map(|step| match step {
                DeployPlanStep::UseExistingContainer {
                    machine_id,
                    container_id,
                    slot,
                } => DeployPlanStepRef::UseExisting {
                    machine_id,
                    container_id,
                    slot,
                },
                DeployPlanStep::RunContainer { machine_id, slot } => {
                    DeployPlanStepRef::RunContainer { machine_id, slot }
                }
            }))
    }
}

#[derive(Debug, Clone, Copy)]
pub enum DeployPlanStepRef<'a> {
    UseExisting {
        machine_id: &'a MachineId,
        container_id: &'a ContainerId,
        slot: &'a ReplicaSlot,
    },
    RunContainer {
        machine_id: &'a MachineId,
        slot: &'a ReplicaSlot,
    },
}

impl DeployPlanStepRef<'_> {
    #[must_use]
    pub const fn machine_id(&self) -> &MachineId {
        match self {
            Self::UseExisting { machine_id, .. } | Self::RunContainer { machine_id, .. } => {
                machine_id
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployRunContainerStep {
    pub machine_id: MachineId,
    pub slot: ReplicaSlot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployServicePlacement {
    Replicated,
    Global {
        candidates: Vec<MachineId>,
        selected: Vec<MachineId>,
        deferred: Vec<crate::operation::UnusableMachine>,
        draining: Vec<MachineId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PreStartHookStep {
    pub machine_id: MachineId,
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReplicaSlot {
    Replicated { number: ReplicatedReplicaSlot },
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "SafeInteger<\"ReplicatedReplicaSlot\">")
)]
#[serde(try_from = "u16", into = "u16")]
pub struct ReplicatedReplicaSlot(u16);

impl ReplicatedReplicaSlot {
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

impl TryFrom<u16> for ReplicatedReplicaSlot {
    type Error = ReplicaSlotError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ReplicatedReplicaSlot> for u16 {
    fn from(value: ReplicatedReplicaSlot) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplicaSlotError {
    #[error("replica slot must be greater than zero")]
    Zero,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployPlanError {
    #[error("validated deploy target does not contain service {}", .service_id.as_str())]
    UnknownService { service_id: ServiceId },
    #[error("service {} has no eligible machines", .service_id.as_str())]
    NoEligibleMachines { service_id: ServiceId },
    #[error("service {} mode does not match its planning placement input", .service_id.as_str())]
    PlacementModeMismatch { service_id: ServiceId },
    #[error(
        "service {} depends on unknown service {}",
        .service_id.as_str(),
        .dependency.as_str()
    )]
    UnknownServiceDependency {
        service_id: ServiceId,
        dependency: ServiceId,
    },
    #[error("service dependency cycle contains {service_ids:?}")]
    ServiceDependencyCycle { service_ids: Vec<ServiceId> },
    #[error(
        "service {} requires healthy dependency {} without an executable healthcheck",
        .service_id.as_str(),
        .dependency.as_str()
    )]
    HealthyDependencyWithoutHealthcheck {
        service_id: ServiceId,
        dependency: ServiceId,
    },
    #[error("service {} has conflicting provisioned volume pins on {machines:?}", .service_id.as_str())]
    ConflictingVolumePins {
        service_id: ServiceId,
        machines: Vec<MachineId>,
    },
    #[error("service {} failed volume admission: {failure}", .service_id.as_str())]
    VolumeAdmission {
        service_id: ServiceId,
        failure: VolumeAdmissionFailure,
    },
    #[error(
        "service {} failed volume admission on machine {}: {failure}",
        .service_id.as_str(),
        .machine_id.as_str()
    )]
    VolumeAdmissionOnMachine {
        service_id: ServiceId,
        machine_id: MachineId,
        failure: Box<VolumeAdmissionFailure>,
    },
}

pub fn plan_namespace_deploy(
    target: &DeployPlanningTarget,
    services: Vec<DeployPlanningInput>,
    cleanup_containers: Vec<DeployCleanupContainer>,
    context: DeployPlanningContext<'_>,
) -> Result<DeployPlacementPlan, DeployPlanError> {
    let phases = dependency_ordered_phases(target, services)?;
    let mut phase_plans = Vec::new();
    let volume_plan = build_namespace_volume_plan(target, &phases, context)?;
    let volume_pin_commits = volume_plan.commits().to_vec();
    let volume_ensures = volume_plan.ensures().to_vec();
    let mut cleanup_actions = Vec::new();
    let mut placement_load = context.placement_load.clone();
    for phase in phases {
        let mut services = Vec::new();
        for input in phase {
            let plan = plan_deploy_service(target, input, &volume_plan, &mut placement_load)?;
            cleanup_actions.extend(plan.cleanup_actions);
            services.push(DeployServicePlan {
                service_id: plan.service_id,
                placement: plan.placement,
                work: plan.work,
                pre_start: plan.pre_start,
            });
        }
        phase_plans.push(DeployPhasePlan { services });
    }

    cleanup_actions.extend(
        cleanup_containers
            .into_iter()
            .map(|target| DeployCleanupAction::RemoveContainer { target }),
    );
    cleanup_actions.sort_by(|left, right| {
        left.target()
            .machine_id
            .cmp(&right.target().machine_id)
            .then_with(|| left.target().container_id.cmp(&right.target().container_id))
    });
    cleanup_actions.dedup_by(|left, right| {
        left.target().machine_id == right.target().machine_id
            && left.target().container_id == right.target().container_id
    });

    Ok(DeployPlacementPlan {
        namespace_id: target.namespace_id().clone(),
        phases: phase_plans,
        volume_pin_commits,
        volume_ensures,
        cleanup_actions,
    })
}

fn dependency_ordered_phases(
    target: &DeployPlanningTarget,
    services: Vec<DeployPlanningInput>,
) -> Result<Vec<Vec<DeployPlanningInput>>, DeployPlanError> {
    let service_healthchecks = services
        .iter()
        .map(|input| {
            let service = planning_service(target, &input.service_id)?;
            Ok((
                input.service_id.clone(),
                service
                    .runtime()
                    .healthcheck
                    .as_ref()
                    .is_some_and(ContainerHealthcheck::reports_docker_health),
            ))
        })
        .collect::<Result<BTreeMap<_, _>, DeployPlanError>>()?;
    let mut dependents = BTreeMap::<ServiceId, Vec<ServiceId>>::new();
    let mut dependency_counts = BTreeMap::<ServiceId, usize>::new();

    for input in &services {
        let service = planning_service(target, &input.service_id)?;
        let mut dependencies = BTreeSet::new();
        for dependency in service.dependencies() {
            let Some(has_healthcheck) = service_healthchecks.get(&dependency.service_id) else {
                return Err(DeployPlanError::UnknownServiceDependency {
                    service_id: input.service_id.clone(),
                    dependency: dependency.service_id.clone(),
                });
            };
            if dependency.condition == DependencyCondition::Healthy && !has_healthcheck {
                return Err(DeployPlanError::HealthyDependencyWithoutHealthcheck {
                    service_id: input.service_id.clone(),
                    dependency: dependency.service_id.clone(),
                });
            }
            dependencies.insert(dependency.service_id.clone());
        }
        dependency_counts.insert(input.service_id.clone(), dependencies.len());
        for dependency in dependencies {
            dependents
                .entry(dependency)
                .or_default()
                .push(input.service_id.clone());
        }
    }

    let mut ready = dependency_counts
        .iter()
        .filter_map(|(service_id, count)| (*count == 0).then_some(service_id.clone()))
        .collect::<BTreeSet<_>>();
    let mut phase_ids = Vec::new();
    let mut placed = 0;
    while !ready.is_empty() {
        let current = std::mem::take(&mut ready);
        placed += current.len();
        for service_id in &current {
            if let Some(service_dependents) = dependents.get(service_id) {
                for dependent in service_dependents {
                    let Some(dependency_count) = dependency_counts.get_mut(dependent) else {
                        continue;
                    };
                    *dependency_count -= 1;
                    if *dependency_count == 0 {
                        ready.insert(dependent.clone());
                    }
                }
            }
        }
        phase_ids.push(current);
    }

    if placed != services.len() {
        let mut service_ids = dependency_counts
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(service_id, _)| service_id.clone())
            .collect::<Vec<_>>();
        service_ids.sort();
        return Err(DeployPlanError::ServiceDependencyCycle { service_ids });
    }

    let mut services = services
        .into_iter()
        .map(|input| (input.service_id.clone(), input))
        .collect::<BTreeMap<_, _>>();
    Ok(phase_ids
        .into_iter()
        .map(|ids| {
            ids.into_iter()
                .filter_map(|service_id| services.remove(&service_id))
                .collect()
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploySingleServicePlan {
    pub service_id: ServiceId,
    pub placement: DeployServicePlacement,
    pub work: DeployServiceWork,
    pub pre_start: Option<PreStartHookStep>,
    pub cleanup_actions: Vec<DeployCleanupAction>,
}

fn planning_service<'a>(
    target: &'a DeployPlanningTarget,
    service_id: &ServiceId,
) -> Result<&'a DeployPlanningService, DeployPlanError> {
    let Some(service) = target.service(service_id) else {
        return Err(DeployPlanError::UnknownService {
            service_id: service_id.clone(),
        });
    };
    Ok(service)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_work_exposes_each_plan_step_through_one_canonical_reference_variant() {
        let machine_id = MachineId::try_new("machine_a").expect("machine id");
        let container_id = ContainerId::try_new("ctr_existing").expect("container id");
        let existing_slot = ReplicaSlot::Replicated {
            number: ReplicatedReplicaSlot::try_new(1).expect("replica slot"),
        };
        let run_slot = ReplicaSlot::Replicated {
            number: ReplicatedReplicaSlot::try_new(2).expect("replica slot"),
        };
        let work = DeployServiceWork::Ordinary {
            steps: vec![
                DeployPlanStep::UseExistingContainer {
                    machine_id: machine_id.clone(),
                    container_id: container_id.clone(),
                    slot: existing_slot,
                },
                DeployPlanStep::RunContainer {
                    machine_id: machine_id.clone(),
                    slot: run_slot,
                },
            ],
        };

        assert!(matches!(
            work.steps().collect::<Vec<_>>().as_slice(),
            [
                DeployPlanStepRef::UseExisting {
                    machine_id: existing_machine,
                    container_id: existing_container,
                    slot: _,
                },
                DeployPlanStepRef::RunContainer {
                    machine_id: run_machine,
                    slot: _,
                },
            ] if *existing_machine == &machine_id
                && *existing_container == &container_id
                && *run_machine == &machine_id
        ));
    }

    #[test]
    fn handoff_causal_collections_reject_empty_values_and_normalize_names() {
        assert!(NonEmptyVolumeNames::try_new([]).is_err());
        assert!(NonEmptyVolumeHandoffParticipants::try_new([]).is_err());
        let names = NonEmptyVolumeNames::try_new([
            VolumeName::try_new("uploads").expect("volume"),
            VolumeName::try_new("data").expect("volume"),
            VolumeName::try_new("data").expect("volume"),
        ])
        .expect("non-empty names");
        assert_eq!(
            names.as_slice(),
            [
                VolumeName::try_new("data").expect("volume"),
                VolumeName::try_new("uploads").expect("volume"),
            ]
        );
        assert!(serde_json::from_str::<NonEmptyVolumeNames>("[]").is_err());
        assert!(serde_json::from_str::<NonEmptyVolumeHandoffParticipants>("[]").is_err());
    }

    #[test]
    fn service_target_machines_are_deduplicated_across_replicas_and_pre_start() {
        let machine = MachineId::try_new("machine_a").expect("machine id");
        let service_id = ServiceId::try_new("api").expect("service id");
        let plan = DeployPlan {
            namespace_id: NamespaceId::try_new("default").expect("namespace id"),
            namespace_revision_id: NamespaceRevisionId::try_new("rev_test").expect("revision id"),
            phases: vec![DeployPhasePlan {
                services: vec![DeployServicePlan {
                    service_id: service_id.clone(),
                    placement: DeployServicePlacement::Replicated,
                    work: DeployServiceWork::Ordinary {
                        steps: vec![
                            DeployPlanStep::RunContainer {
                                machine_id: machine.clone(),
                                slot: replicated_slot(1),
                            },
                            DeployPlanStep::RunContainer {
                                machine_id: machine.clone(),
                                slot: replicated_slot(2),
                            },
                        ],
                    },
                    pre_start: Some(PreStartHookStep {
                        machine_id: machine.clone(),
                    }),
                }],
            }],
            volume_pin_commits: Vec::new(),
            volume_ensures: Vec::new(),
            cleanup_actions: Vec::new(),
        };

        assert_eq!(
            plan.service_target_machines(&service_id),
            Some(vec![machine])
        );
    }
}
