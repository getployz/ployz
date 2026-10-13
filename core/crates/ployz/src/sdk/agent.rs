use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::future::join_all;
use ployz_core::QualifiedService;
use ployz_core::metrics::{MarkerKind, MetricMarker, MetricRange};
use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ContainerRuntimeObservation,
    EnvironmentValues, HealthObservation, LogHistoryRequest, MachineId, MachineObservation,
    MachineStorageObservation, MachineTarget, Namespace, RemoveContainerRequest, RpcError,
    RpcErrorCode, ServiceName,
    StartContainerRequest, StopContainerRequest, op,
};
use ployz_store::DeploymentId;
use ployz_store::tools::cloud::{
    ContainerReport, ContainerState, ContainersAnswer, Disk, Health, LogsAnswer, MetricEvent,
    MetricsAnswer, ServerLog, ServerReport, ServerState, ServersAnswer,
};
use ployz_store::tools::{
    CloudQuery, Freshness, LogsQuery, MetricsQuery, MetricsSince, MetricsSubject, ServerName,
    ServerRef, Source, Timestamp,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::Session;
use crate::cluster::{ContainerObservationCondition, HealthWaitError};
use crate::connect::TARGET_RPC_TIMEOUT;
use crate::operator::DEPLOYMENT_LABEL;

/// Answer `query` from the Servers behind `session`. A Server that did not answer is
/// named in `seen.unanswered`, never guessed.
///
/// # Errors
/// Returns `unsupported` for a query only Cloud answers, and
/// the session's error when it is closed or the Servers cannot be listed.
pub async fn ask(session: &Session, query: CloudQuery) -> Result<Value, RpcError> {
    match query {
        CloudQuery::Servers { only } => encode(&servers(session, only.as_ref()).await?),
        CloudQuery::Containers { namespace, service } => {
            encode(&containers(session, &namespace, service.as_ref()).await?)
        }
        CloudQuery::Logs(query) => encode(&logs(session, query).await?),
        CloudQuery::Metrics(query) => encode(&metrics(session, query).await?),
        CloudQuery::Copies { .. } => Err(unsupported(
            "Copies join the Config Store; Ployz Cloud answers them",
        )),
        CloudQuery::Operations(_)
        | CloudQuery::GithubTree(_)
        | CloudQuery::GithubFile(_)
        | CloudQuery::GithubConnection
        | CloudQuery::GithubBranches { .. } => Err(unsupported(
            "Operations and GitHub are Ployz Cloud's records; the Servers do not hold them",
        )),
    }
}

async fn servers(session: &Session, only: Option<&ServerName>) -> Result<ServersAnswer, RpcError> {
    let mut client = session.client()?;
    session
        .until_closed(async {
            let mut machines = client.machines().await.map_err(RpcError::from)?;
            if let Some(only) = only {
                machines.retain(|machine| &machine.machine.name == only);
            }
            let gaps = client.observe_machine_storage(&mut machines).await;
            let live = client
                .live_services_from(&machines, EnvironmentValues::Redacted)
                .await
                .map_err(RpcError::from)?;
            let mut unanswered = uninvited(&machines);
            unanswered.extend(gaps.failures.iter().map(|failure| failure.machine_id));
            unanswered.extend(gaps.omissions.iter().copied());
            unanswered.extend(
                live.containers
                    .failures
                    .iter()
                    .map(|failure| failure.machine_id),
            );
            unanswered.extend(live.containers.omissions.iter().copied());
            let mut namespaces = BTreeMap::<MachineId, BTreeSet<Namespace>>::new();
            for container in live.containers.successes.iter().flat_map(|s| &s.value) {
                namespaces
                    .entry(container.machine_id)
                    .or_default()
                    .insert(container.namespace.clone());
            }
            let servers = machines
                .iter()
                .map(|machine| server(machine, &unanswered, &mut namespaces))
                .collect();
            Ok(ServersAnswer {
                servers,
                seen: seen(&machines, &unanswered),
            })
        })
        .await
}

fn server(
    observed: &MachineObservation,
    unanswered: &BTreeSet<MachineId>,
    namespaces: &mut BTreeMap<MachineId, BTreeSet<Namespace>>,
) -> ServerReport {
    let machine = &observed.machine;
    let release = &machine.runtime.daemon_version;
    ServerReport {
        machine: machine.id,
        name: machine.name.clone(),
        state: if unanswered.contains(&machine.id) {
            ServerState::Offline
        } else {
            ServerState::Online
        },
        release: (!release.is_empty()).then(|| release.clone()),
        // The newest release on a channel is Cloud's to know, not the Server's.
        upgrade: None,
        draining: !machine.accepts_services,
        disk: match observed.storage {
            Some(MachineStorageObservation::Pool {
                size_bytes,
                used_bytes,
                ..
            }) => Some(Disk {
                used_bytes,
                total_bytes: size_bytes.get(),
            }),
            Some(MachineStorageObservation::Stateless | MachineStorageObservation::Ready)
            | None => None,
        },
        namespaces: namespaces
            .remove(&machine.id)
            .unwrap_or_default()
            .into_iter()
            .collect(),
    }
}

async fn containers(
    session: &Session,
    namespace: &Namespace,
    service: Option<&ServiceName>,
) -> Result<ContainersAnswer, RpcError> {
    let mut client = session.client()?;
    session
        .until_closed(async {
            let machines = client.machines().await.map_err(RpcError::from)?;
            let live = client
                .live_services_from(&machines, EnvironmentValues::Redacted)
                .await
                .map_err(RpcError::from)?;
            let mut unanswered = uninvited(&machines);
            unanswered.extend(
                live.containers
                    .failures
                    .iter()
                    .map(|failure| failure.machine_id),
            );
            unanswered.extend(live.containers.omissions.iter().copied());
            let names: BTreeMap<MachineId, &ServerName> = machines
                .iter()
                .map(|machine| (machine.machine.id, &machine.machine.name))
                .collect();
            let containers = live
                .containers
                .successes
                .iter()
                .flat_map(|success| &success.value)
                .filter(|container| {
                    container.kind == ContainerKind::ServiceContainer
                        && &container.namespace == namespace
                        && service.is_none_or(|service| container.service_name() == service)
                })
                .filter_map(|container| {
                    let server = names.get(&container.machine_id)?;
                    Some(report(container, server))
                })
                .collect();
            Ok(ContainersAnswer {
                containers,
                servers: machines
                    .iter()
                    .map(|machine| machine.machine.name.clone())
                    .collect(),
                seen: seen(&machines, &unanswered),
            })
        })
        .await
}

/// The newest lines of `query`'s Service on each Server, or on the one it names, as
/// [`super::service_tail`] scrubs them. A Server with no lines in range has no entry.
async fn logs(session: &Session, query: LogsQuery) -> Result<LogsAnswer, RpcError> {
    let mut client = session.client()?;
    session
        .until_closed(async {
            let mut machines = client.machines().await.map_err(RpcError::from)?;
            if let Some(only) = &query.server {
                machines.retain(|machine| machine.machine.id == only.machine);
            }
            let since_nanos = query.since.map(|since| {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default();
                i64::try_from(now.saturating_sub(since).as_nanos()).unwrap_or(i64::MAX)
            });
            let selector = LogHistoryRequest {
                namespace: Some(query.namespace.to_string()),
                service: Some(query.service.to_string()),
                deployment: query.deployment.map(|id| id.as_str().to_owned()),
                since_nanos,
                ..LogHistoryRequest::default()
            };
            let mut unanswered = uninvited(&machines);
            let invited: Vec<&MachineObservation> = machines
                .iter()
                .filter(|machine| !unanswered.contains(&machine.machine.id))
                .collect();
            let reads = join_all(invited.iter().map(|machine| {
                super::service_tail(
                    &client,
                    machine.machine.id,
                    selector.clone(),
                    query.lines,
                    &query.plain,
                )
            }))
            .await;
            let mut servers = Vec::new();
            let mut notes = Vec::new();
            for (machine, read) in invited.iter().zip(reads) {
                match read {
                    Ok(Some(text)) => servers.push(ServerLog {
                        server: machine.machine.name.clone(),
                        text,
                    }),
                    Ok(None) => {}
                    Err(error) => {
                        unanswered.insert(machine.machine.id);
                        notes.push(format!(
                            "{} did not answer: {}",
                            machine.machine.name, error.message
                        ));
                    }
                }
            }
            Ok(LogsAnswer {
                servers,
                seen: seen(&machines, &unanswered),
                notes,
            })
        })
        .await
}

/// How many OOM kills, exits and restarts a metrics answer lists one by one.
const METRIC_EVENTS: usize = 20;

const NANOS_PER_SECOND: i64 = 1_000_000_000;

async fn metrics(session: &Session, query: MetricsQuery) -> Result<MetricsAnswer, RpcError> {
    let mut client = session.client()?;
    session
        .until_closed(async {
            let machines = client.machines().await.map_err(RpcError::from)?;
            let until_nanos = now_nanos();
            let until = Timestamp(until_nanos / NANOS_PER_SECOND);
            let since = match query.since {
                MetricsSince::MinutesAgo { minutes } => {
                    Timestamp(until.0.saturating_sub(i64::from(minutes) * 60))
                }
                MetricsSince::At { at } => at.min(Timestamp(until.0.saturating_sub(1))),
            };
            let subject = match query.subject {
                MetricsSubject::Service { namespace, service } => super::MetricSubject::Service {
                    service: QualifiedService {
                        namespace,
                        name: service,
                    },
                },
                MetricsSubject::Server { server } => super::MetricSubject::Machine {
                    machine_id: server.machine,
                },
            };
            let range = MetricRange {
                since_nanos: since.0.saturating_mul(NANOS_PER_SECOND),
                until_nanos,
            };
            let history = super::metric_history(&client, subject, range, None).await?;
            Ok(summarize(
                &history,
                &machines,
                query.deployment.as_ref(),
                (since, until_nanos),
            ))
        })
        .await
}

fn summarize(
    history: &super::MetricHistory,
    machines: &[MachineObservation],
    deployment: Option<&DeploymentId>,
    (since, until_nanos): (Timestamp, i64),
) -> MetricsAnswer {
    let names: BTreeMap<MachineId, &ServerName> = machines
        .iter()
        .map(|machine| (machine.machine.id, &machine.machine.name))
        .collect();
    let mut markers: Vec<&MetricMarker> = history
        .machines
        .iter()
        .flat_map(|machine| &machine.value.markers)
        .filter(|marker| {
            deployment.is_none_or(|id| marker.deployment_id.as_deref() == Some(id.as_str()))
        })
        .collect();
    markers.sort_by_key(|marker| std::cmp::Reverse(marker.ts));
    let count = |kind: fn(&MarkerKind) -> bool| {
        u32::try_from(markers.iter().filter(|marker| kind(&marker.kind)).count())
            .unwrap_or(u32::MAX)
    };
    let mut notes = crate::cli::metrics::gap_notes(history);
    notes.extend(history.unanswered.iter().map(|failure| {
        let name = names
            .get(&failure.machine_id)
            .map_or_else(|| failure.machine_id.to_string(), ToString::to_string);
        format!("{name} did not answer: {}", failure.error.message)
    }));
    let unanswered = history
        .unanswered
        .iter()
        .map(|failure| failure.machine_id)
        .collect();
    MetricsAnswer {
        since,
        until: Timestamp(until_nanos / NANOS_PER_SECOND),
        metrics: crate::cli::metrics::rows(history, until_nanos),
        oom_kills: count(|kind| matches!(kind, MarkerKind::Oom)),
        exits: count(|kind| matches!(kind, MarkerKind::Exit { .. })),
        restarts: count(|kind| matches!(kind, MarkerKind::Restart)),
        events: markers
            .iter()
            .filter_map(|marker| {
                Some(MetricEvent {
                    at: Timestamp(marker.ts / NANOS_PER_SECOND),
                    server: (*names.get(&marker.machine_id)?).clone(),
                    namespace: marker.service.namespace.clone(),
                    service: marker.service.name.clone(),
                    deployment: marker
                        .deployment_id
                        .clone()
                        .and_then(|id| DeploymentId::parse(id).ok()),
                    kind: marker.kind,
                })
            })
            .take(METRIC_EVENTS)
            .collect(),
        notes,
        seen: seen(machines, &unanswered),
    }
}

fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_nanos()).unwrap_or(i64::MAX)
        })
}

