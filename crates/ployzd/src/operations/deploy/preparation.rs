//! Convert current cluster facts into a deploy execution command.

use ployz_core::cert::ManagedLeaseName;
use ployz_core::dataplane::DataplaneMember;
use ployz_core::deploy::{
    DeployCleanupContainer, DeployPreparationInput, DeployRequest, RegistryCredential,
    auto_hostname_route_binding_commits, namespace_route_binding_removals,
    namespace_serving_target_removals, prepare_deploy,
};
use ployz_core::ids::{MachineId, OperationId, ServiceId};
use ployz_core::image::OciPlatform;
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::ops::RouteHostname;
use ployz_core::state::VolumePinState;
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::certificate::GatewayCertificateTarget;

use super::{DeployExecutionCommand, DeployServiceExecutionCommand};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionFacts {
    pub namespace_route_bindings: Vec<RouteBindingState>,
    pub namespace_serving_entries: Vec<ServingTargetEntry>,
    pub namespace_volume_pins: Vec<VolumePinState>,
    pub eligible_machines: Vec<MachineId>,
    pub unusable_machines: Vec<ployz_core::ops::UnusableMachine>,
    pub dataplane_members: Vec<DataplaneMember>,
    pub observed_machines: Vec<MachineContainerObservationSnapshot>,
    pub machine_platforms: BTreeMap<MachineId, OciPlatform>,
    pub namespace_cleanup_candidates: Vec<DeployCleanupContainer>,
    pub managed_lease: Option<ManagedLeaseName>,
    pub gateway_certificate_targets: Vec<GatewayCertificateTarget>,
    pub step_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionInput {
    pub(super) operation_id: OperationId,
    pub(super) request: DeployRequest,
    pub(super) facts: DeployExecutionFacts,
    pub(super) registry_credentials: BTreeMap<ServiceId, RegistryCredential>,
}

impl DeployExecutionInput {
    #[must_use]
    pub fn new(
        operation_id: OperationId,
        request: DeployRequest,
        facts: DeployExecutionFacts,
        registry_credentials: BTreeMap<ServiceId, RegistryCredential>,
    ) -> Self {
        Self {
            operation_id,
            request,
            facts,
            registry_credentials,
        }
    }

    #[must_use]
    pub fn with_step_timeout(mut self, step_timeout: Duration) -> Self {
        self.facts.step_timeout = step_timeout;
        self
    }
}

#[must_use]
pub fn prepare_deploy_execution_command(
    operation_id: OperationId,
    request: DeployRequest,
    facts: DeployExecutionFacts,
) -> DeployExecutionCommand {
    prepare_deploy_execution_command_with_credentials(
        operation_id,
        request,
        facts,
        &BTreeMap::new(),
    )
}

#[must_use]
pub(super) fn prepare_deploy_execution_command_with_credentials(
    operation_id: OperationId,
    request: DeployRequest,
    facts: DeployExecutionFacts,
    registry_credentials: &BTreeMap<ServiceId, RegistryCredential>,
) -> DeployExecutionCommand {
    let service_requests = request.service_requests();
    let mut mint_requests = service_requests.iter().collect::<Vec<_>>();
    mint_requests.sort_by(|left, right| left.service_id.cmp(&right.service_id));
    let mut declared_auto_bindings = Vec::new();
    let mut occupied_bindings = facts.namespace_route_bindings.clone();
    for service_request in mint_requests {
        let commits = auto_hostname_route_binding_commits(
            service_request,
            facts.managed_lease.as_ref(),
            &occupied_bindings,
        );
        occupied_bindings.extend(commits.iter().cloned());
        declared_auto_bindings.extend(commits);
    }
    let namespace_declared_targets = request
        .services
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
        .services
        .iter()
        .map(|service| service.service_id.clone())
        .collect::<Vec<_>>();
    let route_binding_removals = namespace_route_binding_removals(
        &request.namespace_id,
        &namespace_declared_targets,
        &facts.namespace_route_bindings,
    );
    let serving_target_removals = namespace_serving_target_removals(
        &request.namespace_id,
        &declared_services,
        &facts.namespace_serving_entries,
    );
    let draining_machines = facts
        .unusable_machines
        .iter()
        .filter(|unusable| match unusable.reason {
            ployz_core::state::MachineUsabilityReason::Draining => true,
            ployz_core::state::MachineUsabilityReason::FactsUnavailable => false,
        })
        .map(|unusable| unusable.machine_id.clone())
        .collect::<Vec<_>>();
    let mut services = Vec::new();
    for service_request in service_requests {
        let mut prepared = prepare_deploy(DeployPreparationInput {
            request: service_request,
            eligible_machines: facts.eligible_machines.clone(),
            draining_machines: draining_machines.clone(),
            observed_machines: facts.observed_machines.clone(),
        });
        let is_promoted = facts.namespace_serving_entries.iter().any(|entry| {
            entry.namespace_id == prepared.request.namespace_id
                && entry.service_id == prepared.request.service_id
                && entry.namespace_revision_entry_id == prepared.request.namespace_revision_entry_id
        });
        if !is_promoted {
            prepared.existing_replicas.clear();
        }
        let mut route_commits = prepared.route_commits;
        route_commits.extend(
            declared_auto_bindings
                .iter()
                .filter(|binding| binding.service_id == prepared.request.service_id)
                .cloned(),
        );
        services.push(DeployServiceExecutionCommand {
            registry_credential: registry_credentials
                .get(&prepared.request.service_id)
                .cloned(),
            request: prepared.request,
            route_commits,
            volume_pins: facts.namespace_volume_pins.clone(),
            eligible_machines: prepared.eligible_machines,
            existing_replicas: prepared.existing_replicas,
            cleanup_candidates: prepared.cleanup_candidates,
        });
    }
    let custom_certificate_hostnames =
        custom_certificate_hostnames(&services, facts.managed_lease.as_ref());

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
        machine_platforms: facts.machine_platforms,
        dataplane_members: facts.dataplane_members,
        custom_certificate_hostnames,
        gateway_certificate_targets: facts.gateway_certificate_targets,
        unusable_machines: facts.unusable_machines,
        step_timeout: facts.step_timeout,
    }
}

fn custom_certificate_hostnames(
    services: &[DeployServiceExecutionCommand],
    managed_lease: Option<&ManagedLeaseName>,
) -> Vec<RouteHostname> {
    services
        .iter()
        .flat_map(DeployServiceExecutionCommand::route_binding_states)
        .filter(|binding| binding.target.port.get() == 443)
        .map(|binding| &binding.target.hostname)
        .filter(|hostname| {
            !managed_lease.is_some_and(|lease| managed_bundle_covers(lease, hostname))
        })
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn managed_bundle_covers(lease: &ManagedLeaseName, hostname: &RouteHostname) -> bool {
    let suffix = lease.hostname_suffix();
    hostname.as_str() == suffix
        || hostname
            .as_str()
            .strip_suffix(&format!(".{suffix}"))
            .is_some_and(|label| !label.is_empty() && !label.contains('.'))
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
