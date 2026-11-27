//! A Deploy graph's Service rows, folded from each copy's Store row.

use crate::ui::progress::{GraphService, Row, State, Subject, Timing, Work};
use ployz_core::{
    DeployOperation, InPlaceChange, MachineId, OperationRow, QualifiedService, ServiceName,
};
use ployz_store::{RowPhase, RowState, StoppedBy};
use std::collections::BTreeSet;
use std::time::Duration;

pub(crate) struct Replica<'a> {
    pub machine: MachineId,
    pub state: &'a RowState,
    pub starts: bool,
    /// What the copy takes in place, when it keeps serving under its own id.
    pub updates: Option<InPlaceChange>,
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
            && row.operation.serves_new_spec()
    })
}

pub(crate) fn updates(
    operations: &[OperationRow],
    service: &ServiceName,
    machine: MachineId,
) -> Option<InPlaceChange> {
    operations
        .iter()
        .filter(|row| row.machine_id == machine && row.service_name() == Some(service))
        .find_map(|row| {
            if let DeployOperation::UpdateContainer(update) = &row.operation {
                Some(update.change)
            } else {
                None
            }
        })
}

const fn stops_or_removes(phase: RowPhase) -> bool {
    matches!(
        phase,
        RowPhase::StoppingContainer | RowPhase::RemovingContainer | RowPhase::RemovingVolume
    )
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
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
                    stopped_by: StoppedBy::Nothing,
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
    let in_place = counted
        .iter()
        .filter(|replica| replica.updates.is_some())
        .count();
    let in_place_ready = counted
        .iter()
        .filter(|replica| replica.updates.is_some() && is_ready(replica))
        .count();
    let in_place_change = counted
        .iter()
        .filter_map(|replica| replica.updates)
        .min()
        .unwrap_or_default();
    let switching = failed == 0 && ready == counted.len();
    let work = |replica: &Replica<'_>| match replica.state {
        RowState::Running { phase, serving } => Some((
            if *phase == RowPhase::UpdatingContainer {
                Phase::Start
            } else if *serving || (served && !replica.starts) {
                Phase::Retire
            } else if stops_or_removes(*phase) {
                Phase::Clear
            } else if *phase == RowPhase::WaitingForSwitch {
                if !switching {
                    return None;
                }
                Phase::Switch
            } else {
                Phase::Start
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
    } else if any_stopped_by(StoppedBy::Cancel) {
        State::Cancelled
    } else if any_stopped_by(StoppedBy::LostRunner) {
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
            in_place_change,
            draining,
            work: if removal {
                Work::Remove
            } else if old_wait_for_routes || running.is_some_and(|(step, _)| step == Phase::Retire)
            {
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

#[cfg(test)]
#[path = "rows_tests.rs"]
mod tests;
