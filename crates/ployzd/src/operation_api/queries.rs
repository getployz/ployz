//! Read-only query services behind the operation API: machine, service,
//! logs, and operation-status reads. Nothing here writes cluster truth.

use crate::fact_cache::FactCache;
use crate::intent::service::NatsIntentReader;
use crate::operation_api::admission::OperationControllers;
use crate::roles::machine::client::{
    MachineLogsTailError, NatsMachineFactsReader, NatsMachineLogsTailer,
    read_available_machine_facts, read_available_machine_facts_by_id,
};
use crate::roles::machine::protocol::MachineLogsTailRpcRequest;
use ployz_core::ids::{
    ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId,
};
use ployz_core::machine_runtime::{ManagedContainerKind, ManagedContainerObservation};
use ployz_core::ops::{
    OperationEventReplayPage, OperationEventReplayRequest, OperationStatusSnapshot,
};
use ployz_core::state::{ActiveMachineState, RouteBindingState, ServingTargetEntry};
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
pub struct MachineQueryService {
    intent_reader: NatsIntentReader,
    facts: FactCache,
    facts_reader: NatsMachineFactsReader,
}

#[derive(Clone)]
pub struct ServiceQueryService {
    intent_reader: NatsIntentReader,
}

#[derive(Clone)]
pub struct LogsQueryService {
    intent_reader: NatsIntentReader,
    facts_reader: NatsMachineFactsReader,
    tailer: NatsMachineLogsTailer,
}

#[derive(Clone)]
pub struct RuntimeSnapshotQueryService {
    intent_reader: NatsIntentReader,
    facts: FactCache,
    facts_reader: NatsMachineFactsReader,
}

impl RuntimeSnapshotQueryService {
    #[must_use]
    pub(crate) const fn new(
        intent_reader: NatsIntentReader,
        facts: FactCache,
        facts_reader: NatsMachineFactsReader,
    ) -> Self {
        Self {
            intent_reader,
            facts,
            facts_reader,
        }
    }

