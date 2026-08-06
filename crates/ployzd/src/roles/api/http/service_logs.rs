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
use ployz_core::ids::{ClusterId, ContainerId, MachineRowId, ServiceRowId};
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
    let target = match resolve_log_target(service, &service_id).await {
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
    let target = match resolve_log_target(service, &service_id).await {
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
}

async fn decode_log_request(
    body: hyper::body::Incoming,
    mode: LogRequestBody,
) -> Result<DecodedLogRequest, Response<HttpBody>> {
    let body = read_bounded_body(body, MAX_LOG_REQUEST_BYTES, LOG_REQUEST_TIMEOUT)
        .await
        .map_err(log_body_error)?;
    if body.is_empty() && matches!(mode, LogRequestBody::Optional) {
        return Ok(DecodedLogRequest { tail_lines: 0 });
    }
    let request: ServiceLogsRequest = serde_json::from_slice(&body)
        .map_err(|_| log_request_error(StatusCode::BAD_REQUEST, "invalid_request"))?;
    Ok(DecodedLogRequest {
        tail_lines: request.tail_lines.get(),
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
) -> Result<LocalServiceLogTarget, Response<HttpBody>> {
    let resolver = CorrosionServiceLogResolver::new(
        service.corrosion.clone(),
        service.cluster_id.clone(),
        service.local_machine_id.clone(),
    );
    match resolver.resolve(service_id).await {
        Ok(Ok(target)) => Ok(target),
        Ok(Err(refusal)) => Err(log_refusal_response(refusal)),
        Err(error) => {
            tracing::warn!(%error, "service log target resolution failed");
            Err(corrosion_unavailable_response())
        }
    }
}

fn log_refusal_response(refusal: ServiceLogsRefusal) -> Response<HttpBody> {
    let status =
        match &refusal {
            ServiceLogsRefusal::ServiceNotFound { .. }
            | ServiceLogsRefusal::NoActiveDeploy { .. }
            | ServiceLogsRefusal::ContainerNotFound { .. } => StatusCode::NOT_FOUND,
            ServiceLogsRefusal::UnmanagedContainer { .. }
            | ServiceLogsRefusal::RemoteOwner { .. } => StatusCode::CONFLICT,
            ServiceLogsRefusal::DriverDark { .. }
            | ServiceLogsRefusal::RuntimeUnavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
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
                MAX_RESOLVED_ROWS,
            )
            .await?;
        let container_report = read_rows::<ContainerDocument>(&self.cluster_id, container_rows);
        if !container_report.skipped.is_empty() || container_report.accepted.len() > 1 {
            return Err(ServiceLogResolveError::Protocol(
                "active deploy container lookup contained rejected or ambiguous rows".to_owned(),
            ));
        }
        let Some(container) = container_report.accepted.into_iter().next() else {
            return Ok(Err(ServiceLogsRefusal::ContainerNotFound {
                service_id: service_id.clone(),
            }));
        };
        if container.value.machine_id != self.local_machine_id {
            return Ok(Err(ServiceLogsRefusal::RemoteOwner {
                machine_id: container.value.machine_id,
            }));
        }
        let container_id = ContainerId::try_new(container.source.key).map_err(|error| {
            ServiceLogResolveError::Protocol(format!("container row id was invalid: {error}"))
        })?;
        Ok(Ok(LocalServiceLogTarget {
            service_id: service_id.clone(),
            machine_id: self.local_machine_id.clone(),
            container_id,
            identity: V2ManagedContainerIdentity {
                namespace_id: service.value.namespace_id,
                service_id: service_id.clone(),
                operation_id: service.value.active_deploy,
            },
        }))
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

    use super::{
        ExistingV2ManagedContainer, LocalServiceLogTarget, LossyServiceLogFollow, RuntimeLogBatch,
        RuntimeLogFollow, RuntimeLogStream, V2MachineLogReadError, V2MachineLogReader,
        public_lines, tail_local_service_logs, verify_container_identity,
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
}
