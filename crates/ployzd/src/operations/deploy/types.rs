use ployz_core::dataplane::DataplaneMember;
use ployz_core::deploy::{
    DeployCleanupContainer, DeployRequest, DeployServiceRequest, ExistingServiceReplica,
    RegistryCredential,
};
use ployz_core::ids::{
    ContainerId, MachineId, NamespaceRevisionEntryId, NamespaceRevisionId, OperationId, ServiceId,
    StepId,
};
use ployz_core::image::OciPlatform;
use ployz_core::ops::{
    DeployCompletionOutcome, FailureMessage, OperatorHint, RetainedArtifact, RouteHostname,
};
use ployz_core::state::VolumePinState;
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::certificate::GatewayCertificateTarget;

const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionCommand {
    pub(super) operation_id: OperationId,
    pub(super) request: DeployRequest,
    pub(super) services: Vec<DeployServiceExecutionCommand>,
    pub(super) route_binding_removals: Vec<RouteBindingState>,
    pub(super) serving_target_removals: Vec<ServingTargetEntry>,
    pub(super) namespace_cleanup_candidates: Vec<DeployCleanupContainer>,
    pub(super) machine_platforms: BTreeMap<MachineId, OciPlatform>,
    pub(super) dataplane_members: Vec<DataplaneMember>,
    pub(super) custom_certificate_hostnames: Vec<RouteHostname>,
    pub(super) gateway_certificate_targets: Vec<GatewayCertificateTarget>,
    pub(super) unusable_machines: Vec<ployz_core::ops::UnusableMachine>,
    pub(super) step_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployServiceExecutionCommand {
    pub(super) request: DeployServiceRequest,
    pub(super) registry_credential: Option<RegistryCredential>,
    pub(super) route_commits: Vec<RouteBindingState>,
    pub(super) volume_pins: Vec<VolumePinState>,
    pub(super) eligible_machines: Vec<MachineId>,
    pub(super) existing_replicas: Vec<ExistingServiceReplica>,
    pub(super) cleanup_candidates: Vec<DeployCleanupContainer>,
}

impl DeployExecutionCommand {
    #[must_use]
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }

    #[must_use]
    pub fn services(&self) -> &[DeployServiceExecutionCommand] {
        &self.services
    }

    #[must_use]
    pub fn namespace_cleanup_candidates(&self) -> &[DeployCleanupContainer] {
        &self.namespace_cleanup_candidates
    }

    #[must_use]
    pub fn route_binding_removals(&self) -> &[RouteBindingState] {
        &self.route_binding_removals
    }

    #[must_use]
    pub fn serving_target_removals(&self) -> &[ServingTargetEntry] {
        &self.serving_target_removals
    }

    #[must_use]
    pub fn unusable_machines(&self) -> &[ployz_core::ops::UnusableMachine] {
        &self.unusable_machines
    }

    #[must_use]
    pub fn machine_platform(&self, machine_id: &MachineId) -> Option<&OciPlatform> {
        self.machine_platforms.get(machine_id)
    }

    #[must_use]
    pub fn dataplane_machines(&self) -> Vec<MachineId> {
        self.dataplane_members
            .iter()
            .map(|member| member.machine_id.clone())
            .collect()
    }

    #[must_use]
    pub fn custom_certificate_hostnames(&self) -> &[RouteHostname] {
        &self.custom_certificate_hostnames
    }

    #[must_use]
    pub fn gateway_certificate_targets(&self) -> &[GatewayCertificateTarget] {
        &self.gateway_certificate_targets
    }

    #[must_use]
    pub fn step_timeout(&self) -> Duration {
        if self.step_timeout.is_zero() {
            DEFAULT_STEP_TIMEOUT
        } else {
            self.step_timeout
        }
    }
}

impl DeployServiceExecutionCommand {
    #[must_use]
    pub fn registry_credential(&self) -> Option<&RegistryCredential> {
        self.registry_credential.as_ref()
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
    pub fn eligible_machines(&self) -> &[MachineId] {
        &self.eligible_machines
    }

    #[must_use]
    pub fn serving_target_entry_state(&self) -> ServingTargetEntry {
        let mut volume_names = self
            .request
            .runtime
            .volume_mounts
            .iter()
            .map(|mount| mount.volume_name.clone())
            .collect::<Vec<_>>();
        volume_names.sort();
        volume_names.dedup();
        ServingTargetEntry {
            namespace_id: self.request.namespace_id.clone(),
            service_id: self.request.service_id.clone(),
            namespace_revision_entry_id: self.request.namespace_revision_entry_id.clone(),
            image: self.request.image.clone(),
            desired_replicas: self.request.replicas,
            volume_names,
        }
    }

    #[must_use]
    pub fn route_binding_states(&self) -> &[RouteBindingState] {
        &self.route_commits
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionOutcome {
    pub namespace_id: ployz_core::ids::NamespaceId,
    pub namespace_revision_id: NamespaceRevisionId,
    pub containers: Vec<DeployContainer>,
    pub cleanup: Vec<DeployCleanupResult>,
    pub terminal_event: DeployTerminalEvent,
}

impl DeployExecutionOutcome {
    #[must_use]
    pub fn completion_outcome(&self) -> DeployCompletionOutcome {
        DeployCleanupResult::completion_outcome(&self.cleanup)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployCleanupResult {
    Removed(DeployCleanupContainer),
    Failed {
        target: DeployCleanupContainer,
        message: FailureMessage,
    },
}

impl DeployCleanupResult {
    pub(super) fn completion_outcome(cleanup: &[Self]) -> DeployCompletionOutcome {
        if cleanup
            .iter()
            .any(|result| matches!(result, Self::Failed { .. }))
        {
            DeployCompletionOutcome::CompletedWithWarnings
        } else {
            DeployCompletionOutcome::Completed
        }
    }

    pub(super) fn has_failure(cleanup: &[Self]) -> bool {
        cleanup
            .iter()
            .any(|result| matches!(result, Self::Failed { .. }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployTerminalEvent {
    Recorded,
    Missing,
}

/// How a deploy run step produced its running container. Freshly created
/// containers are health-gated before the deploy completes; reused or
/// restarted existing containers already proved themselves and are never
/// re-gated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunContainerDisposition {
    Created,
    Reused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployContainer {
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub step_id: StepId,
    pub requires_docker_healthcheck: bool,
}

impl DeployContainer {
    pub(super) fn retained_artifact(&self) -> RetainedArtifact {
        RetainedArtifact::StartedContainer {
            machine_id: self.machine_id.clone(),
            container_id: self.container_id.clone(),
            log_hint: OperatorHint::try_new(format!("ployz logs {}", self.container_id.as_str()))
                .expect("generated log hint is non-empty"),
        }
    }
}

pub struct DeployExecutionPorts<'a, R, N, H, C, S> {
    pub recorder: &'a mut R,
    pub machine_runtime: &'a mut N,
    pub health_checker: &'a mut H,
    pub certificate_provisioner: &'a mut C,
    pub namespace_state: &'a mut S,
}
