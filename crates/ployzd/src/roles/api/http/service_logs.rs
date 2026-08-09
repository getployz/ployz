use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::stream;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use hyper::{Response, StatusCode};
use ployz_core::corrosion::{
    ContainerDocument, ServiceDocument, SqliteParameter, Statement, V2ManagedContainerIdentity,
    read_rows,
};
use ployz_core::deploy::ReplicaSlot;
use ployz_core::ids::{ClusterId, ContainerId, MachineRowId, ServiceRowId};
use ployz_core::machine::MachineName;
use ployz_core::{
    ServiceLogLine, ServiceLogStream, ServiceLogsFollowEvent, ServiceLogsRefusal,
    ServiceLogsRequest, ServiceLogsTailReply,
};

use crate::corrosion::{CorrosionClient, StoredRowLimit, collect_stored_rows};
use crate::roles::api::runner::{
    ExistingV2ManagedContainer, RuntimeLogBatch, RuntimeLogFollow, RuntimeLogStream,
    V2MachineContainerRunner, V2MachineLogReadError, V2MachineLogReader,
};

use super::server::{
    ApiService, BoundedBodyError, HttpBody, corrosion_unavailable_response, json_response,
    read_bounded_body, sse_data, sse_keepalive, sse_response,
};

const MAX_RESOLVED_ROWS: usize = 2;
// ponytail: bounded replica ceiling; raise when a service legitimately runs more containers.
const MAX_SERVICE_CONTAINER_ROWS: usize = 64;
const MAX_LOG_REQUEST_BYTES: usize = 1_024;
const LOG_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const LOG_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const LOG_REATTACH_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const LOG_REATTACH_MAX_BACKOFF: Duration = Duration::from_secs(2);

