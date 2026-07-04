//! Read-only query runtimes behind the operation API: machine, service,
//! logs, and operation-status reads. Nothing here writes cluster truth.

use crate::controllers::OperationControllers;
use crate::machine_runtime::client::{MachineLogsTailRuntimeError, NatsMachineLogsTailer};
use crate::machine_runtime::protocol::MachineLogsTailRpcRequest;
use ployz_core::ids::{
    ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId,
};
use ployz_core::machine_runtime::{ManagedContainerKind, ManagedContainerObservation};
use ployz_core::ops::{
    OperationEventReplayPage, OperationEventReplayRequest, OperationStatusSnapshot,
};
use ployz_core::state::{ActiveMachineState, RouteBindingState, ServingTargetEntry};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_sdk_types::{
    LogsTailError, LogsTailRequest, LogsTailResult, MachineInspectError, MachineListError,
    MachineListResult, MachineSnapshot, OpsListError, OpsListRequest, OpsListResult,
    OpsStatusError, OpsWatchError, RuntimeDerivedCollectionSource, RuntimeDerivedCollectionStatus,
    RuntimeProjectionSource, RuntimeProjectionSources, RuntimeServiceInstance,
    RuntimeServiceRelease, RuntimeServiceRevision, RuntimeSnapshot, RuntimeSnapshotError,
    RuntimeSnapshotResult, ServiceInspectError, ServiceListError, ServiceListResult,
    ServiceSnapshot,
};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use super::error_map::ops_watch_error_from_replay_error;

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
            .map_err(|MachineListError::Unavailable { message }| {
                RuntimeSnapshotError::Unavailable { message }
            })?
            .machines;
        let services = service_query
            .list()
            .await
            .map_err(|ServiceListError::Unavailable { message }| {
                RuntimeSnapshotError::Unavailable { message }
            })?
            .services;
        let routes = self.core_state.route_bindings().await.map_err(|error| {
            RuntimeSnapshotError::Unavailable {
                message: error.to_string(),
            }
        })?;
        let containers = self
            .observations
            .machine_snapshot_records()
            .await
            .map_err(|error| RuntimeSnapshotError::Unavailable {
                message: error.to_string(),
            })?
            .into_iter()
            .flat_map(|record| record.snapshot.containers().to_vec())
            .collect::<Vec<_>>();
        let revisions = derive_runtime_revisions(&services, &containers);
        let releases = derive_runtime_releases(&services, &routes);
        let instances = derive_runtime_instances(&containers);
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
            .map_err(|error| LogsTailError::Unavailable {
                message: error.to_string(),
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
            .map_err(|error| LogsTailError::Unavailable {
                message: error.to_string(),
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
            .serving_target_entries()
            .await
            .map_err(|error| ServiceListError::Unavailable {
                message: error.to_string(),
            })?
            .into_iter()
            .map(service_snapshot)
            .collect();
        Ok(ServiceListResult { services })
    }

    pub(crate) async fn inspect(
        &self,
        namespace_id: &ployz_core::ids::NamespaceId,
        service_id: &ployz_core::ids::ServiceId,
    ) -> Result<ServiceSnapshot, ServiceInspectError> {
        let Some(active) = self
            .core_state
            .serving_target_entry(namespace_id, service_id)
            .await
            .map_err(|error| ServiceInspectError::Unavailable {
                message: error.to_string(),
            })?
        else {
            return Err(ServiceInspectError::NoSuchService {
                service_id: service_id.clone(),
            });
        };

        Ok(service_snapshot(active))
    }
}

fn service_snapshot(active: ServingTargetEntry) -> ServiceSnapshot {
    ServiceSnapshot { active }
}

fn derive_runtime_revisions(
    services: &[ServiceSnapshot],
    containers: &[ManagedContainerObservation],
) -> Vec<RuntimeServiceRevision> {
    let mut revisions = BTreeSet::new();
    for service in services {
        revisions.insert((
            service.active.namespace_id.clone(),
            service.active.service_id.clone(),
            service.active.namespace_revision_entry_id.clone(),
        ));
    }
    for container in containers {
        // Hook containers (predeploy, job) are operation evidence, not
        // service instances; only service containers evidence a revision.
        if container.identity.kind != ManagedContainerKind::Service {
            continue;
        }
        revisions.insert((
            container.identity.namespace_id.clone(),
            container.identity.service_id.clone(),
            container.identity.namespace_revision_entry_id.clone(),
        ));
    }

    revisions
        .into_iter()
        .map(
            |(namespace_id, service_id, namespace_revision_entry_id)| RuntimeServiceRevision {
                namespace_id,
                service_id,
                namespace_revision_entry_id,
            },
        )
        .collect()
}

fn derive_runtime_releases(
    services: &[ServiceSnapshot],
    routes: &[RouteBindingState],
) -> Vec<RuntimeServiceRelease> {
    let mut releases =
        BTreeMap::<(NamespaceId, ServiceId, NamespaceRevisionEntryId), Vec<_>>::new();
    let mut active_revisions = BTreeMap::new();
    for service in services {
        active_revisions.insert(
            (
                service.active.namespace_id.clone(),
                service.active.service_id.clone(),
            ),
            service.active.namespace_revision_entry_id.clone(),
        );
        releases
            .entry((
                service.active.namespace_id.clone(),
                service.active.service_id.clone(),
                service.active.namespace_revision_entry_id.clone(),
            ))
            .or_default();
    }
    for route in routes {
        // Route bindings are service references (ADR 0024); the served
        // entry identity comes from the serving target, not the binding.
        let Some(namespace_revision_entry_id) =
            active_revisions.get(&(route.namespace_id.clone(), route.service_id.clone()))
        else {
            continue;
        };
        releases
            .entry((
                route.namespace_id.clone(),
                route.service_id.clone(),
                namespace_revision_entry_id.clone(),
            ))
            .or_default()
            .push(route.target.clone());
    }

    releases
        .into_iter()
        .map(
            |((namespace_id, service_id, namespace_revision_entry_id), routes)| {
                RuntimeServiceRelease {
                    namespace_id,
                    service_id,
                    namespace_revision_entry_id,
                    routes,
                }
            },
        )
        .collect()
}

fn derive_runtime_instances(
    containers: &[ManagedContainerObservation],
) -> Vec<RuntimeServiceInstance> {
    containers
        .iter()
        .filter(|container| container.identity.kind == ManagedContainerKind::Service)
        .map(|container| RuntimeServiceInstance {
            namespace_id: container.identity.namespace_id.clone(),
            machine_id: container.machine_id.clone(),
            container_id: container.container_id.clone(),
            service_id: container.identity.service_id.clone(),
            namespace_revision_entry_id: container.identity.namespace_revision_entry_id.clone(),
            operation_id: container.identity.operation_id.clone(),
            step_id: container.identity.step_id.clone(),
            state: container.state.clone(),
        })
        .collect()
}

fn missing_runtime_links(
    services: &[ServiceSnapshot],
    routes: &[RouteBindingState],
    containers: &[ManagedContainerObservation],
) -> usize {
    let serving = services
        .iter()
        .map(|service| {
            (
                service.active.namespace_id.clone(),
                service.active.service_id.clone(),
            )
        })
        .collect::<BTreeSet<_>>();

    routes
        .iter()
        .filter(|route| !serving.contains(&(route.namespace_id.clone(), route.service_id.clone())))
        .count()
        + containers
            .iter()
            .filter(|container| {
                !serving.contains(&(
                    container.identity.namespace_id.clone(),
                    container.identity.service_id.clone(),
                ))
            })
            .count()
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

fn unix_seconds_from_nanos(unix_nanos: i128) -> u64 {
    u64::try_from(unix_nanos.max(0) / 1_000_000_000).unwrap_or(u64::MAX)
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
        let machines = self.core_state.active_machines().await.map_err(|error| {
            MachineListError::Unavailable {
                message: error.to_string(),
            }
        })?;
        let public_ips = self
            .observations
            .machine_public_ips()
            .await
            .map_err(|error| MachineListError::Unavailable {
                message: error.to_string(),
            })?
            .into_iter()
            .map(|observation| (observation.machine_id.clone(), observation))
            .collect::<BTreeMap<_, _>>();
        let gateway_statuses = self
            .observations
            .gateway_statuses()
            .await
            .map_err(|error| MachineListError::Unavailable {
                message: error.to_string(),
            })?
            .into_iter()
            .map(|observation| (observation.machine_id.clone(), observation))
            .collect::<BTreeMap<_, _>>();
        let container_observations = self
            .observations
            .machine_snapshot_records()
            .await
            .map_err(|error| MachineListError::Unavailable {
                message: error.to_string(),
            })?
            .into_iter()
            .map(|record| {
                (
                    record.snapshot.machine_id().clone(),
                    (
                        record.snapshot.containers().len(),
                        record.observed_at_unix_nanos,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut snapshots = Vec::with_capacity(machines.len());
        for machine in machines {
            let observation = container_observations.get(&machine.machine_id).copied();
            snapshots.push(MachineSnapshot {
                public_ip: public_ips.get(&machine.machine_id).cloned(),
                gateway: gateway_statuses.get(&machine.machine_id).cloned(),
                observed_container_count: observation.map(|(count, _)| count).unwrap_or_default(),
                last_observed_at_unix_seconds: observation
                    .map(|(_, observed_at)| unix_seconds_from_nanos(observed_at)),
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
            .map_err(|error| MachineInspectError::Unavailable {
                message: error.to_string(),
            })?
        else {
            return Err(MachineInspectError::NoSuchMachine {
                machine_id: machine_id.clone(),
            });
        };

        self.snapshot(machine)
            .await
            .map_err(|message| MachineInspectError::Unavailable { message })
    }

    async fn snapshot(&self, active: ActiveMachineState) -> Result<MachineSnapshot, String> {
        let public_ip = self
            .observations
            .machine_public_ip(&active.machine_id)
            .await
            .map_err(|error| error.to_string())?;
        let gateway = self
            .observations
            .gateway_status(&active.machine_id)
            .await
            .map_err(|error| error.to_string())?;
        let observation = self
            .observations
            .machine_snapshot_record(&active.machine_id)
            .await
            .map_err(|error| error.to_string())?;

        Ok(MachineSnapshot {
            active,
            public_ip,
            gateway,
            observed_container_count: observation
                .as_ref()
                .map(|record| record.snapshot.containers().len())
                .unwrap_or_default(),
            last_observed_at_unix_seconds: observation
                .map(|record| unix_seconds_from_nanos(record.observed_at_unix_nanos)),
        })
    }
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
        MachineLogsTailRuntimeError::Unavailable { machine_id, reason } => {
            LogsTailError::Unavailable {
                message: reason.failure_message().as_str().to_owned(),
                machine_id: Some(machine_id),
            }
        }
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
            message: error.to_string(),
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
            message: error.to_string(),
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

#[cfg(test)]
mod tests {
    use super::{
        ServiceSnapshot, derive_runtime_instances, derive_runtime_releases,
        derive_runtime_revisions,
    };
    use ployz_core::machine_runtime::ManagedContainerKind;
    use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};
    use ployz_core::state::{RouteBindingState, ServingTargetEntry};
    use ployz_test_support::containers;
    use ployz_test_support::ids::{namespace_id, namespace_revision_entry_id, service_id};

    /// Containers whose service has no serving target entry still surface
    /// as instances and revisions under their own identity namespace:
    /// Docker is execution reality, so orphaned containers are evidence,
    /// not noise (missing_link_count separately reports the mismatch).
    #[test]
    fn orphaned_containers_surface_as_instances_and_revisions() {
        let orphan = containers::observation("machine_a", "ctr_orphan")
            .with(containers::identity("svc_orphan").namespace("team-a"))
            .running_unroutable()
            .build();

        let instances = derive_runtime_instances(std::slice::from_ref(&orphan));
        let [instance] = instances.as_slice() else {
            panic!("orphaned container is projected as an instance");
        };
        assert_eq!(instance.namespace_id, namespace_id("team-a"));
        assert_eq!(instance.service_id, service_id("svc_orphan"));

        let revisions = derive_runtime_revisions(&[], &[orphan]);
        let [revision] = revisions.as_slice() else {
            panic!("orphaned container is projected as a revision");
        };
        assert_eq!(revision.namespace_id, namespace_id("team-a"));
    }

    /// Hook containers are operation evidence, not service instances:
    /// they never fabricate a service revision.
    #[test]
    fn hook_containers_do_not_evidence_revisions() {
        let job = containers::observation("machine_a", "ctr_job")
            .with(containers::identity("svc_api").kind(ManagedContainerKind::Job))
            .running_unroutable()
            .build();

        assert!(derive_runtime_revisions(&[], &[job]).is_empty());
    }

    /// A route for a namespace with no serving entry is a missing link
    /// even when another namespace serves the same service name.
    #[test]
    fn missing_links_are_namespace_scoped() {
        let serving = ServiceSnapshot {
            active: ServingTargetEntry {
                namespace_id: namespace_id("team-a"),
                service_id: service_id("web"),
                namespace_revision_entry_id: namespace_revision_entry_id("entry_a"),
            },
        };
        let dangling_route = RouteBindingState {
            namespace_id: namespace_id("team-b"),
            target: RouteTarget::new(
                RouteHostname::try_new("b.example.com").expect("valid route hostname"),
                RoutePort::try_new(443).expect("valid route port"),
            ),
            endpoint_port: RoutePort::try_new(8080).expect("valid route port"),
            service_id: service_id("web"),
        };

        assert_eq!(
            super::missing_runtime_links(&[serving], &[dangling_route], &[]),
            1
        );
    }

    /// Two namespaces sharing a service name must not cross-attribute
    /// route releases: the lookup is namespace-scoped, so team-b's route
    /// resolves team-b's entry even when team-a inserted last.
    #[test]
    fn releases_resolve_entries_per_namespace() {
        let serving = |namespace: &str, entry: &str| ServiceSnapshot {
            active: ServingTargetEntry {
                namespace_id: namespace_id(namespace),
                service_id: service_id("web"),
                namespace_revision_entry_id: namespace_revision_entry_id(entry),
            },
        };
        let route = RouteBindingState {
            namespace_id: namespace_id("team-b"),
            target: RouteTarget::new(
                RouteHostname::try_new("b.example.com").expect("valid route hostname"),
                RoutePort::try_new(443).expect("valid route port"),
            ),
            endpoint_port: RoutePort::try_new(8080).expect("valid route port"),
            service_id: service_id("web"),
        };

        let releases = derive_runtime_releases(
            &[serving("team-b", "entry_b"), serving("team-a", "entry_a")],
            &[route],
        );

        let release = releases
            .iter()
            .find(|release| !release.routes.is_empty())
            .expect("team-b's route lands on one release");
        assert_eq!(release.namespace_id, namespace_id("team-b"));
        assert_eq!(
            release.namespace_revision_entry_id,
            namespace_revision_entry_id("entry_b")
        );
    }
}
