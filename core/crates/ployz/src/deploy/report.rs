//! Direct execution evidence, grouped by the same RowTracker the Store uses.

use crate::{
    connect::Client,
    failure::Failure,
    ui::progress::{Detail, Diagnostic, Frame, LogTail, Row, Run, State, Subject, Timing},
};
use ployz_core::{
    ContainerId, DeployOperation, DeployOutcome, ExecutionError, FailedOperation, MachineId,
    Namespace, OperationPhase, OperationRow, OperationStatus, QualifiedService,
    ReplacementCompensation, RestartAttempt, RpcError, RpcErrorCode, ServiceName, StopAttempt,
};
use ployz_store::{RowState, RowTracker, ServerProgress};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

/// Factual Service/Server progress plus standalone operations and failed Container identity.
pub(super) struct Direct {
    namespace: Namespace,
    title: String,
    tracker: RowTracker,
    rows: BTreeMap<(ServiceName, MachineId), (ServerProgress, Timing)>,
    pub(super) operations: Vec<OperationRow>,
    notices: Vec<ployz_core::DeployWarning>,
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
        let mut rows: Vec<_> = self
            .rows
            .values()
            .map(|(row, timing)| Row {
                subject: Subject::ServiceOnServer {
                    service: QualifiedService::new(self.namespace.clone(), row.service.clone()),
                    machine: row.machine,
                    server: row.server.clone(),
                },
                state: (&row.state).into(),
                detail: None,
                timing: timing.clone(),
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

    pub(super) async fn tails(&self, client: &Client) -> Vec<LogTail> {
        let mut tails = Vec::new();
        for (row, _) in self
            .rows
            .values()
            .filter(|(row, _)| matches!(row.state, RowState::Failed { .. }))
        {
            let lines = if let Some(container) = self.tracker.container(row) {
                log_tail(client, row.machine, container).await
            } else {
                Vec::new()
            };
            tails.push(LogTail {
                service: QualifiedService::new(self.namespace.clone(), row.service.clone()),
                machine: row.machine,
                server: row.server.clone(),
                lines,
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

async fn log_tail(client: &Client, machine: MachineId, container: ContainerId) -> Vec<String> {
    let read = async {
        let request = ployz_core::op::TailLogs::into_request(ployz_core::TailLogsRequest {
            target: ployz_core::LiveLogTarget::Container(container),
            options: ployz_core::LogsOptions {
                follow: false,
                tail: 10,
                since_nanos: None,
                until_unix_seconds: None,
            },
        })
        .encode()
        .ok()?;
        let mut stream = client
            .tail_logs_stream(&ployz_core::MachineTarget::from(&machine), request)
            .await
            .ok()?;
        let mut lines = std::collections::VecDeque::new();
        while let Ok(Some(payload)) = stream.message().await {
            let entry = ployz_core::LogEntry::decode(&payload).ok()?;
            let bytes = match entry.body {
                ployz_core::LogBody::Stdout(bytes) | ployz_core::LogBody::Stderr(bytes) => bytes,
                ployz_core::LogBody::Heartbeat | ployz_core::LogBody::Error(_) => continue,
            };
            for line in String::from_utf8_lossy(&bytes).lines() {
                if lines.len() == ployz_store::LOG_TAIL {
                    lines.pop_front();
                }
                lines.push_back(line.to_owned());
            }
        }
        Some(lines.into_iter().collect())
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
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
