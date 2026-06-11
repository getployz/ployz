//! Read-only query runtimes behind the operation API: machine, service,
//! logs, and operation-status reads. Nothing here writes cluster truth.

use crate::controllers::OperationControllers;
use crate::node_rpc::{NatsNodeLogsTailer, NodeLogsTailRuntimeError};
use crate::node_runtime_types::NodeLogsTailRequest as NodeRuntimeLogsTailRequest;
use ployz_core::ids::{ContainerId, NodeId, OperationId};
use ployz_core::ops::{
    OperationEventReplayPage, OperationEventReplayRequest, OperationStatusSnapshot,
};
use ployz_core::state::{ActiveMachineState, ActiveServiceState};
use ployz_nats::core_state::{ActiveMachineReadError, AsyncNatsCoreStateStore};
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_sdk_types::{
    LogsTailError, LogsTailRequest, LogsTailResult, LogsTailUnavailableSource, MachineInspectError,
    MachineListError, MachineListResult, MachineQueryUnavailableSource, MachineSnapshot,
    OpsStatusError, OpsStatusUnavailableSource, OpsWatchError, ServiceInspectError,
    ServiceListError, ServiceListResult, ServiceQueryUnavailableSource, ServiceSnapshot,
};
use std::collections::BTreeMap;

use super::error_map::{ops_watch_error_from_replay_error, status_store_read_failure};

#[derive(Clone)]
pub struct MachineQueryRuntime {
    core_state: AsyncNatsCoreStateStore,
    observations: AsyncNatsObservationStore,
}

#[derive(Clone)]
pub struct ServiceQueryRuntime {
    core_state: AsyncNatsCoreStateStore,
}

#[derive(Clone)]
pub struct LogsQueryRuntime {
    observations: AsyncNatsObservationStore,
    tailer: NatsNodeLogsTailer,
}

impl LogsQueryRuntime {
    #[must_use]
    pub(crate) const fn new(
        observations: AsyncNatsObservationStore,
        tailer: NatsNodeLogsTailer,
    ) -> Self {
        Self {
            observations,
            tailer,
        }
    }

    pub(crate) async fn tail(
        &self,
        request: LogsTailRequest,
    ) -> Result<LogsTailResult, LogsTailError> {
        let node_id = match request.node_id.clone() {
            Some(node_id) => {
                self.verify_observed_container_on_node(&node_id, &request.container_id)
                    .await?;
                node_id
            }
            None => self
                .find_container_node(&request.container_id)
                .await?
                .ok_or_else(|| LogsTailError::NoSuchContainer {
                    container_id: request.container_id.clone(),
                })?,
        };

        self.tailer
            .tail_logs(NodeRuntimeLogsTailRequest {
                node_id,
                container_id: request.container_id,
                tail_lines: request.tail_lines.map(|lines| lines.get()),
            })
            .await
            .map(|value| LogsTailResult {
                node_id: value.node_id,
                container_id: value.container_id,
                text: value.text,
                truncated: value.truncated,
            })
            .map_err(logs_tail_node_error)
    }

    async fn find_container_node(
        &self,
        container_id: &ContainerId,
    ) -> Result<Option<NodeId>, LogsTailError> {
        let mut matches = self
            .observations
            .node_snapshot_records()
            .await
            .map_err(|_| LogsTailError::Unavailable {
                source: LogsTailUnavailableSource::Observations,
                node_id: None,
            })?
            .into_iter()
            .filter_map(|record| {
                record
                    .snapshot
                    .container(container_id)
                    .map(|_| record.snapshot.node_id().clone())
            })
            .collect::<Vec<_>>();
        matches.sort();
        matches.dedup();

        match matches.as_slice() {
            [] => Ok(None),
            [node_id] => Ok(Some(node_id.clone())),
            node_ids => Err(LogsTailError::AmbiguousContainer {
                container_id: container_id.clone(),
                node_ids: node_ids.to_vec(),
            }),
        }
    }

