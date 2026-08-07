//! Operation summary lookup and driver-local replay/follow transport.

use std::collections::{BTreeSet, HashMap};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream};
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::header::{CACHE_CONTROL, CONTENT_TYPE, HeaderValue};
use hyper::{Response, StatusCode};
use ployz_core::corrosion::{
    CorrosionBasicTransition, CorrosionDeployFailure, CorrosionDeployOutcome,
    CorrosionDeployServiceResult, CorrosionDeployTransition, CorrosionEvidenceLossReason,
    CorrosionOperation, CorrosionOperationFailure, CorrosionTimestamp, MachineTransport,
    OperationDocument, Principal,
};
use ployz_core::ids::{MachineRowId, OperationRowId};
use ployz_core::{
    DeployRefusal, DeployRequest, HandshakeObservationOutcome, HandshakeObservationUnavailable,
    LensCollection, LensSnapshot, OperationEvidence, OperationLookupRefusal, OperationLookupReply,
    OperationWatchEvent, OperationWatchRefusal, operation_watch_route,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};

use super::deploy::{DeployDriver, DeployDriverError};
use super::deploy_task::AcceptedDeploy;
use super::operation_evidence::{
    DEFAULT_MAX_EVIDENCE_LINE_BYTES, DurablePromotionProgress, EvidenceIdentity,
    OperationEvidenceDirectory, OperationEvidenceError, OperationEvidenceFollower,
    OperationEvidenceLog,
};
use super::operation_lifecycle::{
    OperationShutdownOutcome, OperationTaskRegistry, StartupEvidence, StartupRecoveryAction,
    classify_startup_recovery, operation_is_terminal,
};
use super::operation_proxy::{
    OperationWatchIngress, OperationWatchOwner, owner_routing_refusal,
    resolve_operation_watch_owner,
};
use super::operation_store::{ConditionalOperationWrite, ObservedOperation, OperationStore};
use super::server::{
    ApiService, HttpBody, await_lens_state, corrosion_unavailable_response, json_response,
    sse_data, sse_keepalive, sse_response,
};
use crate::corrosion::CorrosionClient;

const OPERATION_LENS_WAIT: Duration = Duration::from_secs(15);
const MACHINE_HOP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MACHINE_HOP_HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const MACHINE_HOP_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const OPERATION_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const MAX_OWNER_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_OWNER_SSE_FRAME_BYTES: usize = 512 * 1024;
pub(super) const OPERATION_SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

type RemoteByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static>>;

#[async_trait]
pub(super) trait PreparedPromotionResumer: Send + Sync {
    async fn resume_promotion(
        &self,
        operation_id: OperationRowId,
        log: OperationEvidenceLog,
        progress: DurablePromotionProgress,
    ) -> Result<(), String>;
}

#[async_trait]
impl PreparedPromotionResumer for DeployDriver {
    async fn resume_promotion(
        &self,
        operation_id: OperationRowId,
        log: OperationEvidenceLog,
        progress: DurablePromotionProgress,
    ) -> Result<(), String> {
        self.resume_promotion(operation_id, log, progress)
            .await
            .map_err(|error| error.to_string())
    }
}

#[async_trait]
pub(super) trait StartupOperationRows: Send + Sync {
    async fn operation(
        &self,
        operation_id: &OperationRowId,
    ) -> Result<Option<ObservedOperation>, String>;

    async fn local_nonterminal_operations(
        &self,
        machine_id: &MachineRowId,
    ) -> Result<Vec<ObservedOperation>, String>;

    async fn replace_terminal(
        &self,
        observed: &ObservedOperation,
        terminal: &OperationDocument,
    ) -> Result<ConditionalOperationWrite, String>;
}

