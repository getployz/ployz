use std::{collections::BTreeSet, fmt};

use ployz_core::{
    ContainerObservation, ContainerRuntimeObservation, DockerVolumeId, DockerVolumeName,
    MachineName, MachineObservation, MachineTarget, ServiceId,
};
use thiserror::Error;

use crate::compose::ComposeProject;

mod apply;
mod exec;
mod pipeline;
mod planning;

pub(crate) use apply::{apply_requested, deploy_project, deploy_scale, deploy_spec};
pub use exec::execute_plan;
pub use pipeline::DeployError;
pub use planning::plan_deploy;
pub use ployz_core::compare_specs;
pub use ployz_core::{
    DeployIntent, DeployOperation, DeployOutcome, DeployPreview, DeployWarning, ExecutionError,
    FailedOperation, HealthFailure, HookFailure, MachineAction, ObservationKind, PlanOptions,
    ReplacementCompensation, ReplacementOperation, RestartAttempt, ServiceAttempt,
};

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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedDockerVolume {
    pub id: DockerVolumeId,
    pub driver: String,
    pub options: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeployPlan {
    pub operations: Vec<DeployOperation>,
}

impl DeployPlan {
    #[must_use]
    pub fn new(operations: Vec<DeployOperation>) -> Self {
        Self { operations }
    }

    pub fn failure_outcome<E>(&self, completed_count: usize, error: E) -> Option<DeployOutcome<E>> {
        Self::failure_outcome_from(&self.operations, completed_count, error)
    }

    pub(super) fn failure_outcome_from<E>(
        operations: &[DeployOperation],
        completed_count: usize,
        error: E,
    ) -> Option<DeployOutcome<E>> {
        let completed = operations.get(..completed_count)?;
        let (failed, unexecuted) = operations.get(completed_count..)?.split_first()?;
        Some(DeployOutcome::Failed {
            completed: completed.to_vec(),
            failed: FailedOperation::Operation {
                operation: failed.clone(),
                error,
            },
            unexecuted: unexecuted.to_vec(),
        })
    }

    pub fn replacement_health_failure_outcome<E>(
        &self,
        completed_count: usize,
        error: E,
        compensation: ReplacementCompensation<E>,
    ) -> Option<DeployOutcome<E>> {
        Self::replacement_health_failure_outcome_from(
            &self.operations,
            completed_count,
            error,
            compensation,
        )
    }

    pub(super) fn replacement_health_failure_outcome_from<E>(
        operations: &[DeployOperation],
        completed_count: usize,
        error: E,
        compensation: ReplacementCompensation<E>,
    ) -> Option<DeployOutcome<E>> {
        let completed = operations.get(..completed_count)?;
        let (failed, unexecuted) = operations.get(completed_count..)?.split_first()?;
        let DeployOperation::ReplaceContainer(operation) = failed else {
            return None;
        };
        Some(DeployOutcome::Failed {
            completed: completed.to_vec(),
            failed: FailedOperation::ReplacementHealth {
                operation: operation.clone(),
                error,
                compensation,
            },
            unexecuted: unexecuted.to_vec(),
        })
    }

    #[must_use]
    pub fn success_outcome<E>(&self) -> DeployOutcome<E> {
        DeployOutcome::Success {
            completed: self.operations.clone(),
        }
    }
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
    #[error("no machines available that satisfy all constraints: {constraints}")]
    NoEligibleMachines { constraints: EliminatingConstraints },
    #[error("service name matches multiple Service IDs: {matches:?}")]
    AmbiguousService { matches: Vec<ServiceId> },
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
/// placement, volumes, service identity, or the apply-set dependency graph
/// cannot produce a plan.
pub fn plan_compose(
    project: &ComposeProject,
    snapshot: &DeploySnapshot,
) -> Result<DeployPlan, PlanError> {
    reject_missing_external_volumes(project, snapshot)?;
    plan_deploy(
        &DeployIntent::from_named_specs(
            &project.services,
            &project.dependencies,
            project
                .services
                .values()
                .map(|spec| ServiceAttempt {
                    name: spec.name.clone(),
                })
                .collect(),
            PlanOptions::default(),
        ),
        snapshot,
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
