//! Read-only query runtimes behind the operation API: machine, service,
//! logs, and operation-status reads. Nothing here writes cluster truth.

use crate::controllers::OperationControllers;
use crate::machine_runtime::client::{MachineLogsTailRuntimeError, NatsMachineLogsTailer};
use crate::machine_runtime::protocol::MachineLogsTailRpcRequest;
use ployz_core::ids::{ContainerId, MachineId, NamespaceId, OperationId, RevisionId, ServiceId};
use ployz_core::machine_runtime::{ManagedContainerKind, ManagedContainerObservation};
use ployz_core::ops::{
    OperationEventReplayPage, OperationEventReplayRequest, OperationStatusSnapshot,
};
use ployz_core::state::{ActiveMachineState, ActiveRouteState, ActiveServiceState};
use ployz_nats::core_state::{ActiveMachineReadError, AsyncNatsCoreStateStore};
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_sdk_types::{
    LogsTailError, LogsTailRequest, LogsTailResult, LogsTailUnavailableSource, MachineInspectError,
    MachineListError, MachineListResult, MachineQueryUnavailableSource, MachineSnapshot,
    OpsListError, OpsListRequest, OpsListResult, OpsStatusError, OpsStatusUnavailableSource,
    OpsWatchError, RuntimeDerivedCollectionSource, RuntimeDerivedCollectionStatus,
    RuntimeProjectionSource, RuntimeProjectionSources, RuntimeServiceInstance,
    RuntimeServiceRelease, RuntimeServiceRevision, RuntimeSnapshot, RuntimeSnapshotError,
    RuntimeSnapshotResult, RuntimeSnapshotUnavailableSource, ServiceInspectError, ServiceListError,
    ServiceListResult, ServiceQueryUnavailableSource, ServiceSnapshot,
};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use super::error_map::{ops_watch_error_from_replay_error, status_read_failure};

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
    tailer: NatsMachineLogsTailer,
}

#[derive(Clone)]
pub struct RuntimeSnapshotQueryRuntime {
    core_state: AsyncNatsCoreStateStore,
    observations: AsyncNatsObservationStore,
}

impl RuntimeSnapshotQueryRuntime {
    #[must_use]
    pub(crate) const fn new(
        core_state: AsyncNatsCoreStateStore,
        observations: AsyncNatsObservationStore,
    ) -> Self {
        Self {
            core_state,
            observations,
        }
    }

    pub(crate) async fn snapshot(&self) -> Result<RuntimeSnapshotResult, RuntimeSnapshotError> {
        let read_at_unix_seconds = current_unix_seconds();
        let machine_query =
            MachineQueryRuntime::new(self.core_state.clone(), self.observations.clone());
        let service_query = ServiceQueryRuntime::new(self.core_state.clone());
        let machines = machine_query
            .list()
            .await
            .map_err(runtime_machine_error)?
            .machines;
        let services = service_query
            .list()
            .await
            .map_err(runtime_service_error)?
            .services;
        let routes = self.core_state.active_routes().await.map_err(|_| {
            RuntimeSnapshotError::Unavailable {
                source: RuntimeSnapshotUnavailableSource::CoreState,
            }
        })?;
        let containers = self
            .observations
            .machine_snapshot_records()
            .await
            .map_err(|_| RuntimeSnapshotError::Unavailable {
                source: RuntimeSnapshotUnavailableSource::Observations,
            })?
            .into_iter()
            .flat_map(|record| record.snapshot.containers().to_vec())
            .collect::<Vec<_>>();
        let revisions = derive_runtime_revisions(&services, &routes, &containers);
        let releases = derive_runtime_releases(&services, &routes);
        let instances = derive_runtime_instances(&services, &routes, &containers);
        let missing_link_count = missing_runtime_links(&services, &routes, &containers);

        Ok(RuntimeSnapshotResult {
            snapshot: RuntimeSnapshot {
                machines,
                services,
                routes,
                containers,
                projection_sources: RuntimeProjectionSources {
                    core_state: RuntimeProjectionSource {
                        read_at_unix_seconds,
                    },
                    observations: RuntimeProjectionSource {
                        read_at_unix_seconds,
                    },
                    revisions: derived_source(revisions.len(), missing_link_count),
                    releases: derived_source(releases.len(), missing_link_count),
                    instances: derived_source(instances.len(), missing_link_count),
                },
                revisions,
                releases,
                instances,
                updated_at_unix_seconds: read_at_unix_seconds,
            },
        })
    }
}

impl LogsQueryRuntime {
    #[must_use]
    pub(crate) const fn new(
        observations: AsyncNatsObservationStore,
        tailer: NatsMachineLogsTailer,
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
        let machine_id = match request.machine_id.clone() {
            Some(machine_id) => {
                self.verify_observed_container_on_machine(&machine_id, &request.container_id)
                    .await?;
                machine_id
            }
            None => self
                .find_container_machine(&request.container_id)
                .await?
                .ok_or_else(|| LogsTailError::NoSuchContainer {
                    container_id: request.container_id.clone(),
                })?,
        };

        self.tailer
            .tail_logs(
                &machine_id,
                MachineLogsTailRpcRequest {
                    container_id: request.container_id,
                    tail_lines: request.tail_lines.map(|lines| lines.get()),
                },
            )
            .await
            .map(|value| LogsTailResult {
                machine_id: value.machine_id,
                container_id: value.container_id,
                text: value.text,
                truncated: value.truncated,
            })
            .map_err(logs_tail_machine_error)
    }

