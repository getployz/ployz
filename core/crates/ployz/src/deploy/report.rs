//! Direct execution evidence, grouped by the same RowTracker the Store uses.

use crate::{
    connect::Client,
    failure::Failure,
    ui::progress::{
        Detail, Diagnostic, Frame, GraphService, LogTail, Row, Run, StartFailure, State, Subject,
        Timing, Work,
    },
};
use ployz_core::{
    ContainerId, DeployOperation, DeployOutcome, ExecutionError, FailedOperation, HealthFailure,
    MachineAction, MachineId, Namespace, OperationPhase, OperationRow, OperationStatus,
    QualifiedService, ReplacementCompensation, RestoreAttempt, RpcError, RpcErrorCode, ServiceName,
    StopAttempt,
};
use ployz_store::tools::SealedValues;
use ployz_store::{RowPhase, RowState, RowTracker, ServerProgress, StoppedBy, starts_container};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

use super::ServiceGroup;

/// The depth at which each Service starts: 0 without a dependency in this
/// Deploy, else one more than its deepest dependency. `groups` come in
/// execution order, so every dependency is placed before its dependents.
pub(super) fn depths(groups: &[ServiceGroup]) -> BTreeMap<ServiceName, u32> {
    let mut depths = BTreeMap::new();
    for group in groups {
        let Some(service) = &group.service else {
            continue;
        };
        let depth = group
            .dependencies
            .iter()
            .filter_map(|dependency| depths.get(dependency))
            .max()
            .map_or(0, |deepest: &u32| deepest + 1);
        depths.insert(service.clone(), depth);
    }
    depths
}

/// Each `(dependency, dependent)` pair of Services this Deploy runs, in the order
/// `groups` place them.
pub(super) fn dependencies(groups: &[ServiceGroup]) -> Vec<(ServiceName, ServiceName)> {
    let deployed: BTreeSet<&ServiceName> = groups
        .iter()
        .filter_map(|group| group.service.as_ref())
        .collect();
    groups
        .iter()
        .filter_map(|group| Some((group.service.as_ref()?, &group.dependencies)))
        .flat_map(|(service, dependencies)| {
            dependencies
                .iter()
                .filter(|dependency| deployed.contains(dependency))
                .map(|dependency| (dependency.clone(), service.clone()))
        })
        .collect()
}

pub(crate) struct Replica<'a> {
    pub machine: MachineId,
    pub state: &'a RowState,
    pub starts: bool,
    /// The copy keeps serving under its own id and takes new limits in place.
    pub updates: bool,
    pub timing: Timing,
}

pub(crate) fn starts(
    operations: &[OperationRow],
    service: &ServiceName,
    machine: MachineId,
) -> bool {
    operations.iter().any(|row| {
        row.machine_id == machine
            && row.service_name() == Some(service)
            && starts_container(&row.operation)
    })
}

pub(crate) fn updates(
    operations: &[OperationRow],
    service: &ServiceName,
    machine: MachineId,
) -> bool {
    operations.iter().any(|row| {
        row.machine_id == machine
            && row.service_name() == Some(service)
            && matches!(row.operation, DeployOperation::UpdateContainer(_))
    })
}