pub(super) async fn handle_tail(
    service: &ApiService,
    service_id: ServiceRowId,
    request: hyper::Request<hyper::body::Incoming>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Response<HttpBody> {
    let request = match decode_log_request(request.into_body(), LogRequestBody::Required).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    let target = match resolve_log_target(service, &service_id, request.machine.as_ref()).await {
        Ok(target) => target,
        Err(response) => return response,
    };
    let Some(runner) = service.container_runner.as_ref() else {
        return log_refusal_response(ServiceLogsRefusal::RuntimeUnavailable {
            machine_id: target.machine_id,
        });
    };
    if let Err(error) = verify_local_log_target(runner.as_ref(), &target).await {
        return log_refusal_response(error.public_refusal(&target));
    }
    match tail_local_service_logs(runner.as_ref(), &target, request.tail_lines, shutdown).await {
        Ok(reply) => super::mutations::typed_response(StatusCode::OK, &reply),
        Err(error) => log_refusal_response(error.public_refusal(&target)),
    }
}

pub(super) async fn handle_follow(
    service: &ApiService,
    service_id: ServiceRowId,
    request: hyper::Request<hyper::body::Incoming>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Response<HttpBody> {
    let request = match decode_log_request(request.into_body(), LogRequestBody::Optional).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    let target = match resolve_log_target(service, &service_id, request.machine.as_ref()).await {
        Ok(target) => target,
        Err(response) => return response,
    };
    let Some(runner) = service.container_runner.as_ref() else {
        return log_refusal_response(ServiceLogsRefusal::RuntimeUnavailable {
            machine_id: target.machine_id,
        });
    };
    if let Err(error) = verify_local_log_target(runner.as_ref(), &target).await {
        return log_refusal_response(error.public_refusal(&target));
    }
    let mut follow = LossyServiceLogFollow::new();
    let session = match follow
        .open(
            runner.as_ref(),
            &target,
            request.tail_lines,
            shutdown.clone(),
        )
        .await
    {
        Ok(session) => session,
        Err(error) => return log_refusal_response(error.public_refusal(&target)),
    };
    sse_response(service_log_follow_body(
        Arc::clone(runner),
        target,
        follow,
        session,
        shutdown,
    ))
}

#[derive(Clone, Copy)]
enum LogRequestBody {
    Required,
    Optional,
}

struct DecodedLogRequest {
    tail_lines: u16,
    machine: Option<MachineName>,
}

async fn decode_log_request(
    body: hyper::body::Incoming,
    mode: LogRequestBody,
) -> Result<DecodedLogRequest, Response<HttpBody>> {
    let body = read_bounded_body(body, MAX_LOG_REQUEST_BYTES, LOG_REQUEST_TIMEOUT)
        .await
        .map_err(log_body_error)?;
    if body.is_empty() && matches!(mode, LogRequestBody::Optional) {
        return Ok(DecodedLogRequest {
            tail_lines: 0,
            machine: None,
        });
    }
    let request: ServiceLogsRequest = serde_json::from_slice(&body)
        .map_err(|_| log_request_error(StatusCode::BAD_REQUEST, "invalid_request"))?;
    let tail_lines = match (request.tail_lines, mode) {
        (Some(tail_lines), LogRequestBody::Required | LogRequestBody::Optional) => tail_lines.get(),
        // A follow reconnect attaches without replay but keeps its selector.
        (None, LogRequestBody::Optional) => 0,
        (None, LogRequestBody::Required) => {
            return Err(log_request_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ));
        }
    };
    Ok(DecodedLogRequest {
        tail_lines,
        machine: request.machine,
    })
}

fn log_body_error(error: BoundedBodyError) -> Response<HttpBody> {
    match error {
        BoundedBodyError::TooLarge => {
            log_request_error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large")
        }
        BoundedBodyError::Deadline => {
            log_request_error(StatusCode::REQUEST_TIMEOUT, "request_timeout")
        }
        BoundedBodyError::Read => log_request_error(StatusCode::BAD_REQUEST, "invalid_request"),
    }
}

fn log_request_error(status: StatusCode, kind: &'static str) -> Response<HttpBody> {
    json_response(status, format!("{{\"kind\":\"{kind}\"}}").into_bytes())
}

async fn resolve_log_target(
    service: &ApiService,
    service_id: &ServiceRowId,
    machine: Option<&MachineName>,
) -> Result<LocalServiceLogTarget, Response<HttpBody>> {
    let resolver = CorrosionServiceLogResolver::new(
        service.corrosion.clone(),
        service.cluster_id.clone(),
        service.local_machine_id.clone(),
    );
    match resolver.resolve(service_id, machine).await {
        Ok(Ok(target)) => Ok(target),
        Ok(Err(refusal)) => Err(log_refusal_response(refusal)),
        Err(error) => {
            tracing::warn!(%error, "service log target resolution failed");
            Err(corrosion_unavailable_response())
        }
    }
}

fn log_refusal_response(refusal: ServiceLogsRefusal) -> Response<HttpBody> {
    let status = match &refusal {
        ServiceLogsRefusal::ServiceNotFound { .. }
        | ServiceLogsRefusal::NoActiveDeploy { .. }
        | ServiceLogsRefusal::ContainerNotFound { .. } => StatusCode::NOT_FOUND,
        ServiceLogsRefusal::UnmanagedContainer { .. }
        | ServiceLogsRefusal::MachineSelectorRequired { .. }
        | ServiceLogsRefusal::HostingMachinesUnresolved { .. }
        | ServiceLogsRefusal::RemoteOwner { .. } => StatusCode::CONFLICT,
        ServiceLogsRefusal::RuntimeUnavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
    };
    super::mutations::typed_response(status, &refusal)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum ServiceLogAccessError {
    #[error("service log request was refused: {0:?}")]
    Refusal(ServiceLogsRefusal),
    #[error("service log runtime is unavailable: {message}")]
    RuntimeUnavailable { message: String },
}

impl ServiceLogAccessError {
    pub(super) fn public_refusal(self, target: &LocalServiceLogTarget) -> ServiceLogsRefusal {
        match self {
            Self::Refusal(refusal) => refusal,
            Self::RuntimeUnavailable { .. } => ServiceLogsRefusal::RuntimeUnavailable {
                machine_id: target.machine_id.clone(),
            },
        }
    }
}

/// A service-log target proven from accepted service and container rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LocalServiceLogTarget {
    pub(super) service_id: ServiceRowId,
    pub(super) machine_id: MachineRowId,
    pub(super) container_id: ContainerId,
    pub(super) identity: V2ManagedContainerIdentity,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ServiceLogResolveError {
    #[error(transparent)]
    Client(#[from] crate::corrosion::CorrosionClientError),
    #[error("service-log Corrosion response was invalid: {0}")]
    Protocol(String),
}

/// Deep row resolution for service logs. Remote ownership remains typed so the HTTP layer can
/// proxy without ever asking the local Docker daemon about a remote container.
#[derive(Clone)]
pub(super) struct CorrosionServiceLogResolver {
    client: CorrosionClient,
    cluster_id: ClusterId,
    local_machine_id: MachineRowId,
}

impl CorrosionServiceLogResolver {
    #[must_use]
    pub(super) const fn new(
        client: CorrosionClient,
        cluster_id: ClusterId,
        local_machine_id: MachineRowId,
    ) -> Self {
        Self {
            client,
            cluster_id,
            local_machine_id,
        }
    }

    pub(super) async fn resolve(
        &self,
        service_id: &ServiceRowId,
        machine: Option<&MachineName>,
    ) -> Result<Result<LocalServiceLogTarget, ServiceLogsRefusal>, ServiceLogResolveError> {
        let service_rows = self
            .query(
                Statement::with_params(
                    "SELECT id, document FROM services WHERE id = ?",
                    vec![SqliteParameter::Text(service_id.as_str().to_owned())],
                ),
                MAX_RESOLVED_ROWS,
            )
            .await?;
        let service_report = read_rows::<ServiceDocument>(&self.cluster_id, service_rows);
        if !service_report.skipped.is_empty() || service_report.accepted.len() > 1 {
            return Err(ServiceLogResolveError::Protocol(
                "service lookup contained rejected or ambiguous rows".to_owned(),
            ));
        }
        let Some(service) = service_report.accepted.into_iter().next() else {
            return Ok(Err(ServiceLogsRefusal::ServiceNotFound {
                service_id: service_id.clone(),
            }));
        };

        let container_rows = self
            .query(
                Statement::with_params(
                    "SELECT id, document FROM containers WHERE service_id = ? AND deploy = ?",
                    vec![
                        SqliteParameter::Text(service_id.as_str().to_owned()),
                        SqliteParameter::Text(service.value.active_deploy.as_str().to_owned()),
                    ],
                ),
                MAX_SERVICE_CONTAINER_ROWS,
            )
            .await?;
        let container_report = read_rows::<ContainerDocument>(&self.cluster_id, container_rows);
        if !container_report.skipped.is_empty() {
            return Err(ServiceLogResolveError::Protocol(
                "active deploy container lookup contained rejected rows".to_owned(),
            ));
        }
        let mut containers = Vec::new();
        for container in container_report.accepted {
            let container_id = ContainerId::try_new(container.source.key).map_err(|error| {
                ServiceLogResolveError::Protocol(format!("container row id was invalid: {error}"))
            })?;
            containers.push(ReplicaContainer {
                container_id,
                machine_id: container.value.machine_id,
                replica_slot: container.value.replica_slot,
            });
        }
        let machine_names = self.machine_names(&containers).await?;
        let selected = match select_log_container(
            service_id,
            &containers,
            &machine_names,
            machine,
            &self.local_machine_id,
        ) {
            Ok(selected) => selected,
            Err(refusal) => return Ok(Err(refusal)),
        };
        Ok(Ok(LocalServiceLogTarget {
            service_id: service_id.clone(),
            machine_id: self.local_machine_id.clone(),
            container_id: selected.container_id.clone(),
            identity: V2ManagedContainerIdentity {
                namespace_id: service.value.namespace_id,
                service_id: service_id.clone(),
                operation_id: service.value.active_deploy,
                replica_slot: selected.replica_slot,
            },
        }))
    }

    /// Names for every distinct machine hosting one of this deploy's
    /// containers, from one accepted-roster read; a machine whose roster row
    /// is gone or unreadable stays unnamed.
    async fn machine_names(
        &self,
        containers: &[ReplicaContainer],
    ) -> Result<std::collections::BTreeMap<MachineRowId, MachineName>, ServiceLogResolveError> {
        if containers.is_empty() {
            return Ok(std::collections::BTreeMap::new());
        }
        let machine_ids: std::collections::BTreeSet<MachineRowId> = containers
            .iter()
            .map(|container| container.machine_id.clone())
            .collect();
        let roster = super::store::read_accepted_roster(&self.client, &self.cluster_id)
            .await
            .map_err(|error| {
                ServiceLogResolveError::Protocol(format!("accepted roster unavailable: {error}"))
            })?;
        Ok(roster
            .machines
            .into_iter()
            .filter(|machine| machine_ids.contains(&machine.id))
            .map(|machine| (machine.id, machine.document.name))
            .collect())
    }

    async fn query(
        &self,
        statement: Statement,
        limit: usize,
    ) -> Result<Vec<ployz_core::corrosion::StoredRow>, ServiceLogResolveError> {
        let mut stream = self.client.query(&statement).await?;
        collect_stored_rows(&mut stream, StoredRowLimit::new(limit))
            .await
            .map_err(|error| ServiceLogResolveError::Protocol(error.to_string()))
    }
}

/// One accepted container row of the active deploy, keyed by where it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReplicaContainer {
    container_id: ContainerId,
    machine_id: MachineRowId,
    replica_slot: ReplicaSlot,
}

/// Picks the one container this machine may serve logs for.
///
/// A machine selector narrows the set to its machine; several matching
/// containers (or a selector matching none) refuse with the full hosting
/// list, one machine name per container so stacked replicas repeat. A
/// single match hosted elsewhere refuses as the remote owner.
fn select_log_container<'containers>(
    service_id: &ServiceRowId,
    containers: &'containers [ReplicaContainer],
    machine_names: &std::collections::BTreeMap<MachineRowId, MachineName>,
    selector: Option<&MachineName>,
    local_machine_id: &MachineRowId,
) -> Result<&'containers ReplicaContainer, ServiceLogsRefusal> {
    let hosting_machines = || {
        containers
            .iter()
            .filter_map(|container| machine_names.get(&container.machine_id).cloned())
            .collect::<Vec<_>>()
    };
    // A selector refusal must offer at least one name `--machine` accepts;
    // when no hosting roster row resolved to a name, the ids point at the
    // machines lens instead of an empty selector list.
    let selector_refusal = || {
        let machines = hosting_machines();
        if machines.is_empty() {
            return ServiceLogsRefusal::HostingMachinesUnresolved {
                machine_ids: containers
                    .iter()
                    .map(|container| container.machine_id.clone())
                    .collect(),
            };
        }
        ServiceLogsRefusal::MachineSelectorRequired { machines }
    };
    let matching = match selector {
        Some(selector) => containers
            .iter()
            .filter(|container| machine_names.get(&container.machine_id) == Some(selector))
            .collect::<Vec<_>>(),
        None => containers.iter().collect::<Vec<_>>(),
    };
    match matching.as_slice() {
        [] => {
            if selector.is_some() && !containers.is_empty() {
                return Err(selector_refusal());
            }
            Err(ServiceLogsRefusal::ContainerNotFound {
                service_id: service_id.clone(),
            })
        }
        [container] => {
            if container.machine_id != *local_machine_id {
                return Err(ServiceLogsRefusal::RemoteOwner {
                    machine_id: container.machine_id.clone(),
                    machine_name: machine_names.get(&container.machine_id).cloned(),
                });
            }
            Ok(container)
        }
        // ponytail: stacked replicas on one machine also land here; per-container
        // log selection is a future ticket.
        [_, _, ..] => Err(selector_refusal()),
    }
}

