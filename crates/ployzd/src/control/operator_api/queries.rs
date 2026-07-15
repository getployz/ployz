//! Read-only query services behind the operation API: machine, service,
//! logs, and operation-status reads. Nothing here writes cluster truth.

mod volume;

pub use volume::VolumeQueryService;

use crate::control::intent::service::NatsIntentReader;
use crate::control::projection::ingress_endpoint::IngressEndpointProjectionHealth;
use crate::control::projection::runtime::RuntimeProjectionHealthReader;
use crate::control::projection::runtime_state::{
    from_sources as runtime_snapshot_from_sources, load_ingress_sources, service_snapshot,
};
use crate::control::reconciler::certificate::CertificateRenewalHealth;
use crate::control::role_client::machine::{
    MachineLogsTailError, NatsMachineFactsReader, NatsMachineLogsTailer,
    read_available_machine_facts, read_available_machine_facts_by_id,
};
use crate::control::sequencer::OperationControllers;
use crate::control::store::CoreStore;
use crate::role_testimony::RoleTestimonyCache;
use crate::roles::machine::protocol::MachineLogsTailRpcRequest;
use ployz_core::ids::{ContainerId, MachineId, NamespaceId, OperationId, ServiceId};
use ployz_core::intent::ActiveMachineState;
use ployz_core::machine::runtime::ManagedContainerKind;
use ployz_core::nats_config::NatsAuthorizationGrant;
use ployz_core::operation::{
    OperationEventReplayPage, OperationEventReplayRequest, OperationStatus, OperationStatusSnapshot,
};
use ployz_sdk_types::{
    ControlHealth, CredentialListError, CredentialListResult, LogsTailError, LogsTailRequest,
    LogsTailResult, LogsTailResultTarget, LogsTailTarget, MachineInspectError, MachineListError,
    MachineListResult, MachineSnapshot, MachineTestimony, OpsListError, OpsListRequest,
    OpsListResult, OpsStatusError, OpsWatchError, RuntimeSnapshotError, RuntimeSnapshotResult,
    ServiceInspectError, ServiceListError, ServiceListResult, ServiceSnapshot,
};

pub async fn credential_list(
    intent_reader: &NatsIntentReader,
) -> Result<CredentialListResult, CredentialListError> {
    let intent =
        intent_reader
            .intent()
            .await
            .map_err(|error| CredentialListError::Unavailable {
                message: error.to_string(),
            })?;
    let credentials = intent
        .nats_authorizations
        .into_iter()
        .filter_map(|grant| match grant {
            NatsAuthorizationGrant::Credential(credential) => Some(credential),
            NatsAuthorizationGrant::Internal { .. } => None,
        })
        .collect();
    Ok(CredentialListResult { credentials })
}
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use super::error_map::ops_watch_error_from_replay_error;

#[derive(Clone)]
pub struct MachineQueryService {
    intent_reader: NatsIntentReader,
    facts: RoleTestimonyCache,
    facts_reader: NatsMachineFactsReader,
}

#[derive(Clone)]
pub struct ServiceQueryService {
    intent_reader: NatsIntentReader,
    facts_reader: NatsMachineFactsReader,
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
    facts: RoleTestimonyCache,
    facts_reader: NatsMachineFactsReader,
    core_store: CoreStore,
    runtime_projection_health: RuntimeProjectionHealthReader,
    ingress_endpoint_projection_health: IngressEndpointProjectionHealth,
    certificate_renewal_health: CertificateRenewalHealth,
}

impl RuntimeSnapshotQueryService {
    #[must_use]
    pub(crate) fn new(
        intent_reader: NatsIntentReader,
        facts: RoleTestimonyCache,
        facts_reader: NatsMachineFactsReader,
        core_store: CoreStore,
        runtime_projection_health: RuntimeProjectionHealthReader,
        ingress_endpoint_projection_health: IngressEndpointProjectionHealth,
        certificate_renewal_health: CertificateRenewalHealth,
    ) -> Self {
        Self {
            intent_reader,
            facts,
            facts_reader,
            core_store,
            runtime_projection_health,
            ingress_endpoint_projection_health,
            certificate_renewal_health,
        }
    }