const fn stops_or_removes(phase: RowPhase) -> bool {
    matches!(
        phase,
        RowPhase::StoppingContainer | RowPhase::RemovingContainer | RowPhase::RemovingVolume
    )
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Step {
    Start,
    Clear,
    Switch,
    Retire,
}

/// `placed_on` is the Servers the plan leaves the Service on, when the plan is known.
/// The row counts them once the Service completed, since only then do they hold.
pub(crate) fn service_row<'a>(
    service: QualifiedService,
    depth: Option<u32>,
    placed_on: Option<&BTreeSet<MachineId>>,
    replicas: impl IntoIterator<Item = Replica<'a>>,
) -> Row {
    let replicas: Vec<_> = replicas.into_iter().collect();
    let removal =
        depth.is_none() && !replicas.is_empty() && replicas.iter().all(|replica| !replica.starts);
    let served = !removal
        && replicas.iter().any(|replica| {
            replica.starts
                && matches!(
                    replica.state,
                    RowState::Completed | RowState::Running { serving: true, .. }
                )
        });
    let failed = replicas
        .iter()
        .filter(|replica| {
            matches!(
                replica.state,
                RowState::Failed {
                    stopped_by: StoppedBy::NOTHING,
                    ..
                }
            )
        })
        .count();
    let counted: Vec<_> = replicas
        .iter()
        .filter(|replica| removal || replica.starts)
        .collect();
    // A failure cancels the switch, so a copy waiting on it will never serve.
    // A copy under an in-place update serves throughout, and is ready once done.
    let is_ready = |replica: &Replica<'_>| match replica.state {
        RowState::Running {
            phase: RowPhase::UpdatingContainer,
            ..
        } => false,
        RowState::Completed | RowState::Running { serving: true, .. } => true,
        RowState::Running {
            phase: RowPhase::WaitingForSwitch,
            ..
        } => failed == 0,
        RowState::Running { .. }
        | RowState::Pending
        | RowState::Failed { .. }
        | RowState::NotAttempted
        | RowState::Unknown => false,
    };
    let ready = counted.iter().filter(|replica| is_ready(replica)).count();
    let in_place = counted.iter().filter(|replica| replica.updates).count();
    let in_place_ready = counted
        .iter()
        .filter(|replica| replica.updates && is_ready(replica))
        .count();
    let switching = failed == 0 && ready == counted.len();
    let work = |replica: &Replica<'_>| match replica.state {
        RowState::Running { phase, serving } => Some((
            if *phase == RowPhase::UpdatingContainer {
                Step::Start
            } else if *serving || (served && !replica.starts) {
                Step::Retire
            } else if stops_or_removes(*phase) {
                Step::Clear
            } else if *phase == RowPhase::WaitingForSwitch {
                if !switching {
                    return None;
                }
                Step::Switch
            } else {
                Step::Start
            },
            *phase,
        )),
        RowState::Pending
        | RowState::Completed
        | RowState::Failed { .. }
        | RowState::NotAttempted
        | RowState::Unknown => None,
    };
    let all = |test: fn(&RowState) -> bool| replicas.iter().all(|replica| test(replica.state));
    let any_stopped_by = |by: StoppedBy| {
        replicas.iter().any(
            |replica| matches!(replica.state, RowState::Failed { stopped_by, .. } if *stopped_by == by),
        )
    };
    let running = replicas
        .iter()
        .filter_map(work)
        .min_by_key(|(work, _)| *work);
    let draining = replicas
        .iter()
        .filter(|replica| match replica.state {
            RowState::Pending => served && !replica.starts,
            RowState::Running {
                phase: RowPhase::UpdatingContainer,
                ..
            } => false,
            RowState::Running { serving, .. } => *serving || (served && !replica.starts),
            RowState::Completed
            | RowState::Failed { .. }
            | RowState::NotAttempted
            | RowState::Unknown => false,
        })
        .count();
    let old_wait_for_routes = running.is_none() && ready == counted.len() && draining > 0;
    // Every new copy switched and only old copies' removals never ran, as when a
    // later failure aborted the run (#3684).
    let switched = !removal
        && !counted.is_empty()
        && replicas.iter().all(|replica| match replica.state {
            RowState::Completed => true,
            RowState::NotAttempted => !replica.starts,
            RowState::Pending
            | RowState::Running { .. }
            | RowState::Failed { .. }
            | RowState::Unknown => false,
        });
    let state = if failed > 0 {
        running.map_or(State::Failed, |(_, phase)| State::Running(phase))
    } else if replicas
        .iter()
        .any(|replica| matches!(replica.state, RowState::Unknown))
    {
        State::Unknown
    } else if let Some((_, phase)) = running {
        State::Running(phase)
    } else if old_wait_for_routes {
        State::Running(RowPhase::RemovingContainer)
    } else if all(|state| matches!(state, RowState::Pending)) {
        State::Pending
    } else if all(|state| matches!(state, RowState::Completed)) || switched {
        State::Completed
    } else if any_stopped_by(StoppedBy::CANCEL) {
        State::Cancelled
    } else if any_stopped_by(StoppedBy::LOST_RUNNER) {
        State::Interrupted
    } else if replicas
        .iter()
        .any(|replica| matches!(replica.state, RowState::NotAttempted))
    {
        State::NotAttempted
    } else if removal {
        State::Running(RowPhase::RemovingContainer)
    } else {
        State::Running(RowPhase::Starting)
    };
    let started = replicas
        .iter()
        .filter_map(|replica| match replica.timing {
            Timing::Started(at) => Some(at),
            Timing::Unavailable | Timing::Finished(_) => None,
        })
        .min();
    let timing = match started {
        Some(at) => Timing::Started(at),
        None if !replicas.is_empty()
            && replicas
                .iter()
                .all(|replica| matches!(replica.timing, Timing::Finished(_))) =>
        {
            Timing::Finished(
                replicas
                    .iter()
                    .filter_map(|replica| match replica.timing {
                        Timing::Finished(elapsed) => Some(elapsed),
                        Timing::Unavailable | Timing::Started(_) => None,
                    })
                    .max()
                    .unwrap_or(Duration::ZERO),
            )
        }
        None => Timing::Unavailable,
    };
    Row {
        subject: Subject::Service(GraphService {
            service,
            depth,
            ready,
            total: counted.len(),
            in_place,
            in_place_ready,
            draining,
            work: if removal {
                Work::Remove
            } else if old_wait_for_routes || running.is_some_and(|(step, _)| step == Step::Retire) {
                Work::Retire
            } else {
                Work::Start
            },
            machines: match placed_on {
                Some(placed_on) if state == State::Completed && !removal => placed_on.clone(),
                _ => replicas.iter().map(|replica| replica.machine).collect(),
            },
        }),
        detail: (failed > 0 && state.active()).then(|| format!("{failed} failed")),
        state,
        timing,
    }
}

