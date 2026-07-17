//! Convert current cluster facts into a deploy execution command.

use ployz_core::deploy::{
    DeployCleanupContainer, DeployPlanningInput, DeployPlanningService, DeployPlanningTarget,
    DeployPreparationInput, DeployPreviewPreparationInput, ExistingReplicaPolicy,
    RegistryCredential, VolumeDeclaredDeployPreviewTarget, VolumeDeclaredDeployRequest,
    auto_hostname_preview_route_binding_commits, auto_hostname_route_binding_commits,
    namespace_route_binding_removals, namespace_serving_target_removals, prepare_deploy,
    prepare_deploy_preview,
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
use super::types::DeployPreviewPlanningCommand;
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
    pub(super) request: VolumeDeclaredDeployRequest,
    pub(super) facts: DeployExecutionFacts,
    pub(super) registry_credentials: BTreeMap<ServiceId, RegistryCredential>,
    /// Provenance allowed to recover matching unpromoted containers.
    pub(super) reusable_interrupted_operation_ids: BTreeSet<OperationId>,
}

impl DeployExecutionInput {
    #[must_use]
    pub fn new(
        operation_id: OperationId,
        request: VolumeDeclaredDeployRequest,
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
    let request =
        VolumeDeclaredDeployRequest::try_new(request).expect("test deploy request validates");
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
    request: VolumeDeclaredDeployRequest,
    facts: DeployExecutionFacts,
    registry_credentials: &BTreeMap<ServiceId, RegistryCredential>,
    reusable_interrupted_operation_ids: &BTreeSet<OperationId>,
) -> DeployExecutionCommand {
    let mut mint_requests = request.services().iter().collect::<Vec<_>>();
    mint_requests.sort_by(|left, right| left.service_id.cmp(&right.service_id));
    let mut declared_auto_bindings = Vec::new();
    let mut occupied_bindings = facts.namespace_route_bindings.clone();
    for service_request in mint_requests {
        let commits = auto_hostname_route_binding_commits(
            request.namespace_id(),
            service_request,
            facts.automatic_hostname_mode.suffix(),
            &occupied_bindings,
            mint_route_binding_id,
        )
        .expect("route bindings were validated while loading deploy facts");
        occupied_bindings.extend(commits.iter().cloned());
        declared_auto_bindings.extend(commits);
    }
    let namespace_declared_targets = request
        .services()
        .iter()
        .flat_map(|service| service.routes.iter())
        .filter_map(|route| route.target.concrete_target())
        .chain(
            declared_auto_bindings
                .iter()
                .map(|binding| binding.target.clone()),
        )
        .collect::<Vec<_>>();
    let declared_services = request
        .services()
        .iter()
        .map(|service| service.service_id.clone())
        .collect::<Vec<_>>();
    let route_binding_removal_targets = namespace_route_binding_removals(
        request.namespace_id(),
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
        request.namespace_id(),
        &declared_services,
        &facts.namespace_serving_entries,
    );
    let draining_machines = draining_machines(&facts.unusable_machines);
    let mut services = Vec::new();
    let mut unusable_machines = facts.unusable_machines.clone();
    for service in request.services() {
        let storage_requirement = provisioned_storage_requirement(
            DeployPlanningTarget::Deploy(&request),
            DeployPlanningTarget::Deploy(&request)
                .service(&service.service_id)
                .expect("validated deploy target contains its service"),
            &facts.namespace_volume_pins,
        );
        let (service_eligible_machines, mut service_unusable_machines) = classify_storage_usability(
            &facts.eligible_machines,
            &facts.machine_storage_testimony,
            &storage_requirement,
        );
        let is_promoted = facts.namespace_serving_entries.iter().any(|entry| {
            entry.namespace_id == *request.namespace_id()
                && entry.service_id == service.service_id
                && entry.namespace_revision_entry_id
                    == service.namespace_revision_entry_id(request.namespace_id())
        });
        let prepared = prepare_deploy(
            DeployPreparationInput {
                request: &request,
                service_id: service.service_id.clone(),
                occupied_route_bindings: occupied_bindings.clone(),
                eligible_machines: service_eligible_machines,
                machine_platforms: facts.machine_platforms.clone(),
                draining_machines: draining_machines.clone(),
                observed_machines: facts.observed_machines.clone(),
                existing_replica_policy: if is_promoted {
                    ExistingReplicaPolicy::Promoted {
                        interrupted_operation_ids: reusable_interrupted_operation_ids.clone(),
                    }
                } else if reusable_interrupted_operation_ids.is_empty() {
                    ExistingReplicaPolicy::ExcludeUnpromoted
                } else {
                    ExistingReplicaPolicy::RecoverInterrupted {
                        operation_ids: reusable_interrupted_operation_ids.clone(),
                    }
                },
            },
            mint_route_binding_id,
        )
        .expect("route bindings were validated while loading deploy facts");
        for unusable in &prepared.unusable_machines {
            if !service_unusable_machines.contains(unusable) {
                service_unusable_machines.push(unusable.clone());
            }
        }
        service_unusable_machines.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
        occupied_bindings.extend(prepared.route_commits.iter().cloned());
        let mut route_commits = prepared.route_commits;
        route_commits.extend(
            declared_auto_bindings
                .iter()
                .filter(|binding| binding.service_id == prepared.service.service_id)
                .cloned(),
        );
        services.push(DeployServiceExecutionCommand {
            registry_credential: registry_credentials
                .get(&prepared.service.service_id)
                .cloned(),
            service: prepared.service,
            route_commits,
            volume_pins: facts.namespace_volume_pins.clone(),
            eligible_machines: prepared.eligible_machines,
            unusable_machines: service_unusable_machines,
            existing_replicas: prepared.existing_replicas,
            cleanup_candidates: prepared.cleanup_candidates,
        });
    }
    let ployz_automatic_hostnames = facts.automatic_hostname_mode.is_ployz();
    unusable_machines.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
    let exact_certificate_routes = exact_certificate_routes(&services, ployz_automatic_hostnames);

    // Manifest omission removes a service: its containers are cleanup
    // candidates on every deploy, not only when the manifest is empty.
    // The candidates are already namespace-scoped at collection.
    let namespace_cleanup_candidates = facts
        .namespace_cleanup_candidates
        .into_iter()
        .filter(|candidate| !declared_services.contains(&candidate.identity.service_id))
        .collect();

    DeployExecutionCommand {
        operation_id,
        request,
        services,
        route_binding_removals,
        serving_target_removals,
        namespace_cleanup_candidates,
        storage_testimony: facts.machine_storage_testimony,
        machine_platforms: facts.machine_platforms,
        seed_clock_testimony: facts.seed_clock_testimony,
        dataplane_members: facts.dataplane_members,
        exact_certificate_routes,
        ployz_automatic_hostnames,
        gateway_certificate_targets: facts.gateway_certificate_targets,
        ployz_gateway_certificate_targets: facts.ployz_gateway_certificate_targets,
        unusable_machines,
        step_timeout: facts.step_timeout,
    }
}

pub(super) fn prepare_deploy_preview_command(
    target: VolumeDeclaredDeployPreviewTarget,
    facts: DeployExecutionFacts,
) -> DeployPreviewPlanningCommand {
    let mut mint_requests = target.services().iter().collect::<Vec<_>>();
    mint_requests.sort_by(|left, right| left.service_id.cmp(&right.service_id));
    let mut declared_auto_bindings = Vec::new();
    let mut occupied_bindings = facts.namespace_route_bindings.clone();
    for service in mint_requests {
        let commits = auto_hostname_preview_route_binding_commits(
            target.namespace_id(),
            service,
            facts.automatic_hostname_mode.suffix(),
            &occupied_bindings,
            mint_route_binding_id,
        )
        .expect("route bindings were validated while loading deploy preview facts");
        occupied_bindings.extend(commits.iter().cloned());
        declared_auto_bindings.extend(commits);
    }
    let namespace_declared_targets = target
        .services()
        .iter()
        .flat_map(|service| service.routes.iter())
        .filter_map(|route| route.target.concrete_target())
        .chain(
            declared_auto_bindings
                .iter()
                .map(|binding| binding.target.clone()),
        )
        .collect::<Vec<_>>();
    let declared_services = target
        .services()
        .iter()
        .map(|service| service.service_id.clone())
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
    let mut planning_inputs = Vec::new();
    let mut route_binding_commits = Vec::new();
    let mut serving_target_commits = Vec::new();
    let mut unusable_machines_by_service = BTreeMap::new();
    for service in target.services() {
        let planning_service = DeployPlanningTarget::Preview(&target)
            .service(&service.service_id)
            .expect("validated preview target contains its service");
        let storage_requirement = provisioned_storage_requirement(
            DeployPlanningTarget::Preview(&target),
            planning_service,
            &facts.namespace_volume_pins,
        );
        let (service_eligible_machines, mut service_unusable_machines) = classify_storage_usability(
            &facts.eligible_machines,
            &facts.machine_storage_testimony,
            &storage_requirement,
        );
        let is_promoted = planning_service
            .namespace_revision_entry_id(target.namespace_id())
            .is_some_and(|entry_id| {
                facts.namespace_serving_entries.iter().any(|entry| {
                    entry.namespace_id == *target.namespace_id()
                        && entry.service_id == service.service_id
                        && entry.namespace_revision_entry_id == entry_id
                })
            });
        let prepared = prepare_deploy_preview(
            DeployPreviewPreparationInput {
                target: &target,
                service_id: service.service_id.clone(),
                occupied_route_bindings: occupied_bindings.clone(),
                eligible_machines: service_eligible_machines,
                machine_platforms: facts.machine_platforms.clone(),
                draining_machines: draining_machines.clone(),
                observed_machines: facts.observed_machines.clone(),
                existing_replica_policy: if is_promoted {
                    ExistingReplicaPolicy::Promoted {
                        interrupted_operation_ids: BTreeSet::new(),
                    }
                } else {
                    ExistingReplicaPolicy::ExcludeUnpromoted
                },
            },
            mint_route_binding_id,
        )
        .expect("route bindings were validated while loading deploy preview facts");
        for unusable in &prepared.unusable_machines {
            if !service_unusable_machines.contains(unusable) {
                service_unusable_machines.push(unusable.clone());
            }
        }
        service_unusable_machines.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
        occupied_bindings.extend(prepared.route_commits.iter().cloned());
        let mut service_route_commits = prepared.route_commits;
        service_route_commits.extend(
            declared_auto_bindings
                .iter()
                .filter(|binding| binding.service_id == prepared.service.service_id)
                .cloned(),
        );
        route_binding_commits.extend(service_route_commits);
        if let Some(entry) = preview_serving_target_entry(target.namespace_id(), planning_service) {
            serving_target_commits.push(entry);
        }
        unusable_machines_by_service.insert(
            prepared.service.service_id.clone(),
            merged_unusable_machines(&facts.unusable_machines, service_unusable_machines),
        );
        planning_inputs.push(DeployPlanningInput {
            service_id: prepared.service.service_id,
            eligible_machines: prepared.eligible_machines,
            existing_replicas: prepared.existing_replicas,
            cleanup_candidates: prepared.cleanup_candidates,
            volume_pins: facts.namespace_volume_pins.clone(),
        });
    }
    let namespace_cleanup_candidates = facts
        .namespace_cleanup_candidates
        .into_iter()
        .filter(|candidate| !declared_services.contains(&candidate.identity.service_id))
        .collect();
    let mut unusable_machines = facts.unusable_machines;
    unusable_machines.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));

    DeployPreviewPlanningCommand {
        target,
        planning_inputs,
        route_binding_commits,
        route_binding_removals,
        serving_target_commits,
        serving_target_removals,
        namespace_cleanup_candidates,
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
        .filter(|unusable| {
            matches!(
                &unusable.reason,
                ployz_core::machine::MachineUsabilityReason::Draining
            )
        })
        .map(|unusable| unusable.machine_id.clone())
        .collect()
}