    async fn find_container_machine(
        &self,
        container_id: &ContainerId,
    ) -> Result<Option<MachineId>, LogsTailError> {
        let mut matches = self
            .observations
            .machine_snapshot_records()
            .await
            .map_err(|_| LogsTailError::Unavailable {
                source: LogsTailUnavailableSource::Observations,
                machine_id: None,
            })?
            .into_iter()
            .filter_map(|record| {
                record
                    .snapshot
                    .container(container_id)
                    .map(|_| record.snapshot.machine_id().clone())
            })
            .collect::<Vec<_>>();
        matches.sort();
        matches.dedup();

        match matches.as_slice() {
            [] => Ok(None),
            [machine_id] => Ok(Some(machine_id.clone())),
            machine_ids => Err(LogsTailError::AmbiguousContainer {
                container_id: container_id.clone(),
                machine_ids: machine_ids.to_vec(),
            }),
        }
    }

    async fn verify_observed_container_on_machine(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<(), LogsTailError> {
        let Some(snapshot) = self
            .observations
            .machine_snapshot(machine_id)
            .await
            .map_err(|_| LogsTailError::Unavailable {
                source: LogsTailUnavailableSource::Observations,
                machine_id: Some(machine_id.clone()),
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

fn derive_runtime_revisions(
    services: &[ServiceSnapshot],
    routes: &[ActiveRouteState],
    containers: &[ManagedContainerObservation],
) -> Vec<RuntimeServiceRevision> {
    let namespaces = runtime_service_namespaces(services, routes);
    let mut revisions = BTreeSet::new();
    for service in services {
        revisions.insert((
            service.active.namespace_id.clone(),
            service.active.service_id.clone(),
            service.active.active_revision.clone(),
        ));
    }
    for route in routes {
        revisions.insert((
            route.namespace_id.clone(),
            route.service_id.clone(),
            route.revision_id.clone(),
        ));
    }
    for container in containers {
        let Some(namespace_id) = namespaces.get(&container.service_id) else {
            continue;
        };
        revisions.insert((
            namespace_id.clone(),
            container.service_id.clone(),
            container.revision_id.clone(),
        ));
    }

    revisions
        .into_iter()
        .map(
            |(namespace_id, service_id, revision_id)| RuntimeServiceRevision {
                namespace_id,
                service_id,
                revision_id,
            },
        )
        .collect()
}

fn derive_runtime_releases(
    services: &[ServiceSnapshot],
    routes: &[ActiveRouteState],
) -> Vec<RuntimeServiceRelease> {
    let mut releases = BTreeMap::<(NamespaceId, ServiceId, RevisionId), Vec<_>>::new();
    for service in services {
        releases
            .entry((
                service.active.namespace_id.clone(),
                service.active.service_id.clone(),
                service.active.active_revision.clone(),
            ))
            .or_default();
    }
    for route in routes {
        releases
            .entry((
                route.namespace_id.clone(),
                route.service_id.clone(),
                route.revision_id.clone(),
            ))
            .or_default()
            .push(route.target.clone());
    }

    releases
        .into_iter()
        .map(
            |((namespace_id, service_id, revision_id), routes)| RuntimeServiceRelease {
                namespace_id,
                service_id,
                revision_id,
                routes,
            },
        )
        .collect()
}

fn derive_runtime_instances(
    services: &[ServiceSnapshot],
    routes: &[ActiveRouteState],
    containers: &[ManagedContainerObservation],
) -> Vec<RuntimeServiceInstance> {
    let namespaces = runtime_service_namespaces(services, routes);
    containers
        .iter()
        .filter(|container| container.kind == ManagedContainerKind::Service)
        .filter_map(|container| {
            let namespace_id = namespaces.get(&container.service_id)?;
            Some(RuntimeServiceInstance {
                namespace_id: namespace_id.clone(),
                machine_id: container.machine_id.clone(),
                container_id: container.container_id.clone(),
                service_id: container.service_id.clone(),
                revision_id: container.revision_id.clone(),
                operation_id: container.operation_id.clone(),
                step_id: container.step_id.clone(),
                state: container.state.clone(),
            })
        })
        .collect()
}

fn missing_runtime_links(
    services: &[ServiceSnapshot],
    routes: &[ActiveRouteState],
    containers: &[ManagedContainerObservation],
) -> usize {
    let service_ids = services
        .iter()
        .map(|service| service.active.service_id.clone())
        .collect::<BTreeSet<_>>();

    routes
        .iter()
        .filter(|route| !service_ids.contains(&route.service_id))
        .count()
        + containers
            .iter()
            .filter(|container| !service_ids.contains(&container.service_id))
            .count()
}

fn runtime_service_namespaces(
    services: &[ServiceSnapshot],
    routes: &[ActiveRouteState],
) -> BTreeMap<ServiceId, NamespaceId> {
    let mut namespaces = BTreeMap::new();
    for service in services {
        namespaces.insert(
            service.active.service_id.clone(),
            service.active.namespace_id.clone(),
        );
    }
    for route in routes {
        namespaces
            .entry(route.service_id.clone())
            .or_insert_with(|| route.namespace_id.clone());
    }
    namespaces
}

fn derived_source(
    source_count: usize,
    missing_link_count: usize,
) -> RuntimeDerivedCollectionSource {
    RuntimeDerivedCollectionSource {
        status: if missing_link_count == 0 {
            RuntimeDerivedCollectionStatus::Complete
        } else {
            RuntimeDerivedCollectionStatus::Partial
        },
        source_count,
        missing_link_count,
    }
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
            .machine_public_ips()
            .await
            .map_err(|_| MachineListError::Unavailable {
                source: MachineQueryUnavailableSource::Observations,
            })?
            .into_iter()
            .map(|observation| (observation.machine_id.clone(), observation))
            .collect::<BTreeMap<_, _>>();
        let gateway_statuses = self
            .observations
            .gateway_statuses()
            .await
            .map_err(|_| MachineListError::Unavailable {
                source: MachineQueryUnavailableSource::Observations,
            })?
            .into_iter()
            .map(|observation| (observation.machine_id.clone(), observation))
            .collect::<BTreeMap<_, _>>();
        let container_counts = self
            .observations
            .machine_snapshot_records()
            .await
            .map_err(|_| MachineListError::Unavailable {
                source: MachineQueryUnavailableSource::Observations,
            })?
            .into_iter()
            .map(|record| {
                (
                    record.snapshot.machine_id().clone(),
                    record.snapshot.containers().len(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut snapshots = Vec::with_capacity(machines.len());
        for machine in machines {
            snapshots.push(MachineSnapshot {
                public_ip: public_ips.get(&machine.machine_id).cloned(),
                gateway: gateway_statuses.get(&machine.machine_id).cloned(),
                observed_container_count: container_counts
                    .get(&machine.machine_id)
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
        machine_id: &MachineId,
    ) -> Result<MachineSnapshot, MachineInspectError> {
        let Some(machine) = self
            .core_state
            .active_machine(machine_id)
            .await
            .map_err(machine_inspect_core_error)?
        else {
            return Err(MachineInspectError::NoSuchMachine {
                machine_id: machine_id.clone(),
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
            .machine_public_ip(&active.machine_id)
            .await
            .map_err(|_| MachineSnapshotError::Observations)?;
        let gateway = self
            .observations
            .gateway_status(&active.machine_id)
            .await
            .map_err(|_| MachineSnapshotError::Observations)?;
        let observed_container_count = self
            .observations
            .machine_snapshot(&active.machine_id)
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

fn logs_tail_machine_error(error: MachineLogsTailRuntimeError) -> LogsTailError {
    match error {
        MachineLogsTailRuntimeError::NotFound { container_id, .. } => {
            LogsTailError::NoSuchContainer { container_id }
        }
        MachineLogsTailRuntimeError::ReadFailed {
            machine_id,
            container_id,
            message,
        } => LogsTailError::ReadFailed {
            machine_id,
            container_id,
            message,
        },
        MachineLogsTailRuntimeError::Unavailable { machine_id, .. } => LogsTailError::Unavailable {
            source: LogsTailUnavailableSource::MachineRpc,
            machine_id: Some(machine_id),
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

fn runtime_machine_error(error: MachineListError) -> RuntimeSnapshotError {
    match error {
        MachineListError::Unavailable { source } => RuntimeSnapshotError::Unavailable {
            source: match source {
                MachineQueryUnavailableSource::CoreState => {
                    RuntimeSnapshotUnavailableSource::CoreState
                }
                MachineQueryUnavailableSource::Observations => {
                    RuntimeSnapshotUnavailableSource::Observations
                }
            },
        },
    }
}

fn runtime_service_error(error: ServiceListError) -> RuntimeSnapshotError {
    match error {
        ServiceListError::Unavailable { source } => RuntimeSnapshotError::Unavailable {
            source: match source {
                ServiceQueryUnavailableSource::CoreState => {
                    RuntimeSnapshotUnavailableSource::CoreState
                }
            },
        },
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
                failure: status_read_failure(&error),
            },
        }),
    }
}

pub async fn ops_list(
    controllers: &OperationControllers,
    request: OpsListRequest,
) -> Result<OpsListResult, OpsListError> {
    let operations = controllers
        .operation_statuses()
        .await
        .map_err(|error| OpsListError::Unavailable {
            source: OpsStatusUnavailableSource::StatusStore {
                failure: status_read_failure(&error),
            },
        })?
        .into_iter()
        .filter(|status| !request.active_only || !status.is_terminal())
        .map(OperationStatusSnapshot::new)
        .collect();
    Ok(OpsListResult { operations })
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