pub(super) async fn verify_local_log_target<Runner>(
    runner: &Runner,
    target: &LocalServiceLogTarget,
) -> Result<(), ServiceLogAccessError>
where
    Runner: V2MachineContainerRunner + Sync,
{
    let containers = runner
        .existing_v2_managed_containers()
        .await
        .map_err(|error| ServiceLogAccessError::RuntimeUnavailable {
            message: format!("could not inventory managed containers: {error:?}"),
        })?;
    verify_container_identity(target, &containers).map_err(ServiceLogAccessError::Refusal)
}

fn verify_container_identity(
    target: &LocalServiceLogTarget,
    containers: &[ExistingV2ManagedContainer],
) -> Result<(), ServiceLogsRefusal> {
    let Some(container) = containers
        .iter()
        .find(|container| container.container_id == target.container_id)
    else {
        return Err(ServiceLogsRefusal::ContainerNotFound {
            service_id: target.service_id.clone(),
        });
    };
    if container.identity != target.identity {
        return Err(ServiceLogsRefusal::UnmanagedContainer {
            container_id: target.container_id.clone(),
        });
    }
    Ok(())
}

pub(super) async fn tail_local_service_logs<Runner>(
    runner: &Runner,
    target: &LocalServiceLogTarget,
    tail_lines: u16,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<ServiceLogsTailReply, ServiceLogAccessError>
where
    Runner: V2MachineLogReader + Sync,
{
    let batch = runner
        .tail_v2_container_logs(&target.container_id, tail_lines, shutdown)
        .await
        .map_err(|error| runtime_access_error(target, error))?;
    let truncated = batch.truncated;
    Ok(ServiceLogsTailReply {
        lines: public_lines(batch),
        truncated,
    })
}

fn runtime_access_error(
    target: &LocalServiceLogTarget,
    error: V2MachineLogReadError,
) -> ServiceLogAccessError {
    match error {
        V2MachineLogReadError::NotFound { .. } => {
            ServiceLogAccessError::Refusal(ServiceLogsRefusal::ContainerNotFound {
                service_id: target.service_id.clone(),
            })
        }
        V2MachineLogReadError::RuntimeUnavailable => ServiceLogAccessError::RuntimeUnavailable {
            message: "container runtime unavailable".to_owned(),
        },
        V2MachineLogReadError::ReadFailed => ServiceLogAccessError::RuntimeUnavailable {
            message: "container runtime log read failed".to_owned(),
        },
        V2MachineLogReadError::TimedOut => ServiceLogAccessError::RuntimeUnavailable {
            message: "container runtime log read timed out".to_owned(),
        },
        V2MachineLogReadError::Cancelled => ServiceLogAccessError::RuntimeUnavailable {
            message: "container runtime log read cancelled".to_owned(),
        },
    }
}

fn public_lines(batch: RuntimeLogBatch) -> Vec<ServiceLogLine> {
    batch
        .lines
        .into_iter()
        .map(|line| ServiceLogLine {
            stream: match line.stream {
                RuntimeLogStream::Stdout => ServiceLogStream::Stdout,
                RuntimeLogStream::Stderr => ServiceLogStream::Stderr,
            },
            line: line.line,
        })
        .collect()
}

/// Adds a gap before every attach after the first. A caller can retry this state machine after
/// any read error without implying continuity that Docker cannot prove.
#[derive(Debug)]
pub(super) struct LossyServiceLogFollow {
    attached_once: bool,
}

impl LossyServiceLogFollow {
    #[must_use]
    pub(super) const fn new() -> Self {
        Self {
            attached_once: false,
        }
    }

    pub(super) async fn open<Runner>(
        &mut self,
        runner: &Runner,
        target: &LocalServiceLogTarget,
        initial_tail_lines: u16,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<ServiceLogFollowSession, ServiceLogAccessError>
    where
        Runner: V2MachineLogReader + Sync,
    {
        let reconnected = std::mem::replace(&mut self.attached_once, true);
        let tail_lines = if reconnected { 0 } else { initial_tail_lines };
        let inner = runner
            .open_v2_container_log_follow(&target.container_id, tail_lines, shutdown)
            .await
            .map_err(|error| runtime_access_error(target, error))?;
        Ok(ServiceLogFollowSession {
            gap_pending: reconnected,
            target: target.clone(),
            inner,
        })
    }
}

pub(super) struct ServiceLogFollowSession {
    gap_pending: bool,
    target: LocalServiceLogTarget,
    inner: RuntimeLogFollow,
}

impl ServiceLogFollowSession {
    /// Emits a reconnect gap before reading any line from the reopened runtime attachment.
    pub(super) async fn next(
        &mut self,
    ) -> Result<Option<ServiceLogsFollowEvent>, ServiceLogAccessError> {
        if std::mem::take(&mut self.gap_pending) {
            return Ok(Some(ServiceLogsFollowEvent::Gap));
        }
        self.inner
            .next_line()
            .await
            .map(|line| {
                line.map(|line| ServiceLogsFollowEvent::Line {
                    log: ServiceLogLine {
                        stream: match line.stream {
                            RuntimeLogStream::Stdout => ServiceLogStream::Stdout,
                            RuntimeLogStream::Stderr => ServiceLogStream::Stderr,
                        },
                        line: line.line,
                    },
                })
            })
            .map_err(|error| runtime_access_error(&self.target, error))
    }
}

struct HttpServiceLogFollowState {
    runner: Arc<crate::roles::api::execution::docker::runner::DockerManagedContainerRunner>,
    target: LocalServiceLogTarget,
    follow: LossyServiceLogFollow,
    session: ServiceLogFollowSession,
    shutdown: tokio::sync::watch::Receiver<bool>,
    keepalive: tokio::time::Interval,
    reattach_backoff: Duration,
    done: bool,
}

enum HttpFollowPoll {
    Event(Result<Option<ServiceLogsFollowEvent>, ServiceLogAccessError>),
    Keepalive,
    Shutdown,
}

fn service_log_follow_body(
    runner: Arc<crate::roles::api::execution::docker::runner::DockerManagedContainerRunner>,
    target: LocalServiceLogTarget,
    follow: LossyServiceLogFollow,
    session: ServiceLogFollowSession,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> HttpBody {
    let mut keepalive = tokio::time::interval_at(
        tokio::time::Instant::now() + LOG_KEEPALIVE_INTERVAL,
        LOG_KEEPALIVE_INTERVAL,
    );
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let stream = stream::unfold(
        HttpServiceLogFollowState {
            runner,
            target,
            follow,
            session,
            shutdown,
            keepalive,
            reattach_backoff: LOG_REATTACH_INITIAL_BACKOFF,
            done: false,
        },
        |mut state| async move {
            if state.done {
                return None;
            }
            loop {
                let poll = tokio::select! {
                    biased;
                    changed = state.shutdown.changed() => match changed {
                        Ok(()) if *state.shutdown.borrow() => HttpFollowPoll::Shutdown,
                        Ok(()) => continue,
                        Err(_) => HttpFollowPoll::Shutdown,
                    },
                    event = state.session.next() => HttpFollowPoll::Event(event),
                    _ = state.keepalive.tick() => HttpFollowPoll::Keepalive,
                };
                match poll {
                    HttpFollowPoll::Event(Ok(Some(event))) => {
                        if matches!(event, ServiceLogsFollowEvent::Line { .. }) {
                            state.reattach_backoff = LOG_REATTACH_INITIAL_BACKOFF;
                        }
                        return Some((
                            Ok::<_, Infallible>(Frame::data(encoded_follow_event(&event))),
                            state,
                        ));
                    }
                    HttpFollowPoll::Event(Ok(None) | Err(_)) => {
                        if !wait_for_log_reattach(&mut state).await {
                            return None;
                        }
                        match state
                            .follow
                            .open(
                                state.runner.as_ref(),
                                &state.target,
                                0,
                                state.shutdown.clone(),
                            )
                            .await
                        {
                            Ok(session) => {
                                state.session = session;
                                state.reattach_backoff = state
                                    .reattach_backoff
                                    .saturating_mul(2)
                                    .min(LOG_REATTACH_MAX_BACKOFF);
                            }
                            Err(error) => {
                                state.done = true;
                                let event = ServiceLogsFollowEvent::Terminal {
                                    refusal: error.public_refusal(&state.target),
                                };
                                return Some((
                                    Ok(Frame::data(encoded_follow_event(&event))),
                                    state,
                                ));
                            }
                        }
                    }
                    HttpFollowPoll::Keepalive => {
                        return Some((Ok(Frame::data(sse_keepalive())), state));
                    }
                    HttpFollowPoll::Shutdown => return None,
                }
            }
        },
    );
    BodyExt::boxed(StreamBody::new(stream))
}

async fn wait_for_log_reattach(state: &mut HttpServiceLogFollowState) -> bool {
    tokio::select! {
        biased;
        changed = state.shutdown.changed() => {
            match changed {
                Ok(()) => !*state.shutdown.borrow(),
                Err(_) => false,
            }
        }
        () = tokio::time::sleep(state.reattach_backoff) => true,
    }
}

fn encoded_follow_event(event: &ServiceLogsFollowEvent) -> Bytes {
    let json = match serde_json::to_vec(event) {
        Ok(json) => json,
        Err(error) => {
            tracing::error!(%error, "could not encode service log SSE event");
            return Bytes::new();
        }
    };
    let data = sse_data(&json);
    let mut frame = Vec::with_capacity(event.event_name().len() + data.len() + 8);
    frame.extend_from_slice(b"event: ");
    frame.extend_from_slice(event.event_name().as_bytes());
    frame.push(b'\n');
    frame.extend_from_slice(&data);
    Bytes::from(frame)
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use ployz_core::ServiceLogsFollowEvent;
    use ployz_core::corrosion::V2ManagedContainerIdentity;
    use ployz_core::ids::{ContainerId, NamespaceRowId, OperationRowId, ServiceRowId};
    use ployz_core::machine::runtime::ContainerHealth;

    use ployz_core::ServiceLogsRefusal;
    use ployz_core::machine::MachineName;

    use std::time::Duration;

    use bytes::Bytes;
    use hyper::Response;
    use ployz_core::ids::{ClusterId, MachineRowId};

    use super::{
        CorrosionServiceLogResolver, ExistingV2ManagedContainer, LocalServiceLogTarget,
        LossyServiceLogFollow, ReplicaContainer, RuntimeLogBatch, RuntimeLogFollow,
        RuntimeLogStream, V2MachineLogReadError, V2MachineLogReader, public_lines,
        select_log_container, tail_local_service_logs, verify_container_identity,
    };

    fn service_id(value: &str) -> ServiceRowId {
        ServiceRowId::try_new(value).expect("service")
    }

    fn target() -> LocalServiceLogTarget {
        LocalServiceLogTarget {
            service_id: service_id("01J00000000000000000000001"),
            machine_id: ployz_core::ids::MachineRowId::try_new("01J00000000000000000000005")
                .expect("machine"),
            container_id: ContainerId::try_new("container-one").expect("container"),
            identity: V2ManagedContainerIdentity {
                namespace_id: NamespaceRowId::try_new("01J00000000000000000000002")
                    .expect("namespace"),
                service_id: service_id("01J00000000000000000000001"),
                operation_id: OperationRowId::try_new("01J00000000000000000000003")
                    .expect("operation"),
                replica_slot: ployz_core::deploy::ReplicaSlot::Global,
            },
        }
    }

    fn existing(identity: V2ManagedContainerIdentity) -> ExistingV2ManagedContainer {
        ExistingV2ManagedContainer {
            container_id: ContainerId::try_new("container-one").expect("container"),
            identity,
            state: crate::roles::api::runner::ExistingManagedContainerState::Running {
                ip: Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))),
                health: ContainerHealth::None,
                started_at_unix_ms: None,
            },
            health_status: None,
            resolved_image_identity: None,
            created_at_unix_seconds: None,
        }
    }

    fn machine_row(value: &str) -> ployz_core::ids::MachineRowId {
        ployz_core::ids::MachineRowId::try_new(value).expect("machine row id")
    }

    fn replica(container: &str, machine: &str) -> ReplicaContainer {
        ReplicaContainer {
            container_id: ContainerId::try_new(container).expect("container"),
            machine_id: machine_row(machine),
            replica_slot: ployz_core::deploy::ReplicaSlot::Global,
        }
    }

    fn names(
        entries: &[(&str, &str)],
    ) -> std::collections::BTreeMap<ployz_core::ids::MachineRowId, MachineName> {
        entries
            .iter()
            .map(|(id, name)| (machine_row(id), MachineName::try_new(*name).expect("name")))
            .collect()
    }

    const LOCAL: &str = "01J00000000000000000000005";
    const REMOTE: &str = "01J00000000000000000000006";

    #[test]
    fn replicas_on_several_machines_require_a_machine_selector() {
        let containers = [
            replica("container-one", LOCAL),
            replica("container-two", REMOTE),
        ];
        let names = names(&[(LOCAL, "edge-a"), (REMOTE, "edge-b")]);
        let refusal = select_log_container(
            &service_id("01J00000000000000000000001"),
            &containers,
            &names,
            None,
            &machine_row(LOCAL),
        )
        .expect_err("selector required");
        let ServiceLogsRefusal::MachineSelectorRequired { machines } = refusal else {
            panic!("expected machine selector refusal, got {refusal:?}");
        };
        assert_eq!(
            machines,
            vec![
                MachineName::try_new("edge-a").expect("name"),
                MachineName::try_new("edge-b").expect("name"),
            ]
        );
    }

    #[test]
    fn a_machine_selector_picks_the_local_replica() {
        let containers = [
            replica("container-one", LOCAL),
            replica("container-two", REMOTE),
        ];
        let names = names(&[(LOCAL, "edge-a"), (REMOTE, "edge-b")]);
        let selector = MachineName::try_new("edge-a").expect("name");
        let selected = select_log_container(
            &service_id("01J00000000000000000000001"),
            &containers,
            &names,
            Some(&selector),
            &machine_row(LOCAL),
        )
        .expect("local replica");
        assert_eq!(selected.container_id.as_str(), "container-one");
    }

    #[test]
    fn a_remote_replica_refuses_with_the_owning_machine_named() {
        let containers = [replica("container-two", REMOTE)];
        let names = names(&[(REMOTE, "edge-b")]);
        let refusal = select_log_container(
            &service_id("01J00000000000000000000001"),
            &containers,
            &names,
            None,
            &machine_row(LOCAL),
        )
        .expect_err("remote owner");
        assert_eq!(
            refusal,
            ServiceLogsRefusal::RemoteOwner {
                machine_id: machine_row(REMOTE),
                machine_name: Some(MachineName::try_new("edge-b").expect("name")),
            }
        );
    }

    #[test]
    fn a_selector_matching_no_replica_lists_where_the_service_runs() {
        let containers = [replica("container-two", REMOTE)];
        let names = names(&[(REMOTE, "edge-b")]);
        let selector = MachineName::try_new("edge-c").expect("name");
        let refusal = select_log_container(
            &service_id("01J00000000000000000000001"),
            &containers,
            &names,
            Some(&selector),
            &machine_row(LOCAL),
        )
        .expect_err("selector miss");
        assert_eq!(
            refusal,
            ServiceLogsRefusal::MachineSelectorRequired {
                machines: vec![MachineName::try_new("edge-b").expect("name")],
            }
        );
    }

    #[test]
    fn stacked_replicas_repeat_their_machine_in_the_selector_refusal() {
        let containers = [
            replica("container-one", LOCAL),
            replica("container-two", LOCAL),
        ];
        let names = names(&[(LOCAL, "edge-a")]);
        let selector = MachineName::try_new("edge-a").expect("name");
        let refusal = select_log_container(
            &service_id("01J00000000000000000000001"),
            &containers,
            &names,
            Some(&selector),
            &machine_row(LOCAL),
        )
        .expect_err("stacked replicas cannot be disambiguated");
        assert_eq!(
            refusal,
            ServiceLogsRefusal::MachineSelectorRequired {
                machines: vec![
                    MachineName::try_new("edge-a").expect("name"),
                    MachineName::try_new("edge-a").expect("name"),
                ],
            }
        );
    }

    #[test]
    fn no_containers_at_all_stay_a_container_not_found_refusal() {
        let refusal = select_log_container(
            &service_id("01J00000000000000000000001"),
            &[],
            &std::collections::BTreeMap::new(),
            None,
            &machine_row(LOCAL),
        )
        .expect_err("nothing to serve");
        assert!(matches!(
            refusal,
            ServiceLogsRefusal::ContainerNotFound { .. }
        ));
    }

    #[test]
    fn docker_identity_must_match_the_row_only_contract_exactly() {
        let target = target();
        assert!(verify_container_identity(&target, &[existing(target.identity.clone())]).is_ok());
        let mut wrong = target.identity.clone();
        wrong.operation_id =
            OperationRowId::try_new("01J00000000000000000000004").expect("operation");
        assert!(verify_container_identity(&target, &[existing(wrong)]).is_err());
        assert!(verify_container_identity(&target, &[]).is_err());
    }

    struct Runtime {
        truncated: bool,
        follow_tails: std::sync::Mutex<Vec<u16>>,
    }

    fn runtime(truncated: bool) -> Runtime {
        Runtime {
            truncated,
            follow_tails: std::sync::Mutex::new(Vec::new()),
        }
    }

    impl V2MachineLogReader for Runtime {
        async fn tail_v2_container_logs(
            &self,
            _container_id: &ContainerId,
            _tail_lines: u16,
            _shutdown: tokio::sync::watch::Receiver<bool>,
        ) -> Result<RuntimeLogBatch, V2MachineLogReadError> {
            Ok(RuntimeLogBatch {
                lines: Vec::new(),
                truncated: self.truncated,
            })
        }

        async fn open_v2_container_log_follow(
            &self,
            _container_id: &ContainerId,
            tail_lines: u16,
            _shutdown: tokio::sync::watch::Receiver<bool>,
        ) -> Result<RuntimeLogFollow, V2MachineLogReadError> {
            self.follow_tails
                .lock()
                .expect("tail records")
                .push(tail_lines);
            let (sender, receiver) = tokio::sync::mpsc::channel(2);
            sender
                .try_send(Ok(crate::roles::api::runner::RuntimeLogLine {
                    stream: RuntimeLogStream::Stdout,
                    line: "ready".to_owned(),
                }))
                .expect("send line");
            Ok(RuntimeLogFollow::new(receiver))
        }
    }

    #[tokio::test]
    async fn every_reconnect_is_preceded_by_an_explicit_gap() {
        let mut follow = LossyServiceLogFollow::new();
        let runtime = runtime(false);
        let (_shutdown, shutdown) = tokio::sync::watch::channel(false);
        let mut first = follow
            .open(&runtime, &target(), 10, shutdown.clone())
            .await
            .expect("first");
        assert!(matches!(
            first.next().await.expect("first line"),
            Some(ServiceLogsFollowEvent::Line { .. })
        ));
        assert!(first.next().await.expect("first eof").is_none());
        let mut second = follow
            .open(&runtime, &target(), 10, shutdown)
            .await
            .expect("second");
        assert!(matches!(
            second.next().await.expect("gap"),
            Some(ServiceLogsFollowEvent::Gap)
        ));
        assert!(matches!(
            second.next().await.expect("second line"),
            Some(ServiceLogsFollowEvent::Line { .. })
        ));
        assert_eq!(*runtime.follow_tails.lock().expect("tail records"), [10, 0]);
    }

    #[tokio::test]
    async fn a_healthy_session_emits_lines_immediately_without_periodic_gaps() {
        let mut follow = LossyServiceLogFollow::new();
        let (_shutdown, shutdown) = tokio::sync::watch::channel(false);
        let mut session = follow
            .open(&runtime(false), &target(), 10, shutdown)
            .await
            .expect("session");
        assert!(matches!(
            session.next().await.expect("line"),
            Some(ServiceLogsFollowEvent::Line { .. })
        ));
        assert!(session.next().await.expect("eof").is_none());
    }

    #[tokio::test]
    async fn tail_preserves_the_runtime_truncation_flag() {
        let (_shutdown, shutdown) = tokio::sync::watch::channel(false);
        let reply = tail_local_service_logs(&runtime(true), &target(), 10, shutdown)
            .await
            .expect("tail");
        assert!(reply.truncated);
    }

    #[test]
    fn runtime_stdout_and_stderr_remain_distinct() {
        let lines = public_lines(RuntimeLogBatch {
            lines: vec![
                crate::roles::api::runner::RuntimeLogLine {
                    stream: RuntimeLogStream::Stdout,
                    line: "out".to_owned(),
                },
                crate::roles::api::runner::RuntimeLogLine {
                    stream: RuntimeLogStream::Stderr,
                    line: "err".to_owned(),
                },
            ],
            truncated: false,
        });
        let [stdout, stderr] = lines.as_slice() else {
            panic!("two log lines");
        };
        assert_eq!(stdout.stream, ployz_core::ServiceLogStream::Stdout);
        assert_eq!(stderr.stream, ployz_core::ServiceLogStream::Stderr);
    }

    /// One fake Corrosion query endpoint serving `id, document` rows keyed by
    /// the statement's `FROM` table, so the resolver's reads run over the
    /// real wire protocol and column contract.
    async fn spawn_fake_corrosion(
        rows_by_table: std::collections::BTreeMap<&'static str, Vec<(String, String)>>,
    ) -> std::net::SocketAddr {
        use http_body_util::BodyExt as _;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake corrosion");
        let addr = listener.local_addr().expect("local addr");
        let rows_by_table = std::sync::Arc::new(rows_by_table);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let rows_by_table = std::sync::Arc::clone(&rows_by_table);
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(
                        move |request: hyper::Request<hyper::body::Incoming>| {
                            let rows_by_table = std::sync::Arc::clone(&rows_by_table);
                            async move {
                                let body = request
                                    .into_body()
                                    .collect()
                                    .await
                                    .expect("request body")
                                    .to_bytes();
                                let statement: serde_json::Value =
                                    serde_json::from_slice(&body).expect("statement json");
                                let sql = match statement.as_str() {
                                    Some(sql) => sql.to_owned(),
                                    None => statement
                                        .get(0)
                                        .and_then(serde_json::Value::as_str)
                                        .expect("parameterized sql")
                                        .to_owned(),
                                };
                                let rows = rows_by_table
                                    .iter()
                                    .find(|(table, _)| sql.contains(&format!("FROM {table}")))
                                    .map(|(_, rows)| rows)
                                    .unwrap_or_else(|| panic!("unexpected query: {sql}"));
                                let mut ndjson =
                                    String::from("{\"columns\":[\"id\",\"document\"]}\n");
                                for (index, (id, document)) in rows.iter().enumerate() {
                                    ndjson.push_str(
                                        &serde_json::to_string(&serde_json::json!({
                                            "row": [index + 1, [id, document]]
                                        }))
                                        .expect("row frame"),
                                    );
                                    ndjson.push('\n');
                                }
                                ndjson.push_str("{\"eoq\":{\"time\":0.0}}\n");
                                Ok::<_, std::convert::Infallible>(Response::new(
                                    http_body_util::Full::new(Bytes::from(ndjson))
                                        .map_err(|never| match never {})
                                        .boxed(),
                                ))
                            }
                        },
                    );
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        addr
    }

    fn corrosion_client(addr: std::net::SocketAddr) -> crate::corrosion::CorrosionClient {
        crate::corrosion::CorrosionClient::new(
            crate::corrosion::CorrosionClientConfig::new(
                addr,
                crate::corrosion::BearerToken::new("test-token").expect("token"),
                crate::corrosion::CorrosionClientBounds {
                    connect_timeout: Duration::from_secs(1),
                    request_timeout: Duration::from_secs(1),
                    stream_idle_timeout: Duration::from_secs(1),
                    max_ndjson_frame_bytes: 64 * 1024,
                    max_error_body_bytes: 1024,
                },
            )
            .expect("config"),
        )
        .expect("client")
    }

    const RESOLVER_CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const RESOLVER_MACHINE_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const RESOLVER_MACHINE_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const RESOLVER_PEER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
    const RESOLVER_SERVICE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB1";
    const RESOLVER_NAMESPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB2";
    const RESOLVER_DEPLOY: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB3";

    fn resolver_cluster_document() -> String {
        serde_json::json!({
            "v": 1,
            "cluster_id": RESOLVER_CLUSTER,
            "name": "acme-prod",
            "storage_default": "plain",
            "hostname_mode": { "mode": "disabled" },
            "prefix": "10.210.0.0/16",
            "provider": "builtin_wireguard",
            "acme_directory_url": "https://acme.example/directory",
            "acme_contact": null,
            "written_by": { "kind": "peer", "peer_id": RESOLVER_PEER },
            "written_at": "2026-08-04T10:00:00Z"
        })
        .to_string()
    }

    fn resolver_machine_document(name: &str, addr_v6: &str) -> String {
        serde_json::json!({
            "v": 1,
            "cluster_id": RESOLVER_CLUSTER,
            "name": name,
            "lifecycle": "active",
            "transport": {
                "kind": "wireguard",
                "pubkey": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                "addr_v6": addr_v6,
                "endpoint": null,
                "subnet_v4": "10.210.20.0/24"
            },
            "storage": { "mode": "plain", "reason": { "kind": "default" } },
            "written_by": { "kind": "peer", "peer_id": RESOLVER_PEER },
            "written_at": "2026-08-04T10:00:00Z"
        })
        .to_string()
    }

    fn resolver_service_document() -> String {
        serde_json::json!({
            "v": 1,
            "cluster_id": RESOLVER_CLUSTER,
            "written_by": { "kind": "peer", "peer_id": RESOLVER_PEER },
            "written_at": "2026-08-04T10:00:00Z",
            "namespace_id": RESOLVER_NAMESPACE,
            "name": "api",
            "image": "registry.example/api:latest",
            "env_fingerprints": {},
            "mode": "replicated",
            "replicas": 2,
            "pinned_machines": [],
            "active_deploy": RESOLVER_DEPLOY,
            "previous_image": null,
            "deployed_at": "2026-08-04T10:00:00Z",
            "operation_id": RESOLVER_DEPLOY
        })
        .to_string()
    }

    fn resolver_container_document(machine_id: &str, ip: &str) -> String {
        serde_json::json!({
            "v": 1,
            "cluster_id": RESOLVER_CLUSTER,
            "machine_id": machine_id,
            "service_id": RESOLVER_SERVICE,
            "namespace_id": RESOLVER_NAMESPACE,
            "ip": ip,
            "deploy": RESOLVER_DEPLOY
        })
        .to_string()
    }

    fn resolver_rows() -> std::collections::BTreeMap<&'static str, Vec<(String, String)>> {
        std::collections::BTreeMap::from([
            (
                "cluster",
                vec![(RESOLVER_CLUSTER.to_owned(), resolver_cluster_document())],
            ),
            (
                "machines",
                vec![
                    (
                        RESOLVER_MACHINE_A.to_owned(),
                        resolver_machine_document("edge-a", "fd00::20"),
                    ),
                    (
                        RESOLVER_MACHINE_B.to_owned(),
                        resolver_machine_document("edge-b", "fd00::21"),
                    ),
                ],
            ),
            ("peers", Vec::new()),
            (
                "services",
                vec![(RESOLVER_SERVICE.to_owned(), resolver_service_document())],
            ),
            (
                "containers",
                vec![
                    (
                        "aaaaaaaaaaaa".to_owned(),
                        resolver_container_document(RESOLVER_MACHINE_A, "10.210.20.9"),
                    ),
                    (
                        "bbbbbbbbbbbb".to_owned(),
                        resolver_container_document(RESOLVER_MACHINE_B, "10.210.21.9"),
                    ),
                ],
            ),
        ])
    }

    #[tokio::test]
    async fn resolver_names_hosting_machines_through_real_machines_rows() {
        let addr = spawn_fake_corrosion(resolver_rows()).await;
        let resolver = CorrosionServiceLogResolver::new(
            corrosion_client(addr),
            ClusterId::try_new(RESOLVER_CLUSTER).expect("cluster id"),
            MachineRowId::try_new(RESOLVER_MACHINE_A).expect("machine id"),
        );
        let service_id = ServiceRowId::try_new(RESOLVER_SERVICE).expect("service id");

        let refusal = resolver
            .resolve(&service_id, None)
            .await
            .expect("resolution reads succeed")
            .expect_err("two hosting machines require a selector");
        assert_eq!(
            refusal,
            ServiceLogsRefusal::MachineSelectorRequired {
                machines: vec![
                    MachineName::try_new("edge-a").expect("name"),
                    MachineName::try_new("edge-b").expect("name"),
                ],
            }
        );

        let selector = MachineName::try_new("edge-a").expect("name");
        let target = resolver
            .resolve(&service_id, Some(&selector))
            .await
            .expect("resolution reads succeed")
            .expect("the selector resolves the local replica");
        assert_eq!(target.container_id.as_str(), "aaaaaaaaaaaa");
        assert_eq!(
            target.machine_id,
            MachineRowId::try_new(RESOLVER_MACHINE_A).expect("machine id")
        );
    }
}
