//! Pending operation rows and a progress sink for one Deploy.

use std::collections::BTreeMap;

use ployz_core::{
    ContainerId, DeployEvent, DeployOperation, DeployOutcome, ExecutionError, MachineId,
    MachineName, OperationPhase, OperationRow, OperationStatus, ServiceName,
};
use tokio::sync::mpsc::UnboundedSender;

use super::DeploySnapshot;

/// Pending rows for a planned operation list, with names from this snapshot when known.
#[must_use]
pub fn pending_rows(
    operations: &[DeployOperation],
    snapshot: &DeploySnapshot,
) -> Vec<OperationRow> {
    let machines = machine_names(snapshot);
    let containers = container_labels(snapshot);
    operations
        .iter()
        .enumerate()
        .map(|(index, operation)| {
            let machine_name = machines.get(&operation.machine_id()).cloned();
            let (display_name, service_name) = operation.container_id().map_or_else(
                || (None, operation.service_name().cloned()),
                |container_id| {
                    containers
                        .get(&container_id)
                        .cloned()
                        .unwrap_or_else(|| (None, operation.service_name().cloned()))
                },
            );
            OperationRow::pending(
                u32::try_from(index).unwrap_or(u32::MAX),
                operation.clone(),
                machine_name,
                display_name,
                service_name,
            )
        })
        .collect()
}

fn machine_names(snapshot: &DeploySnapshot) -> BTreeMap<MachineId, MachineName> {
    snapshot
        .machines
        .iter()
        .map(|machine| (machine.machine.id, machine.machine.name.clone()))
        .collect()
}

fn container_labels(
    snapshot: &DeploySnapshot,
) -> BTreeMap<ContainerId, (Option<String>, Option<ServiceName>)> {
    snapshot
        .containers
        .iter()
        .map(|container| {
            (
                container.container_id,
                (
                    Some(container.display_name.clone()),
                    Some(container.service_name.clone()),
                ),
            )
        })
        .collect()
}

pub(super) struct Progress {
    rows: Vec<OperationRow>,
    tx: Option<UnboundedSender<DeployEvent>>,
}

impl Progress {
    pub(super) fn new(rows: Vec<OperationRow>, tx: Option<UnboundedSender<DeployEvent>>) -> Self {
        Self { rows, tx }
    }

    pub(super) fn emit(&self) {
        let Some(tx) = &self.tx else {
            return;
        };
        let completed = self
            .rows
            .iter()
            .filter(|row| matches!(row.status, OperationStatus::Completed))
            .count();
        let _ = tx.send(DeployEvent::Progress {
            completed: u32::try_from(completed).unwrap_or(u32::MAX),
            total: u32::try_from(self.rows.len()).unwrap_or(u32::MAX),
            rows: self.rows.clone(),
        });
    }

    pub(super) fn set_running(&mut self, index: usize, phase: OperationPhase) {
        if let Some(row) = self.rows.get_mut(index) {
            row.status = OperationStatus::Running { phase };
        }
        self.emit();
    }

    pub(super) fn set_display_name(&mut self, index: usize, display_name: String) {
        if let Some(row) = self.rows.get_mut(index) {
            row.display_name = Some(display_name);
        }
    }

    pub(super) fn set_completed(&mut self, index: usize) {
        if let Some(row) = self.rows.get_mut(index) {
            row.status = OperationStatus::Completed;
        }
        self.emit();
    }

    pub(super) fn fail(&mut self, index: usize, error: ExecutionError) {
        if let Some(row) = self.rows.get_mut(index) {
            row.status = OperationStatus::Failed { error };
        }
        for (candidate, row) in self.rows.iter_mut().enumerate() {
            if candidate != index && !matches!(row.status, OperationStatus::Completed) {
                row.status = OperationStatus::Unexecuted;
            }
        }
        self.emit();
    }

    pub(super) fn outcome(&self, outcome: DeployOutcome<ExecutionError>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(DeployEvent::Outcome { outcome });
        }
    }
}