/// Factual Service/Server progress plus standalone operations and failed Container identity.
pub(super) struct Direct {
    namespace: Namespace,
    title: String,
    tracker: RowTracker,
    rows: BTreeMap<(ServiceName, MachineId), (ServerProgress, Timing)>,
    pub(super) operations: Vec<OperationRow>,
    notices: Vec<ployz_core::DeployWarning>,
    depths: BTreeMap<ServiceName, u32>,
    placed_on: BTreeMap<ServiceName, BTreeSet<MachineId>>,
}

impl Direct {
    pub(super) fn new(preview: &super::DeployPreview, title: String) -> Self {
        let mut direct = Self {
            namespace: preview.namespace.clone(),
            title,
            tracker: RowTracker::default(),
            rows: BTreeMap::new(),
            operations: Vec::new(),
            notices: preview.warnings.clone(),
            depths: preview.depths.clone(),
            placed_on: preview.placed_on.clone(),
        };
        direct.observe(&preview.operations);
        direct
    }

    pub(super) fn observe(&mut self, operations: &[OperationRow]) {
        for row in self.tracker.changes(operations) {
            let key = (row.service.clone(), row.machine);
            let prior = self.rows.get(&key).map(|(_, timing)| timing);
            let started = match prior {
                Some(Timing::Started(at)) => Some(*at),
                Some(Timing::Finished(_)) | Some(Timing::Unavailable) | None => None,
            };
            let timing = match row.state {
                RowState::Pending => Timing::Unavailable,
                RowState::Running { .. } => {
                    Timing::Started(started.unwrap_or_else(SystemTime::now))
                }
                RowState::Completed
                | RowState::Failed { .. }
                | RowState::NotAttempted
                | RowState::Unknown => started.map_or(Timing::Unavailable, |at| {
                    Timing::Finished(at.elapsed().unwrap_or_default())
                }),
            };
            self.rows.insert(key, (row, timing));
        }
        self.operations = operations.to_vec();
    }

    /// Whether the plan or the run named a Server it could not fully observe.
    pub(super) fn has_observation_gaps(&self) -> bool {
        self.notices
            .iter()
            .any(|warning| warning.observation_gap().is_some())
    }

    /// Show `warning` with the deploy's other notices, once.
    pub(super) fn notice(&mut self, warning: ployz_core::DeployWarning) {
        if !self.notices.contains(&warning) {
            self.notices.push(warning);
        }
    }

