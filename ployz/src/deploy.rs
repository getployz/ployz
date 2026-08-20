use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use ployz_core::{
    BridgeEndpointCapacity, ContainerObservation, ContainerRuntimeObservation, DockerVolumeId,
    DockerVolumeName, IngressHost, IngressLabelTooLong, MachineFailure, MachineId, MachineName,
    MachineObservation, MachineTarget, ProjectName, QualifiedService, RpcError, ServiceObservation,
    derive_services,
};
use thiserror::Error;

use crate::{
    compose::ComposeProject,
    dns::{DomainRequired, ExpandIngressError},
};

mod apply;
mod exec;
mod pipeline;
mod planning;
mod progress;
mod render;

pub(crate) use apply::{
    ConfirmGate, apply_requested, deploy_project, deploy_scale, deploy_spec, remove_project,
};
pub use pipeline::DeployError;
pub(crate) use pipeline::{ReconciliationHints, plan_options};
pub(crate) use planning::capacity::endpoint_capacity_error;
pub use planning::{
    IngressContext, VolumeFate, data_loss_from_plan, plan_project_removal, preview_deploy,
};
pub use ployz_core::compare_specs;
pub use ployz_core::{
    ComposePruneRefusal, DeployEvent, DeployIntent, DeployOperation, DeployOutcome, DeployPreview,
    DeployWarning, ExecutionError, FailedOperation, HealthFailure, HookFailure, MachineAction,
    ObservationKind, OperationPhase, OperationRow, OperationStatus, PlanOptions, PruneRefusal,
    ReplacementCompensation, ReplacementOperation, RestartAttempt, ServiceAttempt,
};

pub(crate) use progress::pending_rows;