    async fn verify_observed_container_on_node(
        &self,
        node_id: &NodeId,
        container_id: &ContainerId,
    ) -> Result<(), LogsTailError> {
        let Some(snapshot) = self
            .observations
            .node_snapshot(node_id)
            .await
            .map_err(|_| LogsTailError::Unavailable {
                source: LogsTailUnavailableSource::Observations,
                node_id: Some(node_id.clone()),
            })?
        else {
            return Err(LogsTailError::NoSuchContainer {
                container_id: container_id.clone(),
            });
        };
        if snapshot.container(container_id).is_none() {
            return Err(LogsTailError::NoSuchContainer {
                container_id: container_id.clone(),
            });
        }
        Ok(())
    }
}

impl ServiceQueryRuntime {
    #[must_use]
    pub(crate) const fn new(core_state: AsyncNatsCoreStateStore) -> Self {
        Self { core_state }
    }

    pub(crate) async fn list(&self) -> Result<ServiceListResult, ServiceListError> {
        let services = self
            .core_state
            .active_services()
            .await
            .map_err(service_list_core_error)?
            .into_iter()
            .map(service_snapshot)
            .collect();
        Ok(ServiceListResult { services })
    }

    pub(crate) async fn inspect(
        &self,
        service_id: &ployz_core::ids::ServiceId,
    ) -> Result<ServiceSnapshot, ServiceInspectError> {
        let Some(active) = self
            .core_state
            .active_service(service_id)
            .await
            .map_err(service_inspect_core_error)?
        else {
            return Err(ServiceInspectError::NoSuchService {
                service_id: service_id.clone(),
            });
        };

        Ok(service_snapshot(active))
    }
}

fn service_snapshot(active: ActiveServiceState) -> ServiceSnapshot {
    ServiceSnapshot { active }
}

impl MachineQueryRuntime {
    #[must_use]
    pub(crate) fn new(
        core_state: AsyncNatsCoreStateStore,
        observations: AsyncNatsObservationStore,
    ) -> Self {
        Self {
            core_state,
            observations,
        }
    }

