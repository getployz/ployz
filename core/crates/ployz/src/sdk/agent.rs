use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::future::join_all;
use ployz_core::QualifiedService;
use ployz_core::metrics::{MarkerKind, MetricMarker, MetricRange};
use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ContainerRuntimeObservation,
    EnvironmentValues, HealthObservation, HealthVerdict, LogHistoryRequest, MachineId,
    MachineObservation, MachineStorageObservation, Namespace, RpcError, RpcErrorCode, ServiceName,
    StopOptions,
};
use ployz_store::DeploymentId;
use ployz_store::tools::cloud::{
    ContainerReport, ContainerState, ContainersAnswer, Disk, Health, LogsAnswer, MetricEvent,
    MetricsAnswer, ServerLog, ServerReport, ServerState, ServersAnswer,
};
use ployz_store::tools::{
    CloudQuery, Freshness, LogsQuery, MetricsQuery, MetricsSince, MetricsSubject, ServerLogsQuery,
    ServerName, ServerRef, Source, Timestamp,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::Session;
use crate::cluster::HealthWaitError;
use crate::deploy::cleanup::{self, UserStarted};
use crate::operator::DEPLOYMENT_LABEL;

/// The Namespaces whose Services an answer may describe. A Server answer keeps its host
/// totals either way; `Only` drops every Service event outside the set before counting.
#[derive(Clone, Debug, Default)]
pub enum Readable {
    #[default]
    All,
    Only(BTreeSet<Namespace>),
}

impl Readable {
    fn admits(&self, namespace: &Namespace) -> bool {
        match self {
            Self::All => true,
            Self::Only(namespaces) => namespaces.contains(namespace),
        }
    }
}

/// Answer `query` from the Servers behind `session`, describing only the Services
/// `readable` admits. A Server that did not answer is named in `seen.unanswered`,
/// never guessed. A logs answer is scrubbed of each Container's values under the
/// `sealed` names, which the caller takes from the Store, never from the asker; None
/// says the caller could not, and scrubs every env value.
///
/// # Errors
/// Returns `unsupported` for a query only Cloud answers, and
/// the session's error when it is closed or the Servers cannot be listed.
pub async fn ask(
    session: &Session,
    query: CloudQuery,
    readable: &Readable,
    sealed: Option<&BTreeSet<String>>,
) -> Result<Value, RpcError> {
    match query {
        CloudQuery::Servers { only } => encode(&servers(session, only.as_ref(), readable).await?),
        CloudQuery::Containers { namespace, service } => {
            encode(&containers(session, &namespace, service.as_ref()).await?)
        }
        CloudQuery::Logs(query) => encode(&logs(session, query, sealed).await?),
        CloudQuery::ServerLogs(query) => encode(&server_logs(session, query).await?),
        CloudQuery::Metrics(query) => encode(&metrics(session, query, readable).await?),
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

async fn servers(
    session: &Session,
    only: Option<&ServerName>,
    readable: &Readable,
) -> Result<ServersAnswer, RpcError> {
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
            for container in live
                .containers
                .successes
                .iter()
                .flat_map(|s| &s.value)
                .filter(|container| readable.admits(&container.namespace))
            {
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

async fn logs(
    session: &Session,
    query: LogsQuery,
    sealed: Option<&BTreeSet<String>>,
) -> Result<LogsAnswer, RpcError> {
    let mut client = session.client()?;
    session
        .until_closed(async {
            let machines = client.machines().await.map_err(RpcError::from)?;
            let selector = LogHistoryRequest {
                namespace: Some(query.namespace.to_string()),
                service: Some(query.service.to_string()),
                deployment: query.deployment.map(|id| id.as_str().to_owned()),
                since_nanos: since_nanos(query.since),
                ..LogHistoryRequest::default()
            };
            Ok(tail_each(machines, query.server.as_ref(), |machine| {
                super::service_tail(&client, machine, selector.clone(), query.lines, sealed)
            })
            .await)
        })
        .await
}

/// The newest lines one Server process wrote on each Server, or on the one `query` names.
async fn server_logs(session: &Session, query: ServerLogsQuery) -> Result<LogsAnswer, RpcError> {
    let mut client = session.client()?;
    session
        .until_closed(async {
            let machines = client.machines().await.map_err(RpcError::from)?;
            let since_nanos = since_nanos(query.since);
            Ok(tail_each(machines, query.server.as_ref(), |machine| {
                super::process_tail(&client, machine, query.process, since_nanos, query.lines)
            })
            .await)
        })
        .await
}

fn since_nanos(since: Option<Duration>) -> Option<i64> {
    since.map(|since| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        i64::try_from(now.saturating_sub(since).as_nanos()).unwrap_or(i64::MAX)
    })
}

/// Read every invited Server, or only `only`, at once. A Server whose read fails is
/// named unanswered with a note. One that withheld some Containers' lines still answers,
/// with a note naming them. One with nothing to show has no entry.
async fn tail_each<F, Fut, T>(
    mut machines: Vec<MachineObservation>,
    only: Option<&ServerRef>,
    read: F,
) -> LogsAnswer
where
    F: Fn(MachineId) -> Fut,
    Fut: Future<Output = Result<Option<T>, RpcError>>,
    T: Into<super::AgentTail>,
{
    if let Some(only) = only {
        machines.retain(|machine| machine.machine.id == only.machine);
    }
    let mut unanswered = uninvited(&machines);
    let invited: Vec<&MachineObservation> = machines
        .iter()
        .filter(|machine| !unanswered.contains(&machine.machine.id))
        .collect();
    let reads = join_all(invited.iter().map(|machine| read(machine.machine.id))).await;
    let mut servers = Vec::new();
    let mut notes = Vec::new();
    for (machine, read) in invited.iter().zip(reads) {
        match read {
            Ok(Some(tail)) => {
                let super::AgentTail { text, withheld } = tail.into();
                if !withheld.is_empty() {
                    notes.push(format!(
                        "Lines from {} on {} withheld: their env could not be read to scrub Sealed values.",
                        withheld.join(", "),
                        machine.machine.name,
                    ));
                }
                servers.push(ServerLog {
                    server: machine.machine.name.clone(),
                    text,
                });
            }
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
    LogsAnswer {
        servers,
        seen: seen(&machines, &unanswered),
        notes,
    }
}

/// How many OOM kills, exits and restarts a metrics answer lists one by one.
const METRIC_EVENTS: usize = 20;

const NANOS_PER_SECOND: i64 = 1_000_000_000;

async fn metrics(
    session: &Session,
    query: MetricsQuery,
    readable: &Readable,
) -> Result<MetricsAnswer, RpcError> {
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
                readable,
                (since, until_nanos),
            ))
        })
        .await
}

fn summarize(
    history: &super::MetricHistory,
    machines: &[MachineObservation],
    deployment: Option<&DeploymentId>,
    readable: &Readable,
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
            readable.admits(&marker.service.namespace)
                && deployment.is_none_or(|id| marker.deployment_id.as_deref() == Some(id.as_str()))
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

/// Stop Container `container` on `machine` once no proxy routes to it. A copy that
/// serves leaves rotation first and stays startable. An answer is the Machine's; a lost
/// one is the transport's error, and the stop may have happened.
///
/// # Errors
/// Returns the Machine's refusal, a copy proxies kept routing (left running), or the
/// transport's error when it did not answer.
pub async fn stop_one(
    session: &Session,
    machine: MachineId,
    container: ContainerId,
) -> Result<(), RpcError> {
    let client = session.client()?;
    session
        .until_closed(cleanup::user_stop(
            &client,
            &machine,
            &container,
            StopOptions {
                signal: None,
                grace_period_seconds: None,
            },
            1,
            &CancellationToken::new(),
        ))
        .await
}

/// Remove Container `container` from `machine` with its anonymous Volumes, keeping its
/// named Volumes. A copy that serves leaves rotation and is removed once no proxy routes
/// to it. A Container the Machine no longer has answers Ok, so a resend after a crash
/// converges.
///
/// # Errors
/// Returns the Machine's refusal, a copy proxies kept routing (left running), or the
/// transport's error when it did not answer.
pub async fn remove_one(
    session: &Session,
    machine: MachineId,
    container: ContainerId,
) -> Result<(), RpcError> {
    let client = session.client()?;
    session
        .until_closed(cleanup::user_remove(
            &client,
            &machine,
            &container,
            &CancellationToken::new(),
        ))
        .await
}

/// What a committed start reached, waiting for it the way a Deploy does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Started {
    /// The Container started and passed its healthcheck, or has none.
    Serving,
    /// The Container started but its health could not be read.
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

/// Start Container `container` on `machine`, then wait until it is healthy or past its
/// healthcheck deadline. A copy that serves is brought back into rotation once healthy.
///
/// # Errors
/// Returns the Machine's refusal, a Conflict for a copy out of rotation (deploy again),
/// or the transport's error when it did not answer.
pub async fn start_one(
    session: &Session,
    machine: MachineId,
    container: ContainerId,
) -> Result<Started, RpcError> {
    let client = session.client()?;
    session
        .until_closed(async {
            Ok(
                match cleanup::user_start(&client, &machine, &container).await? {
                    UserStarted::In => Started::Serving,
                    UserStarted::NotIn(Some(verdict)) if verdict != HealthVerdict::Healthy => {
                        Started::Unhealthy
                    }
                    UserStarted::NotIn(_) => Started::Unconfirmed,
                    UserStarted::Running => {
                        let cancellation = CancellationToken::new();
                        match client
                            .wait_until_ready(&machine, &container, &cancellation)
                            .await
                        {
                            Ok(()) => Started::Serving,
                            Err(HealthWaitError::Health(_)) => Started::Unhealthy,
                            Err(HealthWaitError::Inspect(_)) => Started::Unconfirmed,
                        }
                    }
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
        stopped: container.stopped,
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