fn is_active_runtime(runtime: &ContainerRuntimeObservation) -> bool {
    matches!(
        runtime,
        ContainerRuntimeObservation::Running { .. }
            | ContainerRuntimeObservation::Paused
            | ContainerRuntimeObservation::Restarting
    )
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeploySnapshot {
    pub machines: Vec<MachineObservation>,
    pub containers: Vec<ContainerObservation>,
    pub volumes: Vec<ObservedDockerVolume>,
    /// Target-specific Container listing failures from this observer's fan-out.
    pub container_failures: Vec<MachineFailure<RpcError>>,
    /// Required Container queries that produced no terminal response.
    pub container_omissions: Vec<MachineId>,
    /// Target-specific Docker Volume listing failures from this observer's fan-out.
    pub volume_failures: Vec<MachineFailure<RpcError>>,
    /// Required Docker Volume queries that produced no terminal response.
    pub volume_omissions: Vec<MachineId>,
    /// Fresh targeted bridge observations used only for capacity admission.
    /// Fresh telemetry by Machine, or `None` for snapshot paths that cannot create endpoints.
    pub capacity: Option<BTreeMap<MachineId, BridgeEndpointCapacity>>,
}

impl DeploySnapshot {
    /// Observer-derived Services owned by `project`. Other Projects are excluded.
    #[must_use]
    pub fn services_in(&self, project: &ProjectName) -> Vec<ServiceObservation> {
        derive_services(
            self.containers
                .iter()
                .filter(|container| container.project_name == *project)
                .cloned(),
        )
    }

    /// Completeness relative to this Machine's current visible required fan-out.
    ///
    /// Required targets are Machines whose Membership Observation invites RPC.
    /// A partition may hide a Machine entirely; that absence is not detected here.
    #[must_use]
    pub fn is_observer_complete(&self) -> bool {
        let required = self
            .machines
            .iter()
            .filter(|machine| machine.membership.invites_rpc())
            .map(|machine| machine.machine.id)
            .collect::<BTreeSet<_>>();
        !affects_required(
            &self.container_failures,
            &self.container_omissions,
            &required,
        ) && !affects_required(&self.volume_failures, &self.volume_omissions, &required)
    }
}

fn affects_required(
    failures: &[MachineFailure<RpcError>],
    omissions: &[MachineId],
    required: &BTreeSet<MachineId>,
) -> bool {
    failures
        .iter()
        .any(|failure| required.contains(&failure.machine_id))
        || omissions.iter().any(|machine| required.contains(machine))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedDockerVolume {
    pub id: DockerVolumeId,
    pub driver: String,
    pub options: std::collections::BTreeMap<String, String>,
    pub labels: std::collections::BTreeMap<String, String>,
}

/// Why [`PlanError::NoEligibleMachines`] found zero Machines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EliminatingConstraint {
    NoMachines,
    UnknownPlacement {
        targets: Vec<MachineTarget>,
    },
    MachineDown {
        names: Vec<MachineName>,
    },
    VolumeAlreadyOn {
        volume: DockerVolumeName,
        located_on: Vec<MachineName>,
    },
    VolumeConflictsWithPlacement {
        volume: DockerVolumeName,
        located_on: Vec<MachineName>,
        requested: Vec<MachineTarget>,
    },
    SharedVolumeNoCommonMachine {
        volume: DockerVolumeName,
        requested: Vec<MachineTarget>,
    },
}

/// Display list of [`EliminatingConstraint`] values for [`PlanError::NoEligibleMachines`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EliminatingConstraints(Vec<EliminatingConstraint>);

impl EliminatingConstraints {
    /// Build from the constraints that emptied the remaining Machine set.
    #[must_use]
    pub fn new(constraints: Vec<EliminatingConstraint>) -> Self {
        Self(if constraints.is_empty() {
            vec![EliminatingConstraint::NoMachines]
        } else {
            constraints
        })
    }

    /// Constraints in display order.
    #[must_use]
    pub fn as_slice(&self) -> &[EliminatingConstraint] {
        &self.0
    }
}

fn write_quoted<T: fmt::Display>(f: &mut fmt::Formatter<'_>, items: &[T]) -> fmt::Result {
    let mut first = true;
    for item in items {
        if !first {
            f.write_str(", ")?;
        }
        first = false;
        write!(f, "'{item}'")?;
    }
    Ok(())
}

fn write_machine_names(f: &mut fmt::Formatter<'_>, names: &[MachineName]) -> fmt::Result {
    match names {
        [name] => write!(f, "Machine '{name}'"),
        _ => {
            f.write_str("Machines ")?;
            write_quoted(f, names)
        }
    }
}

impl fmt::Display for EliminatingConstraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMachines => f.write_str("no Machines in the Deploy Snapshot"),
            Self::UnknownPlacement { targets } => {
                f.write_str("x-machines ")?;
                write_quoted(f, targets)?;
                f.write_str(" matched no Machine")
            }
            Self::MachineDown { names } => {
                write_machine_names(f, names)?;
                if names.len() == 1 {
                    f.write_str(" is down")
                } else {
                    f.write_str(" are down")
                }
            }
            Self::VolumeAlreadyOn { volume, located_on } => {
                write!(f, "Docker Volume '{volume}' is already on ")?;
                write_machine_names(f, located_on)
            }
            Self::VolumeConflictsWithPlacement {
                volume,
                located_on,
                requested,
            } => {
                write!(f, "Docker Volume '{volume}' is already on ")?;
                write_machine_names(f, located_on)?;
                f.write_str(", which conflicts with x-machines ")?;
                write_quoted(f, requested)
            }
            Self::SharedVolumeNoCommonMachine { volume, requested } => {
                f.write_str("x-machines ")?;
                write_quoted(f, requested)?;
                write!(f, " have no Machine in common for Docker Volume '{volume}'")
            }
        }
    }
}