    pub(super) fn frame(&self) -> Frame {
        let services: BTreeSet<_> = self.rows.keys().map(|(service, _)| service).collect();
        let mut rows: Vec<_> = services
            .into_iter()
            .map(|service| {
                service_row(
                    QualifiedService::new(self.namespace.clone(), service.clone()),
                    self.depths.get(service).copied(),
                    self.placed_on.get(service),
                    self.rows
                        .values()
                        .filter(|(row, _)| row.service == *service)
                        .map(|(row, timing)| Replica {
                            machine: row.machine,
                            state: &row.state,
                            starts: starts(&self.operations, service, row.machine),
                            updates: updates(&self.operations, service, row.machine),
                            timing: timing.clone(),
                        }),
                )
            })
            .collect();
        rows.extend(
            self.operations
                .iter()
                .filter(|row| row.service_name().is_none())
                .map(|row| Row {
                    subject: Subject::Operation {
                        index: row.index,
                        machine: row.machine_id,
                        server: row
                            .machine_name
                            .as_ref()
                            .map_or_else(|| row.machine_id.to_string(), ToString::to_string),
                        name: visible_row_name(row),
                    },
                    state: match &row.status {
                        OperationStatus::Pending => State::Pending,
                        OperationStatus::Running { phase } => State::Running(phase.into()),
                        OperationStatus::Completed => State::Completed,
                        OperationStatus::Failed { .. } => State::Failed,
                        OperationStatus::Unexecuted => State::NotAttempted,
                    },
                    detail: None,
                    timing: Timing::Unavailable,
                }),
        );
        Frame {
            run: Run::Direct(self.namespace.clone()),
            title: self.title.clone(),
            rows,
            notices: self.notices.clone(),
        }
    }

    /// Each failed row's Container tail with secret-shaped text redacted, giving the
    /// stopped Container `outcome` failed on the last run its tail shows. A direct apply
    /// has no Store, so it has no Sealed values to scrub.
    pub(super) async fn tails(
        &self,
        client: &Client,
        outcome: &mut DeployOutcome<ExecutionError>,
    ) -> Vec<LogTail> {
        let mut stopped = None;
        if let DeployOutcome::Failed {
            failed:
                FailedOperation::Operation { error, .. } | FailedOperation::Replacement { error, .. },
            ..
        } = outcome
            && let ExecutionError::Health {
                container_id,
                failure: HealthFailure::Runtime { .. },
                last_run,
                ..
            } = error
        {
            stopped = Some((*container_id, last_run));
        }
        let mut tails = Vec::new();
        for (row, _) in self
            .rows
            .values()
            .filter(|(row, _)| matches!(row.state, RowState::Failed { .. }))
        {
            let log = match self.tracker.container(row) {
                None => None,
                Some(container) => {
                    let (log, run) = crate::sdk::failed_tail(
                        client,
                        row.machine,
                        container,
                        &SealedValues::none(),
                    )
                    .await;
                    if let Some((_, last_run)) = stopped.as_mut().filter(|(id, _)| *id == container)
                    {
                        **last_run = Some(run);
                    }
                    Some(log)
                }
            };
            tails.push(LogTail {
                service: QualifiedService::new(self.namespace.clone(), row.service.clone()),
                name: row.service.clone(),
                machine: row.machine,
                server: row.server.clone(),
                log,
            });
        }
        tails
    }
}