/// One Container of a Service as its Server reports it: what an act stops and starts.
/// A hook Container is stopped with its Service but never started.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ServiceCopy {
    pub server: ServerName,
    pub machine: MachineId,
    pub container: ContainerId,
    pub state: ContainerState,
    pub hook: bool,
}

/// Every Container of one Service, and the Servers that did not say, by ID so a rename can't hide one.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ServiceCopies {
    pub copies: Vec<ServiceCopy>,
    pub unanswered: Vec<ServerRef>,
}

/// The Containers of `service` in `namespace` on every Server that answers.
///
/// # Errors
/// Returns the session's error when it is closed or the Servers cannot be listed.
pub async fn observe_service(
    session: &Session,
    namespace: &Namespace,
    service: &ServiceName,
) -> Result<ServiceCopies, RpcError> {
    let mut client = session.client()?;
    session
        .until_closed(async {
            let machines = client.machines().await.map_err(RpcError::from)?;
            let live = client
                .live_services_from(&machines, EnvironmentValues::Redacted)
                .await
                .map_err(RpcError::from)?;
            let mut unanswered = uninvited(&machines);
            unanswered.extend(
                live.containers
                    .failures
                    .iter()
                    .map(|failure| failure.machine_id),
            );
            unanswered.extend(live.containers.omissions.iter().copied());
            let names: BTreeMap<MachineId, &ServerName> = machines
                .iter()
                .map(|machine| (machine.machine.id, &machine.machine.name))
                .collect();
            let copies = copies_of(
                live.containers
                    .successes
                    .iter()
                    .flat_map(|success| &success.value),
                &names,
                namespace,
                service,
            );
            Ok(ServiceCopies {
                copies,
                unanswered: machines
                    .iter()
                    .filter(|machine| unanswered.contains(&machine.machine.id))
                    .map(|machine| ServerRef {
                        machine: machine.machine.id,
                        name: machine.machine.name.clone(),
                    })
                    .collect(),
            })
        })
        .await
}

