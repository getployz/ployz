use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::stream;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use hyper::{Response, StatusCode};
use ployz_core::corrosion::{
    CorrosionServiceName, NamespaceDocument, SqliteParameter, Statement,
    V2ManagedContainerIdentity, read_named_rows,
};
use ployz_core::ids::{ClusterName, ContainerId, CorrosionNamespaceName};
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
const MAX_LOG_REQUEST_BYTES: usize = 1_024;
const LOG_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const LOG_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const LOG_REATTACH_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const LOG_REATTACH_MAX_BACKOFF: Duration = Duration::from_secs(2);

pub(super) async fn handle_tail(
    service: &ApiService,
    namespace_name: CorrosionNamespaceName,
    service_name: CorrosionServiceName,
    request: hyper::Request<hyper::body::Incoming>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Response<HttpBody> {
    let request = match decode_log_request(request.into_body(), LogRequestBody::Required).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (runner, target) = match resolve_log_target(
        service,
        &namespace_name,
        &service_name,
        request.machine.as_ref(),
    )
    .await
    {
        Ok(target) => target,
        Err(response) => return response,
    };
    match tail_local_service_logs(runner.as_ref(), &target, request.tail_lines, shutdown).await {
        Ok(reply) => super::mutations::typed_response(StatusCode::OK, &reply),
        Err(error) => log_refusal_response(error.public_refusal(&target)),
    }
}

pub(super) async fn handle_follow(
    service: &ApiService,
    namespace_name: CorrosionNamespaceName,
    service_name: CorrosionServiceName,
    request: hyper::Request<hyper::body::Incoming>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Response<HttpBody> {
    let request = match decode_log_request(request.into_body(), LogRequestBody::Optional).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    let (runner, target) = match resolve_log_target(
        service,
        &namespace_name,
        &service_name,
        request.machine.as_ref(),
    )
    .await
    {
        Ok(target) => target,
        Err(response) => return response,
    };
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
        runner, target, follow, session, shutdown,
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
    namespace_name: &CorrosionNamespaceName,
    service_name: &CorrosionServiceName,
    machine: Option<&MachineName>,
) -> Result<
    (
        Arc<crate::roles::api::execution::docker::runner::DockerManagedContainerRunner>,
        LocalServiceLogTarget,
    ),
    Response<HttpBody>,
> {
    let resolver =
        CorrosionServiceLogResolver::new(service.corrosion.clone(), service.cluster_id.clone());
    let generation = match resolver.resolve(namespace_name, service_name).await {
        Ok(Ok(generation)) => generation,
        Ok(Err(refusal)) => return Err(log_refusal_response(refusal)),
        Err(error) => {
            tracing::warn!(%error, "service log target resolution failed");
            return Err(corrosion_unavailable_response());
        }
    };
    let Some(runner) = service.container_runner.clone() else {
        return Err(log_refusal_response(
            ServiceLogsRefusal::RuntimeUnavailable {
                machine_id: service.local_machine_id.clone(),
            },
        ));
    };
    let containers = runner
        .existing_v2_managed_containers()
        .await
        .map_err(|error| {
            tracing::warn!(
                ?error,
                "could not inventory local containers for service logs"
            );
            log_refusal_response(ServiceLogsRefusal::RuntimeUnavailable {
                machine_id: service.local_machine_id.clone(),
            })
        })?;
    let target = select_local_log_container(
        namespace_name,
        service_name,
        &generation,
        &containers,
        machine,
        &service.local_machine_id,
    )
    .map_err(log_refusal_response)?;
    Ok((runner, target))
}

fn log_refusal_response(refusal: ServiceLogsRefusal) -> Response<HttpBody> {
    let status = match &refusal {
        ServiceLogsRefusal::ServiceNotFound { .. }
        | ServiceLogsRefusal::ContainerNotFound { .. } => StatusCode::NOT_FOUND,
        ServiceLogsRefusal::ContainerAmbiguous { .. }
        | ServiceLogsRefusal::MachineSelectorRequired { .. }
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

/// A service-log target resolved from namespace intent and local Docker reality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LocalServiceLogTarget {
    pub(super) machine_id: MachineName,
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

/// Exact namespace-intent resolution for service logs.
#[derive(Clone)]
pub(super) struct CorrosionServiceLogResolver {
    client: CorrosionClient,
    cluster_id: ClusterName,
}

impl CorrosionServiceLogResolver {
    #[must_use]
    pub(super) const fn new(client: CorrosionClient, cluster_id: ClusterName) -> Self {
        Self { client, cluster_id }
    }

    pub(super) async fn resolve(
        &self,
        namespace_name: &CorrosionNamespaceName,
        service_name: &CorrosionServiceName,
    ) -> Result<Result<ployz_core::ids::DeployName, ServiceLogsRefusal>, ServiceLogResolveError>
    {
        let namespace_rows = self
            .query(
                Statement::with_params(
                    "SELECT id, document FROM namespaces WHERE id = ?",
                    vec![SqliteParameter::Text(namespace_name.as_str().to_owned())],
                ),
                MAX_RESOLVED_ROWS,
            )
            .await?;
        let namespace_report =
            read_named_rows::<NamespaceDocument>(&self.cluster_id, namespace_rows);
        if !namespace_report.skipped.is_empty() || namespace_report.accepted.len() > 1 {
            return Err(ServiceLogResolveError::Protocol(
                "namespace lookup contained rejected or ambiguous rows".to_owned(),
            ));
        }
        let Some(namespace) = namespace_report.accepted.into_iter().next() else {
            return Ok(Err(ServiceLogsRefusal::ServiceNotFound {
                namespace_name: namespace_name.clone(),
                service_name: service_name.clone(),
            }));
        };
        let Some(service) = namespace.value.services.get(service_name) else {
            return Ok(Err(ServiceLogsRefusal::ServiceNotFound {
                namespace_name: namespace_name.clone(),
                service_name: service_name.clone(),
            }));
        };
        Ok(Ok(service.active_deploy.clone()))
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

/// Resolves one local Docker container from the generation selected by namespace intent.
fn select_local_log_container(
    namespace_name: &CorrosionNamespaceName,
    service_name: &CorrosionServiceName,
    deploy: &ployz_core::ids::DeployName,
    containers: &[ExistingV2ManagedContainer],
    selector: Option<&MachineName>,
    local_machine_id: &MachineName,
) -> Result<LocalServiceLogTarget, ServiceLogsRefusal> {
    if let Some(selector) = selector
        && selector != local_machine_id
    {
        return Err(ServiceLogsRefusal::RemoteOwner {
            machine_id: selector.clone(),
            machine_name: Some(selector.clone()),
        });
    }
    let matching = containers
        .iter()
        .filter(|container| {
            container.identity.namespace_id == *namespace_name
                && container.identity.service_name == *service_name
                && container.identity.operation_id == *deploy
        })
        .collect::<Vec<_>>();
    let unique = matching
        .iter()
        .map(|container| &container.identity)
        .collect::<std::collections::HashSet<_>>();
    if unique.len() != matching.len() {
        return Err(ServiceLogsRefusal::ContainerAmbiguous {
            namespace_name: namespace_name.clone(),
            service_name: service_name.clone(),
        });
    }
    match matching.as_slice() {
        [] => Err(ServiceLogsRefusal::ContainerNotFound {
            namespace_name: namespace_name.clone(),
            service_name: service_name.clone(),
        }),
        [container] => Ok(LocalServiceLogTarget {
            machine_id: local_machine_id.clone(),
            container_id: container.container_id.clone(),
            identity: container.identity.clone(),
        }),
        [_, _, ..] => Err(ServiceLogsRefusal::MachineSelectorRequired {
            machines: vec![local_machine_id.clone(); matching.len()],
        }),
    }
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
                namespace_name: target.identity.namespace_id.clone(),
                service_name: target.identity.service_name.clone(),
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
    use ployz_core::corrosion::{CorrosionServiceName, V2ManagedContainerIdentity};
    use ployz_core::ids::{ContainerId, CorrosionNamespaceName, DeployName};
    use ployz_core::machine::runtime::ContainerHealth;

    use ployz_core::ServiceLogsRefusal;
    use ployz_core::machine::MachineName;

    use std::time::Duration;

    use bytes::Bytes;
    use hyper::Response;
    use ployz_core::ids::ClusterName;

    use super::{
        CorrosionServiceLogResolver, ExistingV2ManagedContainer, LocalServiceLogTarget,
        LossyServiceLogFollow, RuntimeLogBatch, RuntimeLogFollow, RuntimeLogStream,
        V2MachineLogReadError, V2MachineLogReader, public_lines, select_local_log_container,
        tail_local_service_logs,
    };

    fn namespace(value: &str) -> CorrosionNamespaceName {
        CorrosionNamespaceName::try_new(value).expect("namespace")
    }

    fn service(value: &str) -> CorrosionServiceName {
        CorrosionServiceName::try_new(value).expect("service")
    }

    fn target() -> LocalServiceLogTarget {
        LocalServiceLogTarget {
            machine_id: ployz_core::ids::MachineName::try_new("edge-a").expect("machine"),
            container_id: ContainerId::try_new("container-one").expect("container"),
            identity: V2ManagedContainerIdentity {
                namespace_id: CorrosionNamespaceName::try_new("prod").expect("namespace"),
                service_name: CorrosionServiceName::try_new("api").expect("service"),
                operation_id: DeployName::try_new("blue").expect("operation"),
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
            host_ports: ployz_core::corrosion::HostPortBindings::default(),
        }
    }

    fn machine(value: &str) -> MachineName {
        MachineName::try_new(value).expect("machine")
    }

    fn replica(
        docker_id: &str,
        slot: ployz_core::deploy::ReplicaSlot,
    ) -> ExistingV2ManagedContainer {
        let mut observed = existing(V2ManagedContainerIdentity {
            namespace_id: namespace("prod"),
            service_name: service("api"),
            operation_id: DeployName::try_new("blue").expect("deploy"),
            replica_slot: slot,
        });
        observed.container_id = ContainerId::try_new(docker_id).expect("container");
        observed
    }

    fn slot(number: u16) -> ployz_core::deploy::ReplicaSlot {
        ployz_core::deploy::ReplicaSlot::Replicated {
            number: ployz_core::deploy::ReplicatedReplicaSlot::try_new(number).expect("slot"),
        }
    }

    const LOCAL: &str = "edge-a";
    const REMOTE: &str = "edge-b";

    #[test]
    fn one_local_replica_resolves_by_natural_identity() {
        let selected = select_local_log_container(
            &namespace("prod"),
            &service("api"),
            &DeployName::try_new("blue").expect("deploy"),
            &[replica(
                "container-one",
                ployz_core::deploy::ReplicaSlot::Global,
            )],
            Some(&machine(LOCAL)),
            &machine(LOCAL),
        )
        .expect("local replica");
        assert_eq!(selected.container_id.as_str(), "container-one");
    }

    #[test]
    fn a_remote_machine_selector_stays_a_typed_redirect() {
        let refusal = select_local_log_container(
            &namespace("prod"),
            &service("api"),
            &DeployName::try_new("blue").expect("deploy"),
            &[],
            Some(&machine(REMOTE)),
            &machine(LOCAL),
        )
        .expect_err("remote owner");
        assert_eq!(
            refusal,
            ServiceLogsRefusal::RemoteOwner {
                machine_id: machine(REMOTE),
                machine_name: Some(machine(REMOTE)),
            }
        );
    }

    #[test]
    fn stacked_local_replicas_repeat_the_local_machine_in_the_refusal() {
        let refusal = select_local_log_container(
            &namespace("prod"),
            &service("api"),
            &DeployName::try_new("blue").expect("deploy"),
            &[
                replica("container-one", slot(1)),
                replica("container-two", slot(2)),
            ],
            None,
            &machine(LOCAL),
        )
        .expect_err("stacked replicas cannot be selected by machine");
        assert_eq!(
            refusal,
            ServiceLogsRefusal::MachineSelectorRequired {
                machines: vec![machine(LOCAL), machine(LOCAL)],
            }
        );
    }

    #[test]
    fn absent_local_runtime_evidence_is_container_not_found() {
        let refusal = select_local_log_container(
            &namespace("prod"),
            &service("api"),
            &DeployName::try_new("blue").expect("deploy"),
            &[],
            None,
            &machine(LOCAL),
        )
        .expect_err("nothing to serve");
        assert!(matches!(
            refusal,
            ServiceLogsRefusal::ContainerNotFound { .. }
        ));
    }

    #[test]
    fn duplicate_natural_identity_is_ambiguous() {
        let refusal = select_local_log_container(
            &namespace("prod"),
            &service("api"),
            &DeployName::try_new("blue").expect("deploy"),
            &[
                replica("container-one", ployz_core::deploy::ReplicaSlot::Global),
                replica("container-two", ployz_core::deploy::ReplicaSlot::Global),
            ],
            None,
            &machine(LOCAL),
        )
        .expect_err("duplicate identity");
        assert!(matches!(
            refusal,
            ServiceLogsRefusal::ContainerAmbiguous { .. }
        ));
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

    const RESOLVER_CLUSTER: &str = "main";
    const RESOLVER_PEER: &str = "operator";
    const RESOLVER_NAMESPACE: &str = "production";
    const RESOLVER_DEPLOY: &str = "release-1";

    fn resolver_namespace_document() -> String {
        serde_json::json!({
            "v": 1,
            "cluster_id": RESOLVER_CLUSTER,
            "name": RESOLVER_NAMESPACE,
            "written_by": { "kind": "peer", "peer_id": RESOLVER_PEER },
            "written_at": "2026-08-04T10:00:00Z",
            "services": {
                "api": {
                    "image": "registry.example/api:latest",
                    "env_fingerprints": {},
                    "mode": "replicated",
                    "replicas": 2,
                    "pinned_machines": [],
                    "active_deploy": RESOLVER_DEPLOY,
                    "previous_image": null,
                    "deployed_at": "2026-08-04T10:00:00Z"
                }
            }
        })
        .to_string()
    }

    fn resolver_rows() -> std::collections::BTreeMap<&'static str, Vec<(String, String)>> {
        std::collections::BTreeMap::from([(
            "namespaces",
            vec![(RESOLVER_NAMESPACE.to_owned(), resolver_namespace_document())],
        )])
    }

    #[tokio::test]
    async fn resolver_reads_generation_only_from_exact_namespace_intent() {
        let addr = spawn_fake_corrosion(resolver_rows()).await;
        let resolver = CorrosionServiceLogResolver::new(
            corrosion_client(addr),
            ClusterName::try_new(RESOLVER_CLUSTER).expect("cluster id"),
        );
        let namespace = CorrosionNamespaceName::try_new(RESOLVER_NAMESPACE).expect("namespace");
        let service = CorrosionServiceName::try_new("api").expect("service");

        let generation = resolver
            .resolve(&namespace, &service)
            .await
            .expect("resolution reads succeed")
            .expect("service exists");
        assert_eq!(
            generation,
            DeployName::try_new(RESOLVER_DEPLOY).expect("deploy")
        );
    }
}