#[async_trait]
impl StartupOperationRows for OperationStore {
    async fn operation(
        &self,
        operation_id: &OperationRowId,
    ) -> Result<Option<ObservedOperation>, String> {
        self.operation(operation_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn local_nonterminal_operations(
        &self,
        machine_id: &MachineRowId,
    ) -> Result<Vec<ObservedOperation>, String> {
        self.local_nonterminal_operations(machine_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn replace_terminal(
        &self,
        observed: &ObservedOperation,
        terminal: &OperationDocument,
    ) -> Result<ConditionalOperationWrite, String> {
        self.replace_terminal(observed, terminal)
            .await
            .map_err(|error| error.to_string())
    }
}

pub(super) struct OperationRuntime {
    store: OperationStore,
    startup_rows: Arc<dyn StartupOperationRows>,
    evidence: OperationEvidenceDirectory,
    tasks: OperationTaskRegistry,
    live_logs: Arc<Mutex<HashMap<OperationRowId, OperationEvidenceLog>>>,
    keeper_control_socket_path: PathBuf,
    machine_hop: reqwest::Client,
    promotion_resumer: Option<Arc<dyn PreparedPromotionResumer>>,
}

impl OperationRuntime {
    pub(super) fn new(
        corrosion: CorrosionClient,
        cluster_id: ployz_core::ids::ClusterId,
        evidence_directory: PathBuf,
        keeper_control_socket_path: PathBuf,
    ) -> Result<Self, reqwest::Error> {
        let store = OperationStore::new(corrosion, cluster_id);
        Ok(Self {
            startup_rows: Arc::new(store.clone()),
            store,
            evidence: OperationEvidenceDirectory::new(
                evidence_directory,
                DEFAULT_MAX_EVIDENCE_LINE_BYTES,
            ),
            tasks: OperationTaskRegistry::new(),
            live_logs: Arc::new(Mutex::new(HashMap::new())),
            keeper_control_socket_path,
            machine_hop: reqwest::Client::builder()
                .connect_timeout(MACHINE_HOP_CONNECT_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()?,
            promotion_resumer: None,
        })
    }

    #[must_use]
    pub(super) fn with_promotion_resumer(
        mut self,
        resumer: Arc<dyn PreparedPromotionResumer>,
    ) -> Self {
        self.promotion_resumer = Some(resumer);
        self
    }

    pub(super) async fn register_live_log(
        &self,
        operation_id: OperationRowId,
        log: OperationEvidenceLog,
    ) {
        self.live_logs.lock().await.insert(operation_id, log);
    }

    pub(super) fn store(&self) -> OperationStore {
        self.store.clone()
    }

    pub(super) fn evidence_directory(&self) -> OperationEvidenceDirectory {
        self.evidence.clone()
    }

    pub(super) async fn spawn_deploy(
        &self,
        accepted: AcceptedDeploy,
    ) -> Result<ployz_core::DeployAccepted, DeployAdmissionError> {
        let reply = accepted.reply.clone();
        let operation_id = reply.operation_id.clone();
        self.register_live_log(operation_id.clone(), accepted.operation_log())
            .await;
        let accepted = Arc::new(Mutex::new(Some(accepted)));
        let task_accepted = Arc::clone(&accepted);
        let task_operation_id = operation_id.clone();
        let live_logs = Arc::clone(&self.live_logs);
        if self
            .tasks
            .spawn(move |shutdown| async move {
                let accepted = task_accepted.lock().await.take();
                let Some(accepted) = accepted else {
                    return;
                };
                let terminal = match accepted.task.run(shutdown).await {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::error!(operation_id = %task_operation_id, %error, "deploy task failed");
                        match accepted.task.recover_after_error().await {
                            Ok(()) => true,
                            Err(recovery_error) => {
                                tracing::error!(operation_id = %task_operation_id, %recovery_error, "deploy task recovery failed");
                                false
                            }
                        }
                    }
                };
                if terminal {
                    live_logs.lock().await.remove(&task_operation_id);
                }
            })
            .await
        {
            return Ok(reply);
        }
        let accepted = accepted
            .lock()
            .await
            .take()
            .ok_or(DeployAdmissionError::LostRejectedTask)?;
        let interruption = accepted.interrupt_unspawned().await;
        match interruption {
            Ok(()) => {
                self.live_logs.lock().await.remove(&operation_id);
            }
            Err(error) => return Err(DeployAdmissionError::Interrupt(error)),
        }
        Err(DeployAdmissionError::AdmissionClosed)
    }

    async fn live_log(&self, operation_id: &OperationRowId) -> Option<OperationEvidenceLog> {
        self.live_logs.lock().await.get(operation_id).cloned()
    }

    pub(super) async fn recover_startup(
        &self,
        local_machine_id: &MachineRowId,
    ) -> Result<(), OperationStartupError> {
        let operations = self
            .startup_rows
            .local_nonterminal_operations(local_machine_id)
            .await
            .map_err(OperationStartupError::Rows)?;
        for observed in operations {
            self.recover_one(local_machine_id, observed).await?;
        }
        Ok(())
    }

    async fn recover_one(
        &self,
        local_machine_id: &MachineRowId,
        observed: ObservedOperation,
    ) -> Result<(), OperationStartupError> {
        let identity = EvidenceIdentity::new(observed.id.clone(), local_machine_id.clone());
        let opened = self.evidence.open(identity.clone()).await;
        let (log, evidence) = match opened {
            Ok(log) => {
                let recovery = log.recovery_evidence().await?;
                (
                    Some(log),
                    StartupEvidence::Usable {
                        terminal: recovery.terminal.map(Box::new),
                        promotion: recovery.promotion.map(Box::new),
                    },
                )
            }
            Err(error) => (
                None,
                StartupEvidence::Unusable {
                    reason: evidence_loss_reason(&error),
                },
            ),
        };
        let action = classify_startup_recovery(local_machine_id, &observed.document, evidence);
        match action {
            StartupRecoveryAction::IgnoreRemoteOwner
            | StartupRecoveryAction::LeaveTerminal
            | StartupRecoveryAction::TerminalDetailUnavailable => Ok(()),
            StartupRecoveryAction::ReconcileDurableTerminal { operation } => {
                replace_terminal_exact(self.startup_rows.as_ref(), &observed, &operation).await?;
                Ok(())
            }
            StartupRecoveryAction::ResumePreparedPromotion { progress } => {
                let Some(log) = log else {
                    return Err(OperationStartupError::MissingUsableLog);
                };
                self.register_live_log(observed.id.clone(), log.clone())
                    .await;
                let Some(resumer) = self.promotion_resumer.clone() else {
                    return Err(OperationStartupError::MissingPromotionResumer);
                };
                let operation_id = observed.id;
                let task_operation_id = operation_id.clone();
                let live_logs = Arc::clone(&self.live_logs);
                if !self
                    .tasks
                    .spawn(move |_| async move {
                        let resumed = resumer
                            .resume_promotion(task_operation_id.clone(), log, *progress)
                            .await;
                        match resumed {
                            Ok(()) => {
                                live_logs.lock().await.remove(&task_operation_id);
                            }
                            Err(error) => {
                                tracing::error!(operation_id = %task_operation_id, %error, "prepared promotion resume failed");
                            }
                        }
                    })
                    .await
                {
                    return Err(OperationStartupError::AdmissionClosed);
                }
                Ok(())
            }
            StartupRecoveryAction::TerminalizeInterrupted => {
                let Some(log) = log else {
                    return Err(OperationStartupError::MissingUsableLog);
                };
                let terminal = startup_terminal(&observed.document, StartupTerminal::Interrupted)?;
                let completed_at = terminal_completed_at(&terminal)?;
                log.append(
                    completed_at,
                    OperationEvidence::Terminal {
                        operation: Box::new(terminal.clone()),
                    },
                )
                .await?;
                replace_terminal_exact(self.startup_rows.as_ref(), &observed, &terminal).await?;
                Ok(())
            }
            StartupRecoveryAction::QuarantineAndTerminalizeEvidenceLoss { reason } => {
                self.evidence.quarantine(&observed.id).await?;
                let completed_at = now()?;
                let log = self.evidence.create(identity, completed_at).await?;
                let terminal = startup_terminal_at(
                    &observed.document,
                    StartupTerminal::EvidenceLost(reason),
                    completed_at,
                )?;
                log.append(
                    completed_at,
                    OperationEvidence::Terminal {
                        operation: Box::new(terminal.clone()),
                    },
                )
                .await?;
                replace_terminal_exact(self.startup_rows.as_ref(), &observed, &terminal).await?;
                Ok(())
            }
        }
    }

    pub(super) async fn shutdown(&self) -> OperationShutdownOutcome {
        self.tasks.shutdown(OPERATION_SHUTDOWN_GRACE).await
    }
}

async fn replace_terminal_exact(
    rows: &dyn StartupOperationRows,
    observed: &ObservedOperation,
    terminal: &OperationDocument,
) -> Result<(), OperationStartupError> {
    match rows
        .replace_terminal(observed, terminal)
        .await
        .map_err(OperationStartupError::Rows)?
    {
        ConditionalOperationWrite::Written => Ok(()),
        ConditionalOperationWrite::Stale => {
            let current = rows
                .operation(&observed.id)
                .await
                .map_err(OperationStartupError::Rows)?;
            match current {
                Some(current) if current.document == *terminal => Ok(()),
                _ => Err(OperationStartupError::ConflictingTerminalWrite {
                    operation_id: observed.id.clone(),
                }),
            }
        }
    }
}

pub(super) async fn handle_lookup(
    service: &ApiService,
    operation_id: OperationRowId,
) -> Response<HttpBody> {
    match service.operations.store.operation(&operation_id).await {
        Ok(Some(observed)) => encode_json(
            StatusCode::OK,
            &OperationLookupReply {
                operation_id: observed.id,
                operation: observed.document,
            },
        ),
        Ok(None) => encode_json(
            StatusCode::NOT_FOUND,
            &OperationLookupRefusal::NotFound { operation_id },
        ),
        Err(error) => {
            tracing::warn!(%error, "operation lookup failed");
            corrosion_unavailable_response()
        }
    }
}

pub(super) async fn handle_deploy(
    service: &ApiService,
    principal: Principal,
    request: hyper::Request<hyper::body::Incoming>,
) -> Response<HttpBody> {
    let Some(driver) = service.deploy.as_ref() else {
        return super::server::refusal_response(ployz_core::ApiRefusal::UnsupportedRoute);
    };
    let request: DeployRequest = match super::mutations::decode_request(request.into_body()).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    match driver.admit(request, principal).await {
        Ok(Ok(accepted)) => match service.operations.spawn_deploy(accepted).await {
            Ok(reply) => super::mutations::typed_response(StatusCode::ACCEPTED, &reply),
            Err(error) => {
                tracing::error!(%error, "deploy task admission failed after durable acceptance");
                corrosion_unavailable_response()
            }
        },
        Ok(Err(refusal)) => {
            let status = match refusal {
                DeployRefusal::NamespaceNotFound { .. }
                | DeployRefusal::UnknownPinnedMachine { .. } => StatusCode::NOT_FOUND,
                DeployRefusal::NamespaceAmbiguous { .. }
                | DeployRefusal::DifferentService { .. }
                | DeployRefusal::MultipleServices { .. }
                | DeployRefusal::RoutesWithoutServices { .. }
                | DeployRefusal::Placement { .. }
                | DeployRefusal::ReplicasOnGlobalService => StatusCode::CONFLICT,
                DeployRefusal::BridgeUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            };
            super::mutations::typed_response(status, &refusal)
        }
        Err(error) => {
            tracing::error!(%error, "deploy admission failed");
            corrosion_unavailable_response()
        }
    }
}

pub(super) async fn handle_watch(
    service: &ApiService,
    operation_id: OperationRowId,
    principal: &Principal,
    shutdown: watch::Receiver<bool>,
) -> Response<HttpBody> {
    let observed = match service.operations.store.operation(&operation_id).await {
        Ok(Some(observed)) => observed,
        Ok(None) => {
            return watch_refusal_response(OperationWatchRefusal::NotFound { operation_id });
        }
        Err(error) => {
            tracing::warn!(%error, "operation watch lookup failed");
            return corrosion_unavailable_response();
        }
    };
    let operation = OperationLookupReply {
        operation_id: observed.id.clone(),
        operation: observed.document.clone(),
    };
    let machines = match accepted_machines(service).await {
        Ok(machines) => machines,
        Err(response) => return response,
    };
    let roster = machines
        .iter()
        .map(|row| row.id.clone())
        .collect::<BTreeSet<_>>();
    let ingress = match principal {
        Principal::Machine { .. } => OperationWatchIngress::AuthenticatedMachineHop,
        Principal::Peer { .. } | Principal::ApiToken { .. } => OperationWatchIngress::External,
    };
    let owner = resolve_operation_watch_owner(
        &service.local_machine_id,
        &observed.document.machine_id,
        &roster,
        ingress,
    );
    if let Some(refusal) = owner_routing_refusal(owner.clone(), operation.clone()) {
        return watch_refusal_response(refusal);
    }
    match owner {
        OperationWatchOwner::Local => local_watch(service, operation, shutdown).await,
        OperationWatchOwner::Remote { machine_id } => {
            let Some(machine) = machines.into_iter().find(|row| row.id == machine_id) else {
                return watch_refusal_response(OperationWatchRefusal::OwnerNoLongerRostered {
                    operation,
                    observation: HandshakeObservationOutcome::Unavailable {
                        reason: HandshakeObservationUnavailable::OwnerNotRostered,
                    },
                });
            };
            remote_watch(service, operation, machine.document.transport, shutdown).await
        }
        OperationWatchOwner::OwnerNoLongerRostered | OperationWatchOwner::ProxyLoop => {
            unreachable!("owner refusals returned above")
        }
    }
}

async fn local_watch(
    service: &ApiService,
    operation: OperationLookupReply,
    shutdown: watch::Receiver<bool>,
) -> Response<HttpBody> {
    if let Some(log) = service.operations.live_log(&operation.operation_id).await {
        return match log.follow().await {
            Ok(follower) => {
                operation_sse_response(local_follow_body(follower, operation, shutdown))
            }
            Err(error) => {
                tracing::warn!(%error, "could not attach to live operation evidence");
                watch_refusal_response(OperationWatchRefusal::DetailUnavailable { operation })
            }
        };
    }
    if !operation_is_terminal(&operation.operation) {
        return watch_refusal_response(OperationWatchRefusal::DetailUnavailable { operation });
    }
    let identity = EvidenceIdentity::new(
        operation.operation_id.clone(),
        service.local_machine_id.clone(),
    );
    let events = match service.operations.evidence.replay_closed(&identity).await {
        Ok(events) => events,
        Err(error) => {
            tracing::warn!(%error, "closed operation detail unavailable");
            return watch_refusal_response(OperationWatchRefusal::DetailUnavailable { operation });
        }
    };
    let exact_terminal = events.last().is_some_and(|event| {
        matches!(
            &event.evidence,
            OperationEvidence::Terminal { operation: terminal }
                if **terminal == operation.operation
        )
    });
    if !exact_terminal {
        return watch_refusal_response(OperationWatchRefusal::DetailUnavailable { operation });
    }
    operation_sse_response(replay_body(events))
}

async fn accepted_machines(
    service: &ApiService,
) -> Result<Vec<ployz_core::MachineLensRow>, Response<HttpBody>> {
    let Some(lenses) = service.lenses() else {
        return Err(corrosion_unavailable_response());
    };
    let mut updates = lenses.watch(LensCollection::Machines).subscribe();
    match tokio::time::timeout(OPERATION_LENS_WAIT, await_lens_state(&mut updates)).await {
        Ok(Ok(Ok(LensSnapshot::Machines { rows, .. }))) => Ok(rows),
        Ok(Ok(Ok(
            LensSnapshot::Services { .. }
            | LensSnapshot::Containers { .. }
            | LensSnapshot::MachineStatus { .. }
            | LensSnapshot::Operations { .. },
        ))) => Err(corrosion_unavailable_response()),
        Ok(Ok(Err(_))) | Ok(Err(_)) | Err(_) => Err(corrosion_unavailable_response()),
    }
}

async fn remote_watch(
    service: &ApiService,
    operation: OperationLookupReply,
    transport: MachineTransport,
    shutdown: watch::Receiver<bool>,
) -> Response<HttpBody> {
    let (ip, public_key) = match transport {
        MachineTransport::Wireguard {
            pubkey, addr_v6, ..
        } => (IpAddr::V6(addr_v6), Some(pubkey)),
        MachineTransport::Tailscale { ip, .. } => (IpAddr::V4(ip), None),
    };
    let target = SocketAddr::new(ip, service.listen_addr.port());
    let url = format!(
        "http://{target}{}",
        operation_watch_route(&operation.operation_id)
    );
    let sent = tokio::time::timeout(
        MACHINE_HOP_HEADER_TIMEOUT,
        service
            .operations
            .machine_hop
            .get(url)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send(),
    )
    .await;
    let response = match sent {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            tracing::warn!(%error, %target, "operation owner transport failed");
            return driver_dark_response(service, operation, public_key, shutdown).await;
        }
        Err(_) => {
            tracing::warn!(%target, "operation owner header deadline elapsed");
            return driver_dark_response(service, operation, public_key, shutdown).await;
        }
    };
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .cloned();
    let is_sse = status.is_success()
        && content_type
            .as_ref()
            .and_then(|value| value.to_str().ok())
            .is_some_and(is_event_stream_content_type);
    if !is_sse {
        let bytes = match bounded_owner_body(response).await {
            Ok(bytes) => bytes,
            Err(()) => return protocol_owner_response(status),
        };
        let mut proxied = Response::new(Full::new(bytes).boxed());
        *proxied.status_mut() = status;
        if let Some(value) =
            content_type.and_then(|value| HeaderValue::from_bytes(value.as_bytes()).ok())
        {
            proxied.headers_mut().insert(CONTENT_TYPE, value);
        }
        return proxied;
    }
    operation_sse_response(remote_follow_body(
        Box::pin(response.bytes_stream()),
        service.operations.keeper_control_socket_path.clone(),
        operation,
        public_key,
        shutdown,
    ))
}

async fn driver_dark_response(
    service: &ApiService,
    operation: OperationLookupReply,
    public_key: Option<ployz_core::network::WireGuardPublicKey>,
    shutdown: watch::Receiver<bool>,
) -> Response<HttpBody> {
    let observation = dark_observation(
        &service.operations.keeper_control_socket_path,
        public_key,
        shutdown,
    )
    .await;
    watch_refusal_response(OperationWatchRefusal::DriverDark {
        operation,
        observation,
    })
}

async fn dark_observation(
    keeper_socket: &std::path::Path,
    public_key: Option<ployz_core::network::WireGuardPublicKey>,
    shutdown: watch::Receiver<bool>,
) -> HandshakeObservationOutcome {
    match public_key {
        Some(public_key) => {
            crate::roles::api::keeper_control::observe_handshake(
                keeper_socket,
                public_key,
                shutdown,
            )
            .await
        }
        None => HandshakeObservationOutcome::Unavailable {
            reason: HandshakeObservationUnavailable::UnsupportedProvider,
        },
    }
}

async fn bounded_owner_body(response: reqwest::Response) -> Result<Bytes, ()> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = tokio::time::timeout(MACHINE_HOP_IDLE_TIMEOUT, stream.next())
        .await
        .map_err(|_| ())?
    {
        let chunk = chunk.map_err(|_| ())?;
        let Some(total) = bytes.len().checked_add(chunk.len()) else {
            return Err(());
        };
        if total > MAX_OWNER_RESPONSE_BYTES {
            return Err(());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(bytes))
}

fn is_event_stream_content_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("text/event-stream"))
}

fn protocol_owner_response(status: StatusCode) -> Response<HttpBody> {
    let mut response = Response::new(Full::new(Bytes::new()).boxed());
    *response.status_mut() = status;
    response
}

fn replay_body(events: Vec<ployz_core::OperationEvidenceEvent>) -> HttpBody {
    let stream = stream::iter(events.into_iter().map(|event| {
        Ok::<_, Infallible>(Frame::data(encoded_operation_event(
            &OperationWatchEvent::Evidence { event },
        )))
    }));
    BodyExt::boxed(StreamBody::new(stream))
}

struct LocalFollowState {
    follower: OperationEvidenceFollower,
    operation: OperationLookupReply,
    shutdown: watch::Receiver<bool>,
    keepalive: tokio::time::Interval,
    done: bool,
}

fn local_follow_body(
    follower: OperationEvidenceFollower,
    operation: OperationLookupReply,
    shutdown: watch::Receiver<bool>,
) -> HttpBody {
    let mut keepalive = tokio::time::interval_at(
        Instant::now() + OPERATION_KEEPALIVE_INTERVAL,
        OPERATION_KEEPALIVE_INTERVAL,
    );
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let stream = stream::unfold(
        LocalFollowState {
            follower,
            operation,
            shutdown,
            keepalive,
            done: false,
        },
        |mut state| async move {
            if state.done {
                return None;
            }
            tokio::select! {
                biased;
                result = state.follower.next() => match result {
                    Ok(Some(event)) => Some((
                        Ok::<_, Infallible>(Frame::data(encoded_operation_event(
                            &OperationWatchEvent::Evidence { event },
                        ))),
                        state,
                    )),
                    Ok(None) => None,
                    Err(error) => {
                        tracing::warn!(%error, "operation evidence follow failed");
                        state.done = true;
                        let terminal = terminal_watch_event(OperationWatchRefusal::DetailUnavailable {
                            operation: state.operation.clone(),
                        });
                        Some((Ok(Frame::data(encoded_operation_event(&terminal))), state))
                    }
                },
                changed = state.shutdown.changed() => match changed {
                    Ok(()) if *state.shutdown.borrow() => None,
                    Ok(()) => Some((Ok(Frame::data(sse_keepalive())), state)),
                    Err(_) => None,
                },
                _ = state.keepalive.tick() => {
                    Some((Ok(Frame::data(sse_keepalive())), state))
                }
            }
        },
    );
    BodyExt::boxed(StreamBody::new(stream))
}

fn remote_follow_body(
    stream: RemoteByteStream,
    keeper_socket: PathBuf,
    operation: OperationLookupReply,
    public_key: Option<ployz_core::network::WireGuardPublicKey>,
    shutdown: watch::Receiver<bool>,
) -> HttpBody {
    let (frames, receiver) = mpsc::channel(8);
    tokio::spawn(relay_remote_frames(
        stream,
        frames,
        keeper_socket,
        operation,
        public_key,
        shutdown,
    ));
    let stream = stream::unfold(receiver, |mut receiver| async move {
        receiver
            .recv()
            .await
            .map(|bytes| (Ok::<_, Infallible>(Frame::data(bytes)), receiver))
    });
    BodyExt::boxed(StreamBody::new(stream))
}

async fn relay_remote_frames(
    mut stream: RemoteByteStream,
    frames: mpsc::Sender<Bytes>,
    keeper_socket: PathBuf,
    operation: OperationLookupReply,
    public_key: Option<ployz_core::network::WireGuardPublicKey>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut pending = Vec::new();
    loop {
        let next = tokio::select! {
            biased;
            changed = shutdown.changed() => match changed {
                Ok(()) if *shutdown.borrow() => return,
                Ok(()) => continue,
                Err(_) => return,
            },
            next = tokio::time::timeout(MACHINE_HOP_IDLE_TIMEOUT, stream.next()) => next,
        };
        let chunk = match next {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(None) => {
                if pending.is_empty() {
                    tracing::warn!("operation owner stream ended before a terminal SSE frame");
                } else {
                    tracing::warn!("operation owner stream ended with an incomplete SSE frame");
                }
                break;
            }
            Ok(Some(Err(error))) => {
                tracing::warn!(%error, "operation owner stream transport failed");
                break;
            }
            Err(_) => {
                tracing::warn!("operation owner stream idle deadline elapsed");
                break;
            }
        };
        pending.extend_from_slice(&chunk);
        while let Some(frame_end) = complete_sse_frame_end(&pending) {
            if frame_end > MAX_OWNER_SSE_FRAME_BYTES {
                tracing::warn!(
                    frame_bytes = frame_end,
                    "operation owner SSE frame exceeded bound"
                );
                send_remote_terminal(&frames, &keeper_socket, &operation, public_key, shutdown)
                    .await;
                return;
            }
            let remainder = pending.split_off(frame_end);
            let frame = std::mem::replace(&mut pending, remainder);
            let terminal = is_terminal_operation_frame(&frame);
            if frames.send(Bytes::from(frame)).await.is_err() {
                return;
            }
            if terminal {
                return;
            }
        }
        if pending.len() > MAX_OWNER_SSE_FRAME_BYTES {
            tracing::warn!(
                frame_bytes = pending.len(),
                "operation owner SSE frame exceeded bound"
            );
            break;
        }
    }
    send_remote_terminal(&frames, &keeper_socket, &operation, public_key, shutdown).await;
}

fn is_terminal_operation_frame(frame: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(frame) else {
        return false;
    };
    let mut data = String::new();
    for line in text.lines() {
        let Some(value) = line.strip_prefix("data:") else {
            continue;
        };
        if !data.is_empty() {
            data.push('\n');
        }
        data.push_str(value.strip_prefix(' ').unwrap_or(value));
    }
    let Ok(event) = serde_json::from_str::<OperationWatchEvent>(&data) else {
        return false;
    };
    matches!(
        event,
        OperationWatchEvent::Terminal { .. }
            | OperationWatchEvent::Evidence {
                event: ployz_core::OperationEvidenceEvent {
                    evidence: OperationEvidence::Terminal { .. },
                    ..
                }
            }
    )
}

fn complete_sse_frame_end(bytes: &[u8]) -> Option<usize> {
    let mut line_start = 0;
    let mut cursor = 0;
    while cursor < bytes.len() {
        let byte = *bytes.get(cursor)?;
        let terminator_end = match byte {
            b'\n' => cursor + 1,
            b'\r' => match bytes.get(cursor + 1) {
                None => return None,
                Some(b'\n') => cursor + 2,
                Some(_) => cursor + 1,
            },
            _ => {
                cursor += 1;
                continue;
            }
        };
        if cursor == line_start {
            return Some(terminator_end);
        }
        line_start = terminator_end;
        cursor = terminator_end;
    }
    None
}

async fn send_remote_terminal(
    frames: &mpsc::Sender<Bytes>,
    keeper_socket: &std::path::Path,
    operation: &OperationLookupReply,
    public_key: Option<ployz_core::network::WireGuardPublicKey>,
    shutdown: watch::Receiver<bool>,
) {
    let observation = dark_observation(keeper_socket, public_key, shutdown).await;
    let terminal = terminal_watch_event(OperationWatchRefusal::DriverDark {
        operation: operation.clone(),
        observation,
    });
    let _ = frames.send(encoded_operation_event(&terminal)).await;
}

fn operation_sse_response(body: HttpBody) -> Response<HttpBody> {
    let mut response = sse_response(body);
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn terminal_watch_event(refusal: OperationWatchRefusal) -> OperationWatchEvent {
    OperationWatchEvent::Terminal {
        refusal: Box::new(refusal),
    }
}

fn encoded_operation_event(event: &OperationWatchEvent) -> Bytes {
    match serde_json::to_vec(event) {
        Ok(json) => {
            let data = sse_data(&json);
            let mut frame = Vec::with_capacity(event.event_name().len() + data.len() + 8);
            frame.extend_from_slice(b"event: ");
            frame.extend_from_slice(event.event_name().as_bytes());
            frame.push(b'\n');
            frame.extend_from_slice(&data);
            Bytes::from(frame)
        }
        Err(error) => {
            tracing::error!(%error, "could not encode operation SSE event");
            Bytes::new()
        }
    }
}

fn watch_refusal_response(refusal: OperationWatchRefusal) -> Response<HttpBody> {
    let status = match &refusal {
        OperationWatchRefusal::NotFound { .. } => StatusCode::NOT_FOUND,
        OperationWatchRefusal::OwnerNoLongerRostered { .. }
        | OperationWatchRefusal::DriverDark { .. }
        | OperationWatchRefusal::DetailUnavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
        OperationWatchRefusal::ProxyLoop => StatusCode::LOOP_DETECTED,
    };
    encode_json(status, &refusal)
}

fn encode_json<Value: serde::Serialize>(status: StatusCode, value: &Value) -> Response<HttpBody> {
    match serde_json::to_vec(value) {
        Ok(body) => json_response(status, body),
        Err(error) => {
            tracing::error!(%error, "could not encode operation response");
            corrosion_unavailable_response()
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum StartupTerminal {
    Interrupted,
    EvidenceLost(CorrosionEvidenceLossReason),
}

fn startup_terminal(
    operation: &OperationDocument,
    failure: StartupTerminal,
) -> Result<OperationDocument, OperationStartupError> {
    startup_terminal_at(operation, failure, now()?)
}

fn startup_terminal_at(
    operation: &OperationDocument,
    failure: StartupTerminal,
    completed_at: CorrosionTimestamp,
) -> Result<OperationDocument, OperationStartupError> {
    match operation.operation() {
        CorrosionOperation::Deploy { service_ids, .. } => {
            let results = service_ids
                .as_slice()
                .iter()
                .cloned()
                .map(CorrosionDeployServiceResult::skipped)
                .collect();
            let failure = match failure {
                StartupTerminal::Interrupted => CorrosionDeployFailure::Interrupted,
                StartupTerminal::EvidenceLost(reason) => {
                    CorrosionDeployFailure::EvidenceLost { reason }
                }
            };
            let outcome = CorrosionDeployOutcome::failed(results, failure)
                .map_err(|error| OperationStartupError::Transition(error.to_string()))?;
            operation
                .clone()
                .transition_deploy(CorrosionDeployTransition::Terminal {
                    completed_at,
                    outcome,
                })
                .map_err(|error| OperationStartupError::Transition(error.to_string()))
        }
        CorrosionOperation::Build { .. }
        | CorrosionOperation::MachineAdd { .. }
        | CorrosionOperation::MachineRemove { .. }
        | CorrosionOperation::Recovery { .. } => {
            let message = match failure {
                StartupTerminal::Interrupted => "operation interrupted by API restart".to_owned(),
                StartupTerminal::EvidenceLost(reason) => {
                    format!("operation evidence unavailable after API restart: {reason:?}")
                }
            };
            operation
                .clone()
                .transition_basic(CorrosionBasicTransition::Failed {
                    completed_at,
                    failure: CorrosionOperationFailure::Interrupted { message },
                })
                .map_err(|error| OperationStartupError::Transition(error.to_string()))
        }
    }
}

fn terminal_completed_at(
    operation: &OperationDocument,
) -> Result<CorrosionTimestamp, OperationStartupError> {
    match operation.operation() {
        CorrosionOperation::Deploy {
            state: ployz_core::corrosion::CorrosionDeployState::Terminal { completed_at, .. },
            ..
        }
        | CorrosionOperation::Build {
            state:
                ployz_core::corrosion::CorrosionOperationState::Succeeded { completed_at, .. }
                | ployz_core::corrosion::CorrosionOperationState::Failed { completed_at, .. },
            ..
        }
        | CorrosionOperation::MachineAdd {
            state:
                ployz_core::corrosion::CorrosionOperationState::Succeeded { completed_at, .. }
                | ployz_core::corrosion::CorrosionOperationState::Failed { completed_at, .. },
            ..
        }
        | CorrosionOperation::MachineRemove {
            state:
                ployz_core::corrosion::CorrosionOperationState::Succeeded { completed_at, .. }
                | ployz_core::corrosion::CorrosionOperationState::Failed { completed_at, .. },
            ..
        }
        | CorrosionOperation::Recovery {
            state:
                ployz_core::corrosion::CorrosionOperationState::Succeeded { completed_at, .. }
                | ployz_core::corrosion::CorrosionOperationState::Failed { completed_at, .. },
            ..
        } => Ok(*completed_at),
        CorrosionOperation::Deploy { .. }
        | CorrosionOperation::Build { .. }
        | CorrosionOperation::MachineAdd { .. }
        | CorrosionOperation::MachineRemove { .. }
        | CorrosionOperation::Recovery { .. } => Err(OperationStartupError::Transition(
            "startup terminal document was not terminal".to_owned(),
        )),
    }
}

fn now() -> Result<CorrosionTimestamp, OperationStartupError> {
    let value = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| OperationStartupError::Clock(error.to_string()))?;
    CorrosionTimestamp::try_new(value)
        .map_err(|error| OperationStartupError::Clock(error.to_string()))
}

fn evidence_loss_reason(error: &OperationEvidenceError) -> CorrosionEvidenceLossReason {
    match error {
        OperationEvidenceError::Missing => CorrosionEvidenceLossReason::MissingFile,
        OperationEvidenceError::IdentityMismatch { .. } => {
            CorrosionEvidenceLossReason::IdentityMismatch
        }
        OperationEvidenceError::TornTail
        | OperationEvidenceError::MissingIdentity
        | OperationEvidenceError::MissingCreated
        | OperationEvidenceError::UnsupportedVersion { .. }
        | OperationEvidenceError::Malformed { .. }
        | OperationEvidenceError::LineTooLarge { .. }
        | OperationEvidenceError::FileTooLarge { .. }
        | OperationEvidenceError::RepeatedIdentity { .. }
        | OperationEvidenceError::RepeatedPreparedPromotion { .. }
        | OperationEvidenceError::PreparedPayloadRequired
        | OperationEvidenceError::InvalidPreparedEvent { .. }
        | OperationEvidenceError::InvalidPreparedIntent { .. }
        | OperationEvidenceError::TerminalDocumentNonterminal
        | OperationEvidenceError::TerminalDriverMismatch
        | OperationEvidenceError::InvalidEventOrder { .. }
        | OperationEvidenceError::NonContiguousSequence { .. }
        | OperationEvidenceError::SequenceExhausted
        | OperationEvidenceError::DurableOffsetChanged
        | OperationEvidenceError::Encode(_)
        | OperationEvidenceError::Create { .. }
        | OperationEvidenceError::Io(_) => CorrosionEvidenceLossReason::MalformedFile,
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum OperationStartupError {
    #[error("operation startup row access failed: {0}")]
    Rows(String),
    #[error(transparent)]
    Evidence(#[from] OperationEvidenceError),
    #[error("operation startup transition failed: {0}")]
    Transition(String),
    #[error("operation startup clock failed: {0}")]
    Clock(String),
    #[error("startup recovery required a usable evidence log")]
    MissingUsableLog,
    #[error("startup recovery requires the prepared-promotion resumer")]
    MissingPromotionResumer,
    #[error("operation task admission closed during startup")]
    AdmissionClosed,
    #[error("operation {operation_id} changed during startup terminal reconciliation")]
    ConflictingTerminalWrite { operation_id: OperationRowId },
}

#[derive(Debug, thiserror::Error)]
pub(super) enum DeployAdmissionError {
    #[error("operation task admission closed during deploy")]
    AdmissionClosed,
    #[error("rejected deploy task was lost before terminalization")]
    LostRejectedTask,
    #[error("rejected deploy could not be terminalized: {0}")]
    Interrupt(DeployDriverError),
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use bytes::Bytes;
    use futures_util::stream;
    use ployz_core::corrosion::{
        CorrosionDeployTargets, CorrosionDocumentVersion, CorrosionTimestamp, OperationDocument,
        Principal,
    };
    use ployz_core::ids::{
        ClusterId, MachineRowId, NamespaceRowId, OperationRowId, PeerId, ServiceRowId,
    };
    use ployz_core::{
        OperationEvidence, OperationEvidenceEvent, OperationEvidenceSequence, OperationLookupReply,
        OperationWatchEvent, OperationWatchRefusal,
    };
    use tokio::sync::{Mutex, mpsc, watch};

    use super::{
        ConditionalOperationWrite, ObservedOperation, OperationStartupError, RemoteByteStream,
        StartupOperationRows, StartupTerminal, encoded_operation_event,
        is_terminal_operation_frame, relay_remote_frames, replace_terminal_exact, startup_terminal,
        terminal_watch_event,
    };

    fn operation_id() -> OperationRowId {
        OperationRowId::try_new("01J00000000000000000000011").expect("operation id")
    }

    fn timestamp() -> CorrosionTimestamp {
        CorrosionTimestamp::try_new("2026-08-05T00:00:00Z").expect("timestamp")
    }

    fn created_operation() -> OperationDocument {
        OperationDocument::deploy_created(
            CorrosionDocumentVersion::V1,
            ClusterId::try_new("01J00000000000000000000012").expect("cluster id"),
            MachineRowId::try_new("01J00000000000000000000013").expect("machine id"),
            Principal::Peer {
                peer_id: PeerId::try_new("01J00000000000000000000014").expect("peer id"),
            },
            NamespaceRowId::try_new("01J00000000000000000000015").expect("namespace id"),
            CorrosionDeployTargets::try_new(vec![
                ServiceRowId::try_new("01J00000000000000000000016").expect("service id"),
            ])
            .expect("deploy targets"),
            timestamp(),
        )
    }

    fn lookup() -> OperationLookupReply {
        OperationLookupReply {
            operation_id: operation_id(),
            operation: created_operation(),
        }
    }

    fn evidence_frame(evidence: OperationEvidence) -> Bytes {
        encoded_operation_event(&OperationWatchEvent::Evidence {
            event: OperationEvidenceEvent {
                sequence: OperationEvidenceSequence::try_new(1).expect("sequence"),
                timestamp: timestamp(),
                machine: None,
                evidence,
            },
        })
    }

    fn observed(document: OperationDocument) -> ObservedOperation {
        ObservedOperation {
            id: operation_id(),
            exact_document: serde_json::to_string(&document).expect("operation json"),
            document,
        }
    }

    struct StartupRows {
        current: Mutex<Option<OperationDocument>>,
        replacement: ConditionalOperationWrite,
    }

    #[async_trait]
    impl StartupOperationRows for StartupRows {
        async fn operation(
            &self,
            _operation_id: &OperationRowId,
        ) -> Result<Option<ObservedOperation>, String> {
            Ok(self.current.lock().await.clone().map(observed))
        }

        async fn local_nonterminal_operations(
            &self,
            _machine_id: &MachineRowId,
        ) -> Result<Vec<ObservedOperation>, String> {
            Ok(Vec::new())
        }

        async fn replace_terminal(
            &self,
            _observed: &ObservedOperation,
            _terminal: &OperationDocument,
        ) -> Result<ConditionalOperationWrite, String> {
            Ok(self.replacement)
        }
    }

    async fn relayed(chunks: Vec<Bytes>) -> Vec<Bytes> {
        let stream: RemoteByteStream = Box::pin(stream::iter(chunks.into_iter().map(Ok)));
        let (sender, mut receiver) = mpsc::channel(8);
        let (_shutdown, shutdown) = watch::channel(false);
        relay_remote_frames(
            stream,
            sender,
            std::path::PathBuf::from("/unused-for-tailscale"),
            lookup(),
            None,
            shutdown,
        )
        .await;
        let mut frames = Vec::new();
        while let Some(frame) = receiver.recv().await {
            frames.push(frame);
        }
        frames
    }

    #[tokio::test]
    async fn clean_eof_before_terminal_emits_driver_dark() {
        let progress = evidence_frame(OperationEvidence::Created);
        let frames = relayed(vec![progress.clone()]).await;
        let [progress_frame, terminal_frame] = frames.as_slice() else {
            panic!("expected progress and terminal frames");
        };
        assert_eq!(progress_frame, &progress);
        assert!(is_terminal_operation_frame(terminal_frame));
    }

    #[tokio::test]
    async fn partial_frame_drop_is_not_forwarded_before_driver_dark() {
        let frames = relayed(vec![Bytes::from_static(b"data: {\"kind\":\"evidence\"")]).await;
        let [terminal_frame] = frames.as_slice() else {
            panic!("expected one terminal frame");
        };
        assert!(is_terminal_operation_frame(terminal_frame));
    }

    #[tokio::test]
    async fn terminal_evidence_ends_the_relay_without_synthesized_failure() {
        let terminal = startup_terminal(&created_operation(), StartupTerminal::Interrupted)
            .expect("terminal operation");
        let terminal = evidence_frame(OperationEvidence::Terminal {
            operation: Box::new(terminal),
        });
        let frames = relayed(vec![terminal.clone()]).await;
        assert_eq!(frames, vec![terminal]);
    }

    #[tokio::test]
    async fn terminal_refusal_ends_the_relay_without_synthesized_failure() {
        let terminal = encoded_operation_event(&terminal_watch_event(
            OperationWatchRefusal::DetailUnavailable {
                operation: lookup(),
            },
        ));
        let frames = relayed(vec![terminal.clone()]).await;
        assert_eq!(frames, vec![terminal]);
    }

    #[tokio::test]
    async fn stale_startup_write_accepts_only_the_exact_terminal() {
        let created = created_operation();
        let terminal =
            startup_terminal(&created, StartupTerminal::Interrupted).expect("terminal operation");
        let rows = StartupRows {
            current: Mutex::new(Some(terminal.clone())),
            replacement: ConditionalOperationWrite::Stale,
        };

        replace_terminal_exact(&rows, &observed(created), &terminal)
            .await
            .expect("exact terminal won the race");
    }

    #[tokio::test]
    async fn stale_startup_write_rejects_conflicting_state() {
        let created = created_operation();
        let terminal =
            startup_terminal(&created, StartupTerminal::Interrupted).expect("terminal operation");
        let rows = StartupRows {
            current: Mutex::new(Some(created.clone())),
            replacement: ConditionalOperationWrite::Stale,
        };

        let error = replace_terminal_exact(&rows, &observed(created), &terminal)
            .await
            .expect_err("conflicting row must stop startup recovery");
        assert!(matches!(
            error,
            OperationStartupError::ConflictingTerminalWrite { .. }
        ));
    }
}