fn copies_of<'a>(
    containers: impl Iterator<Item = &'a ContainerObservation>,
    names: &BTreeMap<MachineId, &ServerName>,
    namespace: &Namespace,
    service: &ServiceName,
) -> Vec<ServiceCopy> {
    containers
        .filter(|container| {
            &container.namespace == namespace && container.service_name() == service
        })
        .filter_map(|container| {
            let server = names.get(&container.machine_id)?;
            Some(ServiceCopy {
                server: (*server).clone(),
                machine: container.machine_id,
                container: container.container_id,
                state: report(container, server).state,
                hook: container.kind == ContainerKind::PreDeployHook,
            })
        })
        .collect()
}

/// Stop Container `container` on `machine`. An answer is the Machine's; a lost one is
/// the transport's error, and the stop may have happened.
///
/// # Errors
/// Returns the Machine's refusal, or the transport's error when it did not answer.
pub async fn stop_one(
    session: &Session,
    machine: MachineId,
    container: ContainerId,
) -> Result<(), RpcError> {
    let client = session.client()?;
    session
        .until_closed(crate::ingress::stop_container(
            &client,
            &machine,
            StopContainerRequest {
                container_id: container,
                signal: None,
                grace_period_seconds: None,
            },
            None,
        ))
        .await
}

