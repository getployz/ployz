//! Direct execution evidence, grouped by the same RowTracker the Store uses.

use crate::{
    connect::Client,
    failure::Failure,
    ui::progress::{
        Detail, Diagnostic, Frame, GraphService, LogTail, Row, Run, State, Subject, Timing,
    },
};
use ployz_core::{
    ContainerId, DeployOperation, DeployOutcome, ExecutionError, FailedOperation, MachineId,
    Namespace, OperationPhase, OperationRow, OperationStatus, QualifiedService,
    ReplacementCompensation, RequestedServiceSpec, RestartAttempt, RpcError, RpcErrorCode,
    ServiceName, StopAttempt,
};
use ployz_store::tools::SecretSet;
use ployz_store::{RowPhase, RowState, RowTracker, ServerProgress};
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

/// One replica's recorded work, for [`service_row`].
pub(crate) struct Replica<'a> {
    pub machine: MachineId,
    pub state: &'a RowState,
    pub timing: Timing,
}

/// One row for a Service's replicas: failed when any failed, booting while any
/// work runs, switched once all completed, and not started when the Deploy
/// stopped before any of it ran. A Service with no recorded work is waiting.
pub(crate) fn service_row<'a>(
    service: QualifiedService,
    depth: Option<u32>,
    replicas: impl IntoIterator<Item = Replica<'a>>,
) -> Row {
    let replicas: Vec<_> = replicas.into_iter().collect();
    let draining = |state: &RowState| {
        matches!(
            state,
            RowState::Running {
                phase: RowPhase::StoppingContainer | RowPhase::RemovingContainer
            }
        )
    };
    let all = |test: fn(&RowState) -> bool| replicas.iter().all(|replica| test(replica.state));
    let phases: Vec<_> = replicas
        .iter()
        .filter_map(|replica| match replica.state {
            RowState::Running { phase } => Some((draining(replica.state), *phase)),
            RowState::Pending
            | RowState::Completed
            | RowState::Failed { .. }
            | RowState::NotAttempted
            | RowState::Unknown => None,
        })
        .collect();
    let running = phases
        .iter()
        .find(|(draining, _)| !draining)
        .or_else(|| phases.first());
    let state = if replicas
        .iter()
        .any(|replica| matches!(replica.state, RowState::Failed { .. }))
    {
        State::Failed
    } else if replicas
        .iter()
        .any(|replica| matches!(replica.state, RowState::Unknown))
    {
        State::Unknown
    } else if let Some((_, phase)) = running {
        State::Running(*phase)
    } else if all(|state| matches!(state, RowState::Pending)) {
        State::Pending
    } else if all(|state| matches!(state, RowState::Completed)) {
        State::Completed
    } else if replicas
        .iter()
        .any(|replica| matches!(replica.state, RowState::NotAttempted))
    {
        State::NotAttempted
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
            ready: replicas
                .iter()
                .filter(|replica| matches!(replica.state, RowState::Completed))
                .count(),
            total: replicas.len(),
            draining: replicas
                .iter()
                .filter(|replica| draining(replica.state))
                .count(),
            machines: replicas.iter().map(|replica| replica.machine).collect(),
        }),
        state,
        detail: None,
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

    pub(super) fn frame(&self) -> Frame {
        let services: BTreeSet<_> = self.rows.keys().map(|(service, _)| service).collect();
        let mut rows: Vec<_> = services
            .into_iter()
            .map(|service| {
                service_row(
                    QualifiedService::new(self.namespace.clone(), service.clone()),
                    Some(self.depths.get(service).copied().unwrap_or(0)),
                    self.rows
                        .values()
                        .filter(|(row, _)| row.service == *service)
                        .map(|(row, timing)| Replica {
                            machine: row.machine,
                            state: &row.state,
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

    /// Each failed row's scrubbed Container tail. Every value of `requested`'s
    /// environment counts as a secret: a direct apply has no Store to say otherwise.
    pub(super) async fn tails(
        &self,
        client: &Client,
        requested: &RequestedServiceSpec,
    ) -> Vec<LogTail> {
        let mut tails = Vec::new();
        for (row, _) in self
            .rows
            .values()
            .filter(|(row, _)| matches!(row.state, RowState::Failed { .. }))
        {
            let log = match self.tracker.container(row) {
                None => None,
                Some(container) => {
                    let secrets = if row.service == requested.name {
                        SecretSet::from_store(requested.container.environment.clone(), [])
                    } else {
                        SecretSet::unknown()
                    };
                    Some(crate::sdk::failed_tail(client, row.machine, container, secrets).await)
                }
            };
            tails.push(LogTail {
                service: QualifiedService::new(self.namespace.clone(), row.service.clone()),
                machine: row.machine,
                server: row.server.clone(),
                log,
            });
        }
        tails
    }
}

pub(super) fn failure(outcome: &DeployOutcome<ExecutionError>, tails: Vec<LogTail>) -> Failure {
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
    let stored = ployz_store::Failure::from(error);
    let message = service.map_or_else(
        || "Deployment failed.".to_owned(),
        |service| format!("Deployment of {service} failed."),
    );
    let mut causes = vec![stored.reason];
    causes.extend(stored.cause);
    let mut diagnostic = Diagnostic {
        failed_machine: Some(machine),
        logs: tails,
        ..Diagnostic::default()
    };
    if let FailedOperation::Replacement { compensation, .. } = failed {
        let stop = |attempt: &StopAttempt<ExecutionError>| match attempt {
            StopAttempt::Stopped => Detail {
                message: "Stopped the new Container.".into(),
                causes: Vec::new(),
            },
            StopAttempt::Failed { error } => Detail {
                message: "Could not stop the new Container.".into(),
                causes: failure_causes(error),
            },
        };
        match compensation {
            ReplacementCompensation::OldUntouched { stop_new_container } => {
                diagnostic.compensation.push(stop(stop_new_container))
            }
            ReplacementCompensation::OldStopped {
                stop_new_container,
                restart_old_container,
            } => {
                diagnostic
                    .compensation
                    .extend(stop_new_container.iter().map(stop));
                diagnostic.compensation.push(match restart_old_container {
                    RestartAttempt::Restarted => Detail {
                        message: "Restarted the old Container.".into(),
                        causes: Vec::new(),
                    },
                    RestartAttempt::Failed { error } => Detail {
                        message: "Could not restart the old Container.".into(),
                        causes: failure_causes(error),
                    },
                });
            }
        }
    }
    Failure::from(RpcError {
        code: RpcErrorCode::Internal,
        message,
        cause: causes,
        details: serde_json::json!({"outcome": outcome}),
    })
    .with_diagnostic(diagnostic)
}

fn failure_causes(error: &ExecutionError) -> Vec<String> {
    let failure = ployz_store::Failure::from(error);
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
                | OperationPhase::WaitingForHook { container_id, .. },
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