    pub(crate) async fn snapshot(&self) -> Result<RuntimeSnapshotResult, RuntimeSnapshotError> {
        let read_at_unix_seconds = current_unix_seconds();
        let intent = self.intent_reader.intent().await.map_err(|error| {
            RuntimeSnapshotError::Unavailable {
                message: error.to_string(),
            }
        })?;
        let machine_query = MachineQueryService::new(
            self.intent_reader.clone(),
            self.facts.clone(),
            self.facts_reader.clone(),
        );
        let service_query = ServiceQueryService::new(self.intent_reader.clone());
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
        let routes = intent.route_bindings;
        let machine_ids = machines
            .iter()
            .map(|machine| machine.active.machine_id.clone())
            .collect::<Vec<_>>();
        let facts = read_available_machine_facts(&self.facts_reader, machine_ids).await;
        let containers = facts
            .into_iter()
            .flat_map(|facts| facts.containers().containers().to_vec())
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
                    intent: RuntimeProjectionSource {
                        read_at_unix_seconds,
                    },
                    facts: RuntimeProjectionSource {
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

impl LogsQueryService {
    #[must_use]
    pub(crate) fn new(
        intent_reader: NatsIntentReader,
        facts_reader: NatsMachineFactsReader,
        tailer: NatsMachineLogsTailer,
    ) -> Self {
        Self {
            intent_reader,
            facts_reader,
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
        let intent =
            self.intent_reader
                .intent()
                .await
                .map_err(|error| LogsTailError::Unavailable {
                    message: error.to_string(),
                    machine_id: None,
                })?;
        let machine_ids = intent
            .active_machines
            .into_iter()
            .map(|machine| machine.machine_id)
            .collect::<Vec<_>>();
        let mut matches = read_available_machine_facts(&self.facts_reader, machine_ids)
            .await
            .into_iter()
            .filter_map(|facts| {
                facts
                    .containers()
                    .container(container_id)
                    .map(|_| facts.machine_id().clone())
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
        let facts = self
            .facts_reader
            .machine_facts(machine_id)
            .await
            .map_err(|error| LogsTailError::Unavailable {
                message: error.to_string(),
                machine_id: Some(machine_id.clone()),
            })?;
        if facts.containers().container(container_id).is_none() {
            return Err(LogsTailError::NoSuchContainer {
                container_id: container_id.clone(),
            });
        }
        Ok(())
    }
}

impl ServiceQueryService {
    #[must_use]
    pub(crate) const fn new(intent_reader: NatsIntentReader) -> Self {
        Self { intent_reader }
    }

    pub(crate) async fn list(&self) -> Result<ServiceListResult, ServiceListError> {
        let intent =
            self.intent_reader
                .intent()
                .await
                .map_err(|error| ServiceListError::Unavailable {
                    message: error.to_string(),
                })?;
        let services = intent
            .serving_target_entries
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
        let intent = self.intent_reader.intent().await.map_err(|error| {
            ServiceInspectError::Unavailable {
                message: error.to_string(),
            }
        })?;
        let Some(active) = intent
            .serving_target_entries
            .into_iter()
            .find(|entry| entry.namespace_id == *namespace_id && entry.service_id == *service_id)
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

impl MachineQueryService {
    #[must_use]
    pub(crate) fn new(
        intent_reader: NatsIntentReader,
        facts: FactCache,
        facts_reader: NatsMachineFactsReader,
    ) -> Self {
        Self {
            intent_reader,
            facts,
            facts_reader,
        }
    }

    pub(crate) async fn list(&self) -> Result<MachineListResult, MachineListError> {
        let intent =
            self.intent_reader
                .intent()
                .await
                .map_err(|error| MachineListError::Unavailable {
                    message: error.to_string(),
                })?;
        let machines = intent.active_machines;
        let machine_ids = machines
            .iter()
            .map(|machine| machine.machine_id.clone())
            .collect::<Vec<_>>();
        let facts = read_available_machine_facts_by_id(&self.facts_reader, machine_ids).await;
        let public_ips = facts
            .values()
            .filter_map(|facts| {
                facts
                    .public_ip()
                    .map(|public_ip| (facts.machine_id().clone(), public_ip.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        let gateway_statuses = self
            .facts
            .gateway_statuses()
            .into_iter()
            .map(|observation| (observation.machine_id.clone(), observation))
            .collect::<BTreeMap<_, _>>();
        let container_observations = facts
            .values()
            .map(|facts| {
                let container_count = facts.containers().containers().len();
                (
                    facts.machine_id().clone(),
                    (container_count, facts.observed_at_unix_ms() / 1_000),
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
                last_observed_at_unix_seconds: observation.map(|(_, observed_at)| observed_at),
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
        let intent = self.intent_reader.intent().await.map_err(|error| {
            MachineInspectError::Unavailable {
                message: error.to_string(),
            }
        })?;
        let Some(machine) = intent
            .active_machines
            .into_iter()
            .find(|machine| machine.machine_id == *machine_id)
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
        let facts = self
            .facts_reader
            .machine_facts(&active.machine_id)
            .await
            .ok();
        let public_ip = facts.as_ref().and_then(|facts| facts.public_ip().cloned());
        let gateway = self.facts.gateway_status(&active.machine_id);

        Ok(MachineSnapshot {
            active,
            public_ip,
            gateway,
            observed_container_count: facts
                .as_ref()
                .map(|facts| facts.containers().containers().len())
                .unwrap_or_default(),
            last_observed_at_unix_seconds: facts.map(|facts| facts.observed_at_unix_ms() / 1_000),
        })
    }
}

fn logs_tail_machine_error(error: MachineLogsTailError) -> LogsTailError {
    match error {
        MachineLogsTailError::NotFound { container_id, .. } => {
            LogsTailError::NoSuchContainer { container_id }
        }
        MachineLogsTailError::ReadFailed {
            machine_id,
            container_id,
            message,
        } => LogsTailError::ReadFailed {
            machine_id,
            container_id,
            message,
        },
        MachineLogsTailError::Unavailable { machine_id, reason } => LogsTailError::Unavailable {
            message: reason.failure_message().as_str().to_owned(),
            machine_id: Some(machine_id),
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
    match controllers
        .operation_status_snapshot(&operation_id)
        .await
        .map_err(|error| OpsStatusError::Unavailable {
            operation_id: operation_id.clone(),
            message: error.to_string(),
        })? {
        Some(snapshot) => Ok(snapshot),
        None => Err(ops_status_missing(&operation_id)),
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