impl fmt::Display for EliminatingConstraints {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for constraint in &self.0 {
            if !first {
                f.write_str("; ")?;
            }
            first = false;
            write!(f, "{constraint}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PlanError {
    /// At least one relevant Machine did not return a fresh capacity observation.
    #[error("capacity unknown: an eligible Machine did not return fresh bridge telemetry")]
    CapacityUnknown,
    /// Fresh observations confirm the requested operations cannot fit.
    #[error("insufficient capacity on observed eligible Machines")]
    InsufficientCapacity,
    #[error("no machines available that satisfy all constraints: {constraints}")]
    NoEligibleMachines { constraints: EliminatingConstraints },
    #[error("service mode cannot be changed")]
    ServiceModeCannotChange,
    #[error("mounted Service Volumes disagree about Docker Volume {name}")]
    ConflictingDockerVolumeDefinitions { name: DockerVolumeName },
    #[error("plan service '{service}': {source}")]
    Service {
        service: String,
        #[source]
        source: Box<PlanError>,
    },
    #[error(
        "Docker Volume {name} cannot be shared by global service '{global}' and replicated service '{replicated}'"
    )]
    MixedVolumeModes {
        name: DockerVolumeName,
        global: String,
        replicated: String,
    },
    #[error("dependency cycle at service '{service}'")]
    DependencyCycle { service: String },
    #[error("external volumes not found: {}", quoted_names(.names))]
    ExternalVolumesNotFound { names: Vec<DockerVolumeName> },
    #[error("hostname {hostname} is already published by {owner}")]
    HostnameConflict {
        hostname: IngressHost,
        owner: QualifiedService,
    },
    #[error(transparent)]
    DomainRequired(#[from] DomainRequired),
    #[error(transparent)]
    GeneratedLabel(#[from] IngressLabelTooLong),
}

impl From<ExpandIngressError> for PlanError {
    fn from(error: ExpandIngressError) -> Self {
        match error {
            ExpandIngressError::DomainRequired(error) => Self::DomainRequired(error),
            ExpandIngressError::LabelTooLong(error) => Self::GeneratedLabel(error),
        }
    }
}

fn quoted_names(names: &[DockerVolumeName]) -> String {
    let mut quoted = String::new();
    for (index, name) in names.iter().enumerate() {
        if index > 0 {
            quoted.push_str(", ");
        }
        quoted.push('\'');
        quoted.push_str(name.as_str());
        quoted.push('\'');
    }
    quoted
}

/// Plan a Compose project: fail if any `external: true` volume is missing from
/// the snapshot, then plan service operations.
///
/// # Errors
///
/// Returns when an external volume is absent from every Machine, or when
/// placement, volumes, service identity, hostname assignment, or the apply-set
/// dependency graph cannot produce a preview.
pub fn plan_compose(
    project: &ComposeProject,
    snapshot: &DeploySnapshot,
    project_name: ProjectName,
) -> Result<DeployPreview, PlanError> {
    reject_missing_external_volumes(project, snapshot)?;
    preview_deploy(
        &DeployIntent::from_named_specs(
            project_name,
            &project.services,
            &project.dependencies,
            PlanOptions::default(),
        )
        .with_service_profiles(project.service_profiles()),
        snapshot,
        IngressContext::default(),
    )
}

pub(crate) fn reject_missing_external_volumes(
    project: &ComposeProject,
    snapshot: &DeploySnapshot,
) -> Result<(), PlanError> {
    let present = snapshot
        .volumes
        .iter()
        .map(|volume| &volume.id.name)
        .collect::<BTreeSet<_>>();
    let names = project
        .external_volume_names()
        .filter(|name| !present.contains(name))
        .collect::<Vec<_>>();
    if names.is_empty() {
        Ok(())
    } else {
        Err(PlanError::ExternalVolumesNotFound { names })
    }
}

impl PlanError {
    pub(crate) fn no_eligible_machines(constraints: Vec<EliminatingConstraint>) -> Self {
        Self::NoEligibleMachines {
            constraints: EliminatingConstraints::new(constraints),
        }
    }
}