pub(super) fn failure(
    outcome: &DeployOutcome<ExecutionError>,
    operations: &[OperationRow],
    tails: Vec<LogTail>,
) -> Failure {
    let DeployOutcome::Failed { failed, .. } = outcome else {
        return Failure::coded(
            RpcErrorCode::Internal,
            "Deployment failed without failure evidence.",
        );
    };
    let (service, machine, error) = match failed {
        FailedOperation::Operation { operation, error } => {
            (operation.service_name(), operation.machine_id(), error)
        }
        FailedOperation::Replacement {
            operation, error, ..
        } => (Some(&operation.spec.name), operation.machine_id, error),
    };
    let ployz_store::Failure { reason, cause, .. } =
        ployz_store::Failure::on(error, operations, Some(machine));
    let message = crate::ui::sentence(match service {
        Some(service) => format!("{service}: {reason}"),
        None => reason,
    });
    let lost_to_newer = matches!(error, ExecutionError::NewerServing { .. });
    let fresh = matches!(
        failed,
        FailedOperation::Operation {
            operation: DeployOperation::RunContainer { .. },
            ..
        }
    );
    let start_failure = match error {
        ExecutionError::Machine {
            action: MachineAction::CreateContainer,
            error,
        } if fresh => std::iter::once(&error.message)
            .chain(&error.cause)
            .any(|cause| crate::ingress::port_taken(cause))
            .then_some(StartFailure::PortTaken),
        ExecutionError::Health { failure, .. }
            if fresh && !matches!(failure, HealthFailure::Cancelled) =>
        {
            Some(StartFailure::Unhealthy)
        }
        ExecutionError::Machine { .. }
        | ExecutionError::Health { .. }
        | ExecutionError::DependencyHealth { .. }
        | ExecutionError::Hook { .. }
        | ExecutionError::NewerServing { .. }
        | ExecutionError::Cancelled => None,
    };
    let mut diagnostic = Diagnostic {
        failed_machine: (!lost_to_newer).then_some(machine),
        start_failure,
        logs: if lost_to_newer { Vec::new() } else { tails },
        ..Diagnostic::default()
    };
    if let FailedOperation::Replacement { compensation, .. } = failed {
        let stop = |attempt: &StopAttempt<ExecutionError>| Detail {
            message: attempt.line().into(),
            causes: match attempt {
                StopAttempt::Stopped => Vec::new(),
                StopAttempt::Failed { error } => failure_causes(error),
            },
        };
        match compensation {
            ReplacementCompensation::OldUntouched { stop_new_container } => {
                diagnostic.compensation.push(stop(stop_new_container));
            }
            ReplacementCompensation::OldStopped {
                stop_new_container,
                restore_old_container,
            } => {
                diagnostic
                    .compensation
                    .extend(stop_new_container.iter().map(stop));
                diagnostic.compensation.push(Detail {
                    message: restore_old_container.line().into(),
                    causes: match restore_old_container {
                        RestoreAttempt::Failed { error } => failure_causes(error),
                        RestoreAttempt::Restored
                        | RestoreAttempt::LeftHeld
                        | RestoreAttempt::NotServing
                        | RestoreAttempt::NewerServing => Vec::new(),
                    },
                });
            }
        }
    }
    Failure::from(RpcError {
        code: if lost_to_newer {
            RpcErrorCode::Conflict
        } else {
            RpcErrorCode::Internal
        },
        message,
        cause,
        details: serde_json::json!({"outcome": outcome}),
    })
    .with_diagnostic(diagnostic)
}

fn failure_causes(error: &ExecutionError) -> Vec<String> {
    let failure = ployz_store::Failure::of(error, &[]);
    std::iter::once(failure.reason)
        .chain(failure.cause)
        .collect()
}

pub(super) fn visible_row_name(row: &OperationRow) -> String {
    visible_name(
        row.display_name.as_deref(),
        &row.operation,
        live_container_id(row).as_ref(),
    )
}

fn visible_name(
    display: Option<&str>,
    operation: &DeployOperation,
    live_id: Option<&ContainerId>,
) -> String {
    if let Some(display) = display.filter(|name| !is_hex_len(name, 64)) {
        return display.to_owned();
    }
    match operation {
        DeployOperation::PrepareVolumes { .. } => "Managed volumes".into(),
        DeployOperation::WaitHealthy { dependency, .. } => dependency.to_string(),
        DeployOperation::RunContainer { spec, .. } | DeployOperation::RunHook { spec, .. } => {
            spec.name.to_string()
        }
        DeployOperation::ReplaceContainer(replacement) => replacement.spec.name.to_string(),
        DeployOperation::UpdateContainer(update) => update.spec.name.to_string(),
        DeployOperation::RemoveVolume { id } => id.name.to_string(),
        DeployOperation::StopContainer { container_id, .. }
        | DeployOperation::RemoveContainer { container_id, .. }
        | DeployOperation::StopHook { container_id, .. } => {
            crate::ui::short_id(live_id.unwrap_or(container_id).as_str()).to_owned()
        }
    }
}

fn live_container_id(row: &OperationRow) -> Option<ContainerId> {
    match &row.status {
        OperationStatus::Running {
            phase:
                OperationPhase::WaitingForHealth { container_id, .. }
                | OperationPhase::WaitingForHook { container_id, .. }
                | OperationPhase::WaitingForSwitch { container_id },
        } => Some(*container_id),
        OperationStatus::Failed {
            error:
                ExecutionError::Health { container_id, .. } | ExecutionError::Hook { container_id, .. },
        } => Some(*container_id),
        OperationStatus::Pending
        | OperationStatus::Running { .. }
        | OperationStatus::Completed
        | OperationStatus::Failed { .. }
        | OperationStatus::Unexecuted => row.operation.container_id(),
    }
}

fn is_hex_len(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
