//! Convert current cluster facts into a deploy execution command.

use ployz_core::deploy::{
    DeployCleanupContainer, DeployPlanningInput, DeployPlanningService, DeployPlanningTarget,
    DeployPreparationInput, DeployPreviewTarget, DeployRequest, DeployRouteBindingAddition,
    ExistingReplicaPolicy, RegistryCredential, commit_deploy_route_bindings,
    namespace_route_binding_removals, namespace_serving_target_removals, prepare_deploy,
    validate_deploy_route_bindings,
};
use ployz_core::ids::{MachineId, OperationId, RouteBindingId, ServiceId};
use ployz_core::image::OciPlatform;
use ployz_core::intent::RouteBindingState;
use ployz_core::intent::ServingTargetEntry;
use ployz_core::intent::VolumePinState;
use ployz_core::machine::runtime::MachineContainerObservationSnapshot;
use ployz_core::network::DataplaneMember;
use ployz_core::operation::{RouteHostname, RouteTarget};

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::certificate::GatewayCertificateTarget;
use crate::control::role_client::machine::MachineClockTestimony;

use super::placement::{ProvisionedStorageRequirement, classify_storage_usability};
use super::types::{DeployPreviewPlanningCommand, merged_unusable_machines, serving_target_entry};
use super::{DeployExecutionCommand, DeployServiceExecutionCommand};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomaticHostnameMode {
    Disabled,
    Ployz { suffix: RouteHostname },
    Custom { suffix: RouteHostname },
}

impl AutomaticHostnameMode {
    #[must_use]
    pub fn suffix(&self) -> Option<&RouteHostname> {
        match self {
            Self::Disabled => None,
            Self::Ployz { suffix } | Self::Custom { suffix } => Some(suffix),
        }
    }