    pub(crate) async fn snapshot(&self) -> Result<RuntimeSnapshotResult, RuntimeSnapshotError> {
        let read_at_unix_seconds = current_unix_seconds();
        let intent = self.intent_reader.intent().await.map_err(|error| {
            RuntimeSnapshotError::Unavailable {
                message: error.to_string(),
            }
        })?;
        let machine_ids = intent
            .active_machines
            .iter()
            .map(|machine| machine.machine_id.clone())
            .collect::<Vec<_>>();
        let facts = read_available_machine_facts_by_id(&self.facts_reader, machine_ids).await;
        let gateway_statuses = self
            .facts
            .gateway_statuses()
            .into_iter()
            .map(|status| (status.machine_id.clone(), status))
            .collect();
        let ingress = load_ingress_sources(&self.core_store)
            .await
            .map_err(|message| RuntimeSnapshotError::Unavailable { message })?;

        Ok(RuntimeSnapshotResult {
            snapshot: runtime_snapshot_from_sources(
                intent,
                &facts,
                &gateway_statuses,
                ingress,
                read_at_unix_seconds,
            ),
            control_health: Some(ControlHealth {
                runtime_projection: self.runtime_projection_health.snapshot(),
                ingress_endpoint_projection: self
                    .ingress_endpoint_projection_health
                    .operational_health(),
                certificate_renewal: self.certificate_renewal_health.operational_health(),
            }),
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
        match request.target {
            LogsTailTarget::Service {
                namespace_id,
                service_id,
            } => {
                self.tail_service(
                    namespace_id,
                    service_id,
                    request.tail_lines,
                    request.since_unix_seconds,
                )
                .await
            }
            LogsTailTarget::Container {
                container_id,
                machine_id,
            } => {
                self.tail_container(
                    container_id,
                    machine_id,
                    request.tail_lines,
                    request.since_unix_seconds,
                )
                .await
            }
        }
    }

    async fn tail_container(
        &self,
        container_id: ContainerId,
        machine_id: Option<MachineId>,
        tail_lines: Option<ployz_sdk_types::LogsTailLines>,
        since_unix_seconds: Option<u64>,
    ) -> Result<LogsTailResult, LogsTailError> {
        let machine_id = match machine_id.clone() {
            Some(machine_id) => {
                self.verify_observed_container_on_machine(&machine_id, &container_id)
                    .await?;
                machine_id
            }
            None => self
                .find_container_machine(&container_id)
                .await?
                .ok_or_else(|| LogsTailError::NoSuchContainer {
                    container_id: container_id.clone(),
                })?,
        };

        self.tailer
            .tail_logs(
                &machine_id,
                MachineLogsTailRpcRequest {
                    container_id: container_id.clone(),
                    tail_lines: tail_lines.map(|lines| lines.get()),
                    since_unix_seconds,
                    timestamps: false,
                },
            )
            .await
            .map(|value| LogsTailResult {
                target: LogsTailResultTarget::Container {
                    machine_id: value.machine_id,
                    container_id: value.container_id,
                },
                text: value.text,
                truncated: value.truncated,
            })
            .map_err(logs_tail_machine_error)
    }

    async fn tail_service(
        &self,
        namespace_id: NamespaceId,
        service_id: ServiceId,
        tail_lines: Option<ployz_sdk_types::LogsTailLines>,
        since_unix_seconds: Option<u64>,
    ) -> Result<LogsTailResult, LogsTailError> {
        let intent =
            self.intent_reader
                .intent()
                .await
                .map_err(|error| LogsTailError::Unavailable {
                    message: error.to_string(),
                    machine_id: None,
                })?;
        let service_in_intent = intent
            .serving_target_entries
            .iter()
            .any(|entry| entry.namespace_id == namespace_id && entry.service_id == service_id);
        let machine_ids = intent
            .active_machines
            .into_iter()
            .map(|machine| machine.machine_id)
            .collect::<Vec<_>>();
        let facts_by_id =
            read_available_machine_facts_by_id(&self.facts_reader, machine_ids.clone()).await;
        let mut containers = Vec::new();
        for facts in facts_by_id.values() {
            containers.extend(
                facts
                    .containers()
                    .containers()
                    .iter()
                    .filter(|container| {
                        container.identity.kind == ManagedContainerKind::Service
                            && container.identity.namespace_id == namespace_id
                            && container.identity.service_id == service_id
                    })
                    .cloned(),
            );
        }
        let missing_machine_ids = missing_machine_ids(&machine_ids, &facts_by_id);

        if containers.is_empty() && !service_in_intent && missing_machine_ids.is_empty() {
            return Err(LogsTailError::NoSuchService {
                service_id: service_id.clone(),
            });
        }

        let mut lines = Vec::new();
        let mut truncated = false;
        for container in containers {
            match self
                .tailer
                .tail_logs(
                    &container.machine_id,
                    MachineLogsTailRpcRequest {
                        container_id: container.container_id.clone(),
                        tail_lines: tail_lines.map(|lines| lines.get()),
                        since_unix_seconds,
                        timestamps: true,
                    },
                )
                .await
            {
                Ok(value) => {
                    truncated |= value.truncated;
                    lines.extend(service_log_lines(
                        &value.machine_id,
                        &value.container_id,
                        &value.text,
                    ));
                }
                Err(MachineLogsTailError::NotFound { .. }) => {}
                Err(MachineLogsTailError::ReadFailed {
                    machine_id,
                    container_id,
                    message,
                }) => lines.push(ServiceLogLine {
                    sort_key: None,
                    text: format!(
                        "{} {} | log read failed: {}\n",
                        machine_id.as_str(),
                        container_id.as_str(),
                        message.as_str()
                    ),
                }),
                Err(MachineLogsTailError::Unavailable { machine_id, .. }) => {
                    lines.push(ServiceLogLine {
                        sort_key: None,
                        text: format!("machine {}: no answer\n", machine_id.as_str()),
                    });
                }
            }
        }

        for machine_id in missing_machine_ids {
            lines.push(ServiceLogLine {
                sort_key: None,
                text: format!("machine {}: no answer\n", machine_id.as_str()),
            });
        }
        lines.sort_by(|left, right| {
            left.sort_key
                .cmp(&right.sort_key)
                .then(left.text.cmp(&right.text))
        });

        Ok(LogsTailResult {
            target: LogsTailResultTarget::Service {
                namespace_id,
                service_id,
            },
            text: lines.into_iter().map(|line| line.text).collect(),
            truncated,
        })
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
    pub(crate) const fn new(
        intent_reader: NatsIntentReader,
        facts_reader: NatsMachineFactsReader,
    ) -> Self {
        Self {
            intent_reader,
            facts_reader,
        }
    }

    pub(crate) async fn list(&self) -> Result<ServiceListResult, ServiceListError> {
        let intent =
            self.intent_reader
                .intent()
                .await
                .map_err(|error| ServiceListError::Unavailable {
                    message: error.to_string(),
                })?;
        let machine_ids = intent
            .active_machines
            .iter()
            .map(|machine| machine.machine_id.clone())
            .collect::<Vec<_>>();
        let facts =
            read_available_machine_facts_by_id(&self.facts_reader, machine_ids.clone()).await;
        let routes = intent.route_bindings;
        let services = intent
            .serving_target_entries
            .into_iter()
            .map(|active| service_snapshot(active, &routes, &machine_ids, &facts))
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
        let machine_ids = intent
            .active_machines
            .iter()
            .map(|machine| machine.machine_id.clone())
            .collect::<Vec<_>>();
        let facts =
            read_available_machine_facts_by_id(&self.facts_reader, machine_ids.clone()).await;
        let routes = intent.route_bindings;
        let Some(active) = intent
            .serving_target_entries
            .into_iter()
            .find(|entry| entry.namespace_id == *namespace_id && entry.service_id == *service_id)
        else {
            return Err(ServiceInspectError::NoSuchService {
                service_id: service_id.clone(),
            });
        };

        Ok(service_snapshot(active, &routes, &machine_ids, &facts))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServiceLogLine {
    sort_key: Option<String>,
    text: String,
}

fn service_log_lines(
    machine_id: &MachineId,
    container_id: &ContainerId,
    text: &str,
) -> Vec<ServiceLogLine> {
    text.split_inclusive('\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (sort_key, message) = docker_timestamped_message(line);
            ServiceLogLine {
                sort_key,
                text: format!(
                    "{} {} | {}{}",
                    machine_id.as_str(),
                    container_id.as_str(),
                    message,
                    if message.ends_with('\n') { "" } else { "\n" },
                ),
            }
        })
        .collect()
}

fn docker_timestamped_message(line: &str) -> (Option<String>, &str) {
    let Some((timestamp, message)) = line.split_once(' ') else {
        return (None, line);
    };
    if timestamp.contains('T') {
        (Some(timestamp.to_owned()), message)
    } else {
        (None, line)
    }
}

fn missing_machine_ids(
    machine_ids: &[MachineId],
    facts_by_id: &BTreeMap<MachineId, impl Sized>,
) -> Vec<MachineId> {
    machine_ids
        .iter()
        .filter(|machine_id| !facts_by_id.contains_key(*machine_id))
        .cloned()
        .collect()
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
        facts: RoleTestimonyCache,
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
        let endpoints = facts
            .values()
            .filter_map(|facts| {
                facts
                    .endpoints()
                    .map(|endpoints| (facts.machine_id().clone(), endpoints.clone()))
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
                    (
                        container_count,
                        facts.disk_space(),
                        facts.observed_at_unix_ms() / 1_000,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut snapshots = Vec::with_capacity(machines.len());
        for machine in machines {
            let observation = container_observations.get(&machine.machine_id).copied();
            let testimony = match observation {
                Some((observed_container_count, disk_space, last_observed_at_unix_seconds)) => {
                    MachineTestimony::Answered {
                        endpoints: endpoints.get(&machine.machine_id).cloned(),
                        gateway: gateway_statuses
                            .get(&machine.machine_id)
                            .cloned()
                            .map(Box::new),
                        observed_container_count,
                        disk_space,
                        last_observed_at_unix_seconds,
                    }
                }
                None => MachineTestimony::NoAnswer,
            };
            snapshots.push(MachineSnapshot {
                testimony,
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
        let gateway = self.facts.gateway_status(&active.machine_id);
        let testimony = match facts {
            Some(facts) => MachineTestimony::Answered {
                endpoints: facts.endpoints().cloned(),
                gateway: gateway.map(Box::new),
                observed_container_count: facts.containers().containers().len(),
                disk_space: facts.disk_space(),
                last_observed_at_unix_seconds: facts.observed_at_unix_ms() / 1_000,
            },
            None => MachineTestimony::NoAnswer,
        };

        Ok(MachineSnapshot { active, testimony })
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
    let statuses = match request.before {
        Some(before) => controllers
            .operation_statuses_before(&before, request.active_only)
            .await
            .map_err(|error| OpsListError::Unavailable {
                message: error.to_string(),
            })?
            .ok_or(OpsListError::NoSuchOperation {
                operation_id: before,
            })?,
        None => controllers
            .operation_statuses_newest_first(request.active_only)
            .await
            .map_err(|error| OpsListError::Unavailable {
                message: error.to_string(),
            })?,
    };
    Ok(bounded_ops_list(statuses, request.active_only))
}

const OPS_LIST_LIMIT: usize = 100;

fn bounded_ops_list(statuses: Vec<OperationStatus>, active_only: bool) -> OpsListResult {
    let mut operations = statuses
        .into_iter()
        .filter(|status| !active_only || !status.is_terminal())
        .map(OperationStatusSnapshot::new)
        .take(OPS_LIST_LIMIT + 1)
        .collect::<Vec<_>>();
    let has_more = operations.len() > OPS_LIST_LIMIT;
    operations.truncate(OPS_LIST_LIMIT);
    OpsListResult {
        operations,
        has_more,
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

#[cfg(test)]
#[path = "queries_tests.rs"]
mod tests;