fn merged_unusable_machines(
    shared: &[ployz_core::operation::UnusableMachine],
    service: Vec<ployz_core::operation::UnusableMachine>,
) -> Vec<ployz_core::operation::UnusableMachine> {
    let mut merged = shared.to_vec();
    for unusable in service {
        if !merged.contains(&unusable) {
            merged.push(unusable);
        }
    }
    merged.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
    merged
}

fn preview_serving_target_entry(
    namespace_id: &ployz_core::ids::NamespaceId,
    service: DeployPlanningService<'_>,
) -> Option<ServingTargetEntry> {
    let namespace_revision_entry_id = service.namespace_revision_entry_id(namespace_id)?;
    let (image, _) = service.concrete_image()?;
    let mut volume_names = service
        .runtime()
        .volume_mounts
        .iter()
        .map(|mount| mount.volume_name.clone())
        .collect::<Vec<_>>();
    volume_names.sort();
    volume_names.dedup();
    Some(ServingTargetEntry {
        namespace_id: namespace_id.clone(),
        service_id: service.service_id().clone(),
        namespace_revision_entry_id,
        image: image.clone(),
        desired_replicas: service.replicas(),
        volume_names,
    })
}

fn provisioned_storage_requirement(
    target: DeployPlanningTarget<'_>,
    service: DeployPlanningService<'_>,
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