    #[must_use]
    pub const fn is_ployz(&self) -> bool {
        matches!(self, Self::Ployz { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionFacts {
    pub namespace_route_bindings: Vec<RouteBindingState>,
    pub namespace_serving_entries: Vec<ServingTargetEntry>,
    pub namespace_volume_pins: Vec<VolumePinState>,
    pub eligible_machines: Vec<MachineId>,
    pub unusable_machines: Vec<ployz_core::operation::UnusableMachine>,
    pub dataplane_members: Vec<DataplaneMember>,
    pub observed_machines: Vec<MachineContainerObservationSnapshot>,
    pub machine_platforms: BTreeMap<MachineId, OciPlatform>,
    pub seed_clock_testimony: BTreeMap<MachineId, MachineClockTestimony>,
    pub machine_storage_testimony:
        BTreeMap<MachineId, Option<ployz_core::machine::StorageCapability>>,
    pub namespace_cleanup_candidates: Vec<DeployCleanupContainer>,
    pub automatic_hostname_mode: AutomaticHostnameMode,
    pub gateway_certificate_targets: Vec<GatewayCertificateTarget>,
    pub ployz_gateway_certificate_targets: Vec<GatewayCertificateTarget>,
    pub step_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionInput {
    pub(super) operation_id: OperationId,
    pub(super) request: DeployRequest,
    pub(super) facts: DeployExecutionFacts,
    pub(super) registry_credentials: BTreeMap<ServiceId, RegistryCredential>,
    /// Provenance allowed to recover matching unpromoted containers.
    pub(super) reusable_interrupted_operation_ids: BTreeSet<OperationId>,
}

impl DeployExecutionInput {
    #[must_use]
    pub fn new(
        operation_id: OperationId,
        request: DeployRequest,
        facts: DeployExecutionFacts,
        registry_credentials: BTreeMap<ServiceId, RegistryCredential>,
        reusable_interrupted_operation_ids: BTreeSet<OperationId>,
    ) -> Self {
        Self {
            operation_id,
            request,
            facts,
            registry_credentials,
            reusable_interrupted_operation_ids,
        }
    }

    #[must_use]
    #[cfg(test)]
    pub fn with_step_timeout(mut self, step_timeout: Duration) -> Self {
        self.facts.step_timeout = step_timeout;
        self
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn with_seed_clock_testimony(
        mut self,
        seed: MachineId,
        testimony: MachineClockTestimony,
    ) -> Self {
        self.facts.seed_clock_testimony.insert(seed, testimony);
        self
    }
}

#[must_use]
#[cfg(test)]
pub fn prepare_deploy_execution_command(
    operation_id: OperationId,
    request: ployz_core::deploy::DeployRequest,
    facts: DeployExecutionFacts,
) -> DeployExecutionCommand {
    prepare_deploy_execution_command_with_credentials(
        operation_id,
        request,
        facts,
        &BTreeMap::new(),
        &BTreeSet::new(),
    )
}

#[must_use]
pub(super) fn prepare_deploy_execution_command_with_credentials(
    operation_id: OperationId,
    request: DeployRequest,
    facts: DeployExecutionFacts,
    registry_credentials: &BTreeMap<ServiceId, RegistryCredential>,
    reusable_interrupted_operation_ids: &BTreeSet<OperationId>,
) -> DeployExecutionCommand {
    let target = DeployPlanningTarget::try_from_deploy(&request)
        .expect("volume-declared deploy request is a validated planning target");
    let prepared = prepare_deploy_command(
        &target,
        &facts,
        DeployPreparationKind::Execution,
        reusable_interrupted_operation_ids,
    );
    let route_binding_commits = commit_deploy_route_bindings(
        prepared.route_binding_additions.clone(),
        &facts.namespace_route_bindings,
        mint_route_binding_id,
    );
    let services = prepared
        .services
        .into_iter()
        .map(|prepared| DeployServiceExecutionCommand {
            service: request
                .services
                .iter()
                .find(|service| service.service_id == prepared.planning_input.service_id)
                .expect("validated deploy target contains its service")
                .clone(),
            registry_credential: registry_credentials
                .get(&prepared.planning_input.service_id)
                .cloned(),
            route_commits: route_binding_commits
                .iter()
                .filter(|binding| binding.service_id == prepared.planning_input.service_id)
                .cloned()
                .collect(),
            volume_pins: prepared.planning_input.volume_pins,
            eligible_machines: prepared.planning_input.eligible_machines,
            unusable_machines: prepared.unusable_machines,
            existing_replicas: prepared.planning_input.existing_replicas,
            cleanup_candidates: prepared.planning_input.cleanup_candidates,
        })
        .collect::<Vec<_>>();
    let ployz_automatic_hostnames = facts.automatic_hostname_mode.is_ployz();
    let exact_certificate_routes = exact_certificate_routes(&services, ployz_automatic_hostnames);

    DeployExecutionCommand {
        operation_id,
        request,
        services,
        route_binding_removals: prepared.route_binding_removals,
        serving_target_removals: prepared.serving_target_removals,
        namespace_cleanup_candidates: prepared.namespace_cleanup_candidates,
        storage_testimony: facts.machine_storage_testimony,
        machine_platforms: facts.machine_platforms,
        seed_clock_testimony: facts.seed_clock_testimony,
        dataplane_members: facts.dataplane_members,
        exact_certificate_routes,
        ployz_automatic_hostnames,
        gateway_certificate_targets: facts.gateway_certificate_targets,
        ployz_gateway_certificate_targets: facts.ployz_gateway_certificate_targets,
        unusable_machines: prepared.unusable_machines,
        step_timeout: facts.step_timeout,
    }
}

struct PreparedDeployCommand {
    services: Vec<PreparedDeployService>,
    route_binding_additions: Vec<DeployRouteBindingAddition>,
    route_binding_removals: Vec<RouteBindingState>,
    serving_target_commits: Vec<ServingTargetEntry>,
    serving_target_removals: Vec<ServingTargetEntry>,
    namespace_cleanup_candidates: Vec<DeployCleanupContainer>,
    unusable_machines: Vec<ployz_core::operation::UnusableMachine>,
}

struct PreparedDeployService {
    planning_input: DeployPlanningInput,
    unusable_machines: Vec<ployz_core::operation::UnusableMachine>,
}

#[derive(Clone, Copy)]
enum DeployPreparationKind {
    Execution,
    Preview,
}

fn prepare_deploy_command(
    target: &DeployPlanningTarget,
    facts: &DeployExecutionFacts,
    kind: DeployPreparationKind,
    reusable_interrupted_operation_ids: &BTreeSet<OperationId>,
) -> PreparedDeployCommand {
    let mut planning_services = target.services().iter().collect::<Vec<_>>();
    planning_services.sort_by(|left, right| left.service_id().cmp(right.service_id()));
    let route_binding_additions = validate_deploy_route_bindings(
        target,
        facts.automatic_hostname_mode.suffix(),
        &facts.namespace_route_bindings,
    )
    .expect("route bindings were validated while loading deploy facts");
    let namespace_declared_targets = route_binding_additions
        .iter()
        .map(|addition| addition.target.clone())
        .collect::<Vec<_>>();
    let declared_services = planning_services
        .iter()
        .map(|service| service.service_id().clone())
        .collect::<Vec<_>>();
    let route_binding_removal_targets = namespace_route_binding_removals(
        target.namespace_id(),
        &namespace_declared_targets,
        &facts.namespace_route_bindings,
    );
    let route_binding_removals = facts
        .namespace_route_bindings
        .iter()
        .filter(|binding| route_binding_removal_targets.contains(&binding.target))
        .cloned()
        .collect();
    let serving_target_removals = namespace_serving_target_removals(
        target.namespace_id(),
        &declared_services,
        &facts.namespace_serving_entries,
    );
    let draining_machines = draining_machines(&facts.unusable_machines);
    let mut services = Vec::new();
    let mut serving_target_commits = Vec::new();
    for service in planning_services {
        let storage_requirement =
            provisioned_storage_requirement(target, service, &facts.namespace_volume_pins);
        let (service_eligible_machines, service_unusable_machines) = classify_storage_usability(
            &facts.eligible_machines,
            &facts.machine_storage_testimony,
            &storage_requirement,
        );
        let is_promoted = service
            .namespace_revision_entry_id(target.namespace_id())
            .is_some_and(|entry_id| {
                facts.namespace_serving_entries.iter().any(|entry| {
                    entry.namespace_id == *target.namespace_id()
                        && entry.service_id == *service.service_id()
                        && entry.namespace_revision_entry_id == entry_id
                })
            });
        let prepared = prepare_planning_service(
            target,
            service.service_id(),
            service_eligible_machines,
            &draining_machines,
            facts,
            existing_replica_policy(kind, is_promoted, reusable_interrupted_operation_ids),
        );
        if let Some(entry) = preview_serving_target_entry(target.namespace_id(), service) {
            serving_target_commits.push(entry);
        }
        services.push(PreparedDeployService {
            planning_input: DeployPlanningInput {
                service_id: service.service_id().clone(),
                eligible_machines: prepared.eligible_machines,
                existing_replicas: prepared.existing_replicas,
                cleanup_candidates: prepared.cleanup_candidates,
                volume_pins: facts.namespace_volume_pins.clone(),
            },
            unusable_machines: merged_unusable_machines(
                &service_unusable_machines,
                prepared.unusable_machines,
            ),
        });
    }
    let namespace_cleanup_candidates = facts
        .namespace_cleanup_candidates
        .iter()
        .filter(|candidate| !declared_services.contains(&candidate.identity.service_id))
        .cloned()
        .collect();
    let mut unusable_machines = facts.unusable_machines.clone();
    unusable_machines.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));

    PreparedDeployCommand {
        services,
        route_binding_additions,
        route_binding_removals,
        serving_target_commits,
        serving_target_removals,
        namespace_cleanup_candidates,
        unusable_machines,
    }
}

struct PreparedPlanningService {
    eligible_machines: Vec<MachineId>,
    unusable_machines: Vec<ployz_core::operation::UnusableMachine>,
    existing_replicas: Vec<ployz_core::deploy::ExistingServiceReplica>,
    cleanup_candidates: Vec<ployz_core::deploy::ObservedCleanupCandidate>,
}

fn prepare_planning_service(
    target: &DeployPlanningTarget,
    service_id: &ServiceId,
    eligible_machines: Vec<MachineId>,
    draining_machines: &[MachineId],
    facts: &DeployExecutionFacts,
    existing_replica_policy: ExistingReplicaPolicy,
) -> PreparedPlanningService {
    let prepared = prepare_deploy(DeployPreparationInput {
        target,
        service_id: service_id.clone(),
        occupied_route_bindings: facts.namespace_route_bindings.clone(),
        eligible_machines,
        machine_platforms: facts.machine_platforms.clone(),
        draining_machines: draining_machines.to_vec(),
        observed_machines: facts.observed_machines.clone(),
        existing_replica_policy,
    })
    .expect("route bindings were validated while loading deploy facts");
    PreparedPlanningService {
        eligible_machines: prepared.eligible_machines,
        unusable_machines: prepared.unusable_machines,
        existing_replicas: prepared.existing_replicas,
        cleanup_candidates: prepared.cleanup_candidates,
    }
}

fn existing_replica_policy(
    kind: DeployPreparationKind,
    is_promoted: bool,
    reusable_interrupted_operation_ids: &BTreeSet<OperationId>,
) -> ExistingReplicaPolicy {
    if is_promoted {
        ExistingReplicaPolicy::Promoted {
            interrupted_operation_ids: reusable_interrupted_operation_ids.clone(),
        }
    } else if matches!(kind, DeployPreparationKind::Execution)
        && !reusable_interrupted_operation_ids.is_empty()
    {
        ExistingReplicaPolicy::RecoverInterrupted {
            operation_ids: reusable_interrupted_operation_ids.clone(),
        }
    } else {
        ExistingReplicaPolicy::ExcludeUnpromoted
    }
}

pub(super) fn prepare_deploy_preview_command(
    target: DeployPreviewTarget,
    facts: DeployExecutionFacts,
) -> DeployPreviewPlanningCommand {
    let planning_target = DeployPlanningTarget::try_from_preview(&target)
        .expect("volume-declared deploy preview is a validated planning target");
    let prepared = prepare_deploy_command(
        &planning_target,
        &facts,
        DeployPreparationKind::Preview,
        &BTreeSet::new(),
    );
    let mut planning_inputs = Vec::with_capacity(prepared.services.len());
    let mut unusable_machines_by_service = BTreeMap::new();
    let mut unusable_machines = prepared.unusable_machines.clone();
    for service in prepared.services {
        unusable_machines =
            merged_unusable_machines(&unusable_machines, service.unusable_machines.clone());
        unusable_machines_by_service.insert(
            service.planning_input.service_id.clone(),
            merged_unusable_machines(&prepared.unusable_machines, service.unusable_machines),
        );
        planning_inputs.push(service.planning_input);
    }

    DeployPreviewPlanningCommand {
        target,
        planning_inputs,
        route_binding_additions: prepared.route_binding_additions,
        route_binding_removals: prepared.route_binding_removals,
        serving_target_commits: prepared.serving_target_commits,
        serving_target_removals: prepared.serving_target_removals,
        namespace_cleanup_candidates: prepared.namespace_cleanup_candidates,
        storage_testimony: facts.machine_storage_testimony,
        machine_platforms: facts.machine_platforms,
        seed_clock_testimony: facts.seed_clock_testimony,
        unusable_machines,
        unusable_machines_by_service,
    }
}

fn draining_machines(
    unusable_machines: &[ployz_core::operation::UnusableMachine],
) -> Vec<MachineId> {
    unusable_machines
        .iter()
        .filter_map(|unusable| {
            use ployz_core::machine::MachineUsabilityReason;
            match &unusable.reason {
                MachineUsabilityReason::Draining => Some(unusable.machine_id.clone()),
                MachineUsabilityReason::PlatformMismatch { .. }
                | MachineUsabilityReason::FactsUnavailable
                | MachineUsabilityReason::BuildUnavailable
                | MachineUsabilityReason::StorageTestimonyNotReported
                | MachineUsabilityReason::StorageUnprepared
                | MachineUsabilityReason::StorageUnavailable { .. }
                | MachineUsabilityReason::StoragePoolMismatch { .. }
                | MachineUsabilityReason::DataplaneUnavailable { .. } => None,
            }
        })
        .collect()
}

fn preview_serving_target_entry(
    namespace_id: &ployz_core::ids::NamespaceId,
    service: &DeployPlanningService,
) -> Option<ServingTargetEntry> {
    let namespace_revision_entry_id = service.namespace_revision_entry_id(namespace_id)?;
    let (image, _) = service.concrete_image()?;
    Some(serving_target_entry(
        namespace_id,
        service.service_id(),
        namespace_revision_entry_id,
        image,
        service.replicas(),
        service.runtime(),
    ))
}

fn provisioned_storage_requirement(
    target: &DeployPlanningTarget,
    service: &DeployPlanningService,
    pins: &[VolumePinState],
) -> ProvisionedStorageRequirement {
    let mut expected_pools = Vec::new();
    let needs_provisioned = service.runtime().volume_mounts.iter().any(|mount| {
        matches!(
            target.volumes().get(&mount.volume_name),
            Some(ployz_core::deploy::VolumeSpec::Provisioned { .. })
        )
    });
    if !needs_provisioned {
        return ProvisionedStorageRequirement::None;
    }
    for mount in &service.runtime().volume_mounts {
        for pin in pins.iter().filter(|pin| {
            pin.namespace_id() == target.namespace_id() && pin.volume_name() == &mount.volume_name
        }) {
            if let ployz_core::intent::VolumeKind::Provisioned { dataset, .. } = pin.kind() {
                expected_pools.push(dataset.pool());
            }
        }
    }
    expected_pools.sort();
    expected_pools.dedup();
    ProvisionedStorageRequirement::Ready { expected_pools }
}

fn exact_certificate_routes(
    services: &[DeployServiceExecutionCommand],
    ployz_automatic_hostnames: bool,
) -> Vec<RouteBindingState> {
    let mut routes = services
        .iter()
        .flat_map(DeployServiceExecutionCommand::route_binding_states)
        .filter(|binding| {
            !ployz_automatic_hostnames
                || binding.origin != ployz_core::ingress::RouteBindingOrigin::Automatic
        })
        .cloned()
        .collect::<Vec<_>>();
    routes.sort_by(|left, right| left.id.cmp(&right.id));
    routes.dedup_by(|left, right| left.id == right.id);
    routes
}

pub(super) fn mint_route_binding_id(_target: &RouteTarget) -> RouteBindingId {
    RouteBindingId::try_new(format!("route_{}", nuid::next()))
        .expect("NUID route binding id is a valid subject token")
}

pub fn namespace_cleanup_candidates(
    namespace_id: &ployz_core::ids::NamespaceId,
    observed_machines: &[MachineContainerObservationSnapshot],
) -> Vec<DeployCleanupContainer> {
    observed_machines
        .iter()
        .flat_map(MachineContainerObservationSnapshot::containers)
        .filter(|container| {
            container.is_service() && container.identity.namespace_id == *namespace_id
        })
        .map(|container| DeployCleanupContainer {
            machine_id: container.machine_id.clone(),
            container_id: container.container_id.clone(),
            identity: container.identity.clone(),
        })
        .collect()
}