/// Remove stopped Container `container` from `machine`, keeping its Volumes. A Container
/// the Machine no longer has answers Ok, so a resend after a crash converges.
///
/// # Errors
/// Returns the Machine's refusal, or the transport's error when it did not answer.
pub async fn remove_one(
    session: &Session,
    machine: MachineId,
    container: ContainerId,
) -> Result<(), RpcError> {
    let client = session.client()?;
    let removed = session
        .until_closed(client.invoke::<op::RemoveContainer>(
            RemoveContainerRequest {
                container_id: container,
                remove_volumes: false,
                force: false,
            },
            &MachineTarget::from(&machine),
            Some(TARGET_RPC_TIMEOUT),
        ))
        .await;
    match removed {
        Ok(_) => Ok(()),
        Err(error) if error.code == RpcErrorCode::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// What a committed start reached, waiting for it the way a Deploy does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Started {
    /// The Container serves traffic, or no Machine can report whether it does.
    Serving,
    /// The Container started but was not seen serving before the wait ended.
    Unconfirmed,
    /// The Container started but failed its healthcheck, or did not pass it in time.
    Unhealthy,
}

impl Started {
    /// The wire name the dashboard reads.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Serving => "serving",
            Self::Unconfirmed => "unconfirmed",
            Self::Unhealthy => "unhealthy",
        }
    }
}