    pub(crate) async fn list(&self) -> Result<MachineListResult, MachineListError> {
        let machines = self
            .core_state
            .active_machines()
            .await
            .map_err(machine_list_core_error)?;
        let public_ips = self
            .observations
            .node_public_ips()
            .await
            .map_err(|_| MachineListError::Unavailable {
                source: MachineQueryUnavailableSource::Observations,
            })?
            .into_iter()
            .map(|observation| (observation.node_id.clone(), observation))
            .collect::<BTreeMap<_, _>>();
        let gateway_statuses = self
            .observations
            .gateway_statuses()
            .await
            .map_err(|_| MachineListError::Unavailable {
                source: MachineQueryUnavailableSource::Observations,
            })?
            .into_iter()
            .map(|observation| (observation.node_id.clone(), observation))
            .collect::<BTreeMap<_, _>>();
        let container_counts = self
            .observations
            .node_snapshot_records()
            .await
            .map_err(|_| MachineListError::Unavailable {
                source: MachineQueryUnavailableSource::Observations,
            })?
            .into_iter()
            .map(|record| {
                (
                    record.snapshot.node_id().clone(),
                    record.snapshot.containers().len(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut snapshots = Vec::with_capacity(machines.len());
        for machine in machines {
            snapshots.push(MachineSnapshot {
                public_ip: public_ips.get(&machine.node_id).cloned(),
                gateway: gateway_statuses.get(&machine.node_id).cloned(),
                observed_container_count: container_counts
                    .get(&machine.node_id)
                    .copied()
                    .unwrap_or_default(),
                active: machine,
            });
        }
        Ok(MachineListResult {
            machines: snapshots,
        })
    }

    pub(crate) async fn inspect(
        &self,
        node_id: &NodeId,
    ) -> Result<MachineSnapshot, MachineInspectError> {
        let Some(machine) = self
            .core_state
            .active_machine(node_id)
            .await
            .map_err(machine_inspect_core_error)?
        else {
            return Err(MachineInspectError::NoSuchMachine {
                node_id: node_id.clone(),
            });
        };

        self.snapshot(machine).await.map_err(machine_inspect_error)
    }

    async fn snapshot(
        &self,
        active: ActiveMachineState,
    ) -> Result<MachineSnapshot, MachineSnapshotError> {
        let public_ip = self
            .observations
            .node_public_ip(&active.node_id)
            .await
            .map_err(|_| MachineSnapshotError::Observations)?;
        let gateway = self
            .observations
            .gateway_status(&active.node_id)
            .await
            .map_err(|_| MachineSnapshotError::Observations)?;
        let observed_container_count = self
            .observations
            .node_snapshot(&active.node_id)
            .await
            .map_err(|_| MachineSnapshotError::Observations)?
            .map(|snapshot| snapshot.containers().len())
            .unwrap_or_default();

        Ok(MachineSnapshot {
            active,
            public_ip,
            gateway,
            observed_container_count,
        })
    }
}

enum MachineSnapshotError {
    Observations,
}

fn logs_tail_node_error(error: NodeLogsTailRuntimeError) -> LogsTailError {
    match error {
        NodeLogsTailRuntimeError::NotFound { container_id, .. } => {
            LogsTailError::NoSuchContainer { container_id }
        }
        NodeLogsTailRuntimeError::ReadFailed {
            node_id,
            container_id,
            message,
        } => LogsTailError::ReadFailed {
            node_id,
            container_id,
            message,
        },
        NodeLogsTailRuntimeError::Unavailable { node_id, .. } => LogsTailError::Unavailable {
            source: LogsTailUnavailableSource::NodeRpc,
            node_id: Some(node_id),
        },
    }
}

fn machine_list_core_error(_error: ActiveMachineReadError) -> MachineListError {
    MachineListError::Unavailable {
        source: MachineQueryUnavailableSource::CoreState,
    }
}

fn machine_inspect_core_error(_error: ActiveMachineReadError) -> MachineInspectError {
    MachineInspectError::Unavailable {
        source: MachineQueryUnavailableSource::CoreState,
    }
}

fn machine_inspect_error(error: MachineSnapshotError) -> MachineInspectError {
    match error {
        MachineSnapshotError::Observations => MachineInspectError::Unavailable {
            source: MachineQueryUnavailableSource::Observations,
        },
    }
}

fn service_list_core_error(
    _error: ployz_nats::core_state::CoreStateStoreError,
) -> ServiceListError {
    ServiceListError::Unavailable {
        source: ServiceQueryUnavailableSource::CoreState,
    }
}

fn service_inspect_core_error(
    _error: ployz_nats::core_state::CoreStateStoreError,
) -> ServiceInspectError {
    ServiceInspectError::Unavailable {
        source: ServiceQueryUnavailableSource::CoreState,
    }
}

#[must_use]
pub fn ops_status_missing(operation_id: &OperationId) -> OpsStatusError {
    OpsStatusError::NoSuchOperation {
        operation_id: operation_id.clone(),
    }
}

pub async fn ops_status(
    controllers: &OperationControllers,
    operation_id: OperationId,
) -> Result<OperationStatusSnapshot, OpsStatusError> {
    match controllers.operation_status_snapshot(&operation_id).await {
        Ok(Some(snapshot)) => Ok(snapshot),
        Ok(None) => Err(ops_status_missing(&operation_id)),
        Err(error) => Err(OpsStatusError::Unavailable {
            operation_id,
            source: OpsStatusUnavailableSource::StatusStore {
                failure: status_store_read_failure(&error),
            },
        }),
    }
}

pub async fn ops_watch(
    controllers: &OperationControllers,
    request: OperationEventReplayRequest,
) -> Result<OperationEventReplayPage, OpsWatchError> {
    let operation_id = request.operation_id.clone();
    controllers
        .repository()
        .replay_operation_events(request)
        .await
        .map_err(|error| ops_watch_error_from_replay_error(operation_id, error))
}