/// Start Container `container` on `machine`, wait until it is healthy or past its
/// healthcheck deadline, then wait, bounded, until it serves.
///
/// # Errors
/// Returns the Machine's refusal, or the transport's error when it did not answer.
pub async fn start_one(
    session: &Session,
    machine: MachineId,
    container: ContainerId,
) -> Result<Started, RpcError> {
    let client = session.client()?;
    session
        .until_closed(async {
            client
                .invoke::<op::StartContainer>(
                    StartContainerRequest {
                        container_id: container,
                    },
                    &MachineTarget::from(&machine),
                    Some(TARGET_RPC_TIMEOUT),
                )
                .await?;
            let cancellation = CancellationToken::new();
            match client
                .wait_until_ready(&machine, &container, &cancellation)
                .await
            {
                Ok(()) => {}
                Err(HealthWaitError::Health(_)) => return Ok(Started::Unhealthy),
                Err(HealthWaitError::Inspect(_)) => return Ok(Started::Unconfirmed),
            }
            Ok(
                match client
                    .wait_for_container_observations(
                        &[container],
                        ContainerObservationCondition::Serving,
                        &cancellation,
                    )
                    .await
                {
                    Ok(()) => Started::Serving,
                    Err(_) => Started::Unconfirmed,
                },
            )
        })
        .await
}

fn report(container: &ContainerObservation, server: &ServerName) -> ContainerReport {
    let (state, exit_code, health) = match &container.runtime {
        ContainerRuntimeObservation::Created => (ContainerState::Created, None, None),
        ContainerRuntimeObservation::Running { health } => {
            (ContainerState::Running, None, self::health(health))
        }
        ContainerRuntimeObservation::Paused => (ContainerState::Paused, None, None),
        ContainerRuntimeObservation::Restarting => (ContainerState::Restarting, None, None),
        ContainerRuntimeObservation::Exited { code } => (ContainerState::Exited, Some(*code), None),
        ContainerRuntimeObservation::Removing => (ContainerState::Exited, None, None),
        ContainerRuntimeObservation::Dead | ContainerRuntimeObservation::Unknown { .. } => {
            (ContainerState::Dead, None, None)
        }
    };
    ContainerReport {
        server: server.clone(),
        service: container.service_name().clone(),
        state,
        exit_code,
        health,
        restarts: None,
        image: container.resolved_spec.container.image.clone(),
        deployment: container
            .labels
            .get(DEPLOYMENT_LABEL)
            .and_then(|id| DeploymentId::parse(id.clone()).ok()),
    }
}

fn health(observed: &HealthObservation) -> Option<Health> {
    match observed {
        HealthObservation::Starting => Some(Health::Starting),
        HealthObservation::Healthy => Some(Health::Healthy),
        HealthObservation::Unhealthy | HealthObservation::Failing => Some(Health::Unhealthy),
        HealthObservation::NotConfigured
        | HealthObservation::Stopping
        | HealthObservation::Unrecognized(_) => None,
    }
}

fn uninvited(machines: &[MachineObservation]) -> BTreeSet<MachineId> {
    machines
        .iter()
        .filter(|machine| !machine.invites_rpc())
        .map(|machine| machine.machine.id)
        .collect()
}

fn seen(machines: &[MachineObservation], unanswered: &BTreeSet<MachineId>) -> Freshness {
    Freshness {
        seen_by: None,
        observed_at: None,
        read_at: now(),
        age_seconds: None,
        from: Source::Server,
        unanswered: machines
            .iter()
            .filter(|machine| unanswered.contains(&machine.machine.id))
            .map(|machine| machine.machine.name.clone())
            .collect(),
    }
}

fn now() -> Timestamp {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    Timestamp(i64::try_from(seconds).unwrap_or(i64::MAX))
}

fn encode(answer: &impl serde::Serialize) -> Result<Value, RpcError> {
    serde_json::to_value(answer).map_err(|error| RpcError {
        code: RpcErrorCode::Internal,
        message: format!("encoding a Cloud answer: {error}"),
        details: Value::Null,
        cause: Vec::new(),
    })
}

fn unsupported(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unsupported,
        message: message.to_owned(),
        details: Value::Null,
        cause: Vec::new(),
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
