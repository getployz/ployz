use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufStream};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::StoreDriver;
use crate::error::{Error, Result};
use crate::model::{
    DeployId, InstanceId, InstancePhase, InstanceStatusRecord, MachineId, MachineRecord, OverlayIp,
    SlotId,
};
use crate::spec::{Namespace, ServiceSpec};
use crate::store::DeployStore;
use crate::transport::{RemoteDeployRequest, RemoteDeployResponse};

use super::{
    DrainState, LocalDeployRuntime, NamespaceLock, NamespaceLockManager, adopt_instances,
    build_instance_status_record, list_local_instance_status,
};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

pub struct RemoteControlHandle {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl RemoteControlHandle {
    #[must_use]
    pub fn noop() -> Self {
        Self {
            cancel: CancellationToken::new(),
            task: tokio::spawn(async {}),
        }
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.task.await;
    }
}

#[derive(Clone)]
struct RemoteDeployService {
    store: StoreDriver,
    namespace_locks: NamespaceLockManager,
    local_machine_id: MachineId,
    overlay_network_name: Option<String>,
}

impl RemoteDeployService {
    fn new(
        store: StoreDriver,
        namespace_locks: NamespaceLockManager,
        local_machine_id: MachineId,
        overlay_network_name: Option<String>,
    ) -> Self {
        Self {
            store,
            namespace_locks,
            local_machine_id,
            overlay_network_name,
        }
    }

    async fn handle_request(
        &self,
        request: RemoteDeployRequest,
        held_locks: &mut HashMap<String, NamespaceLock>,
    ) -> RemoteDeployResponse {
        match self.handle_request_inner(request, held_locks).await {
            Ok(response) => response,
            Err(err) => RemoteDeployResponse::Error {
                code: "REMOTE_DEPLOY_FAILED".into(),
                message: err.to_string(),
            },
        }
    }

    async fn handle_request_inner(
        &self,
        request: RemoteDeployRequest,
        held_locks: &mut HashMap<String, NamespaceLock>,
    ) -> Result<RemoteDeployResponse> {
        match request {
            RemoteDeployRequest::LockAcquire {
                namespace,
                deploy_id,
                coordinator_id,
                participant_fingerprint: _participant_fingerprint,
            } => {
                let namespace = Namespace(namespace);
                let deploy_id = DeployId(deploy_id);
                let lock = self.namespace_locks.try_acquire(&namespace, &deploy_id)?;
                held_locks.insert(namespace.0.clone(), lock);
                Ok(RemoteDeployResponse::Ack {
                    message: format!(
                        "acquired namespace lock for '{}' from coordinator '{}'",
                        namespace, coordinator_id
                    ),
                })
            }
            RemoteDeployRequest::LockHeartbeat {
                namespace,
                deploy_id,
            } => Ok(RemoteDeployResponse::Ack {
                message: format!("heartbeat for namespace '{namespace}' deploy '{deploy_id}'"),
            }),
            RemoteDeployRequest::InspectNamespace { namespace } => {
                let namespace = Namespace(namespace);
                if let Ok(runtime) = self.new_runtime() {
                    adopt_instances(&self.store, &runtime, &namespace).await?;
                }
                let instances =
                    list_local_instance_status(&self.store, &namespace, &self.local_machine_id)
                        .await?;
                Ok(RemoteDeployResponse::NamespaceSnapshot { instances })
            }
            RemoteDeployRequest::StartCandidate {
                namespace,
                service,
                slot_id,
                instance_id,
                deploy_id,
                spec_json,
            } => {
                let namespace = Namespace(namespace);
                let deploy_id = DeployId(deploy_id);
                let slot_id = SlotId(slot_id);
                let instance_id = InstanceId(instance_id);
                let spec: ServiceSpec = serde_json::from_str(&spec_json).map_err(|e| {
                    Error::operation("remote_start_candidate", format!("decode spec: {e}"))
                })?;
                if spec.namespace != namespace {
                    return Err(Error::operation(
                        "remote_start_candidate",
                        format!(
                            "spec namespace '{}' did not match request namespace '{}'",
                            spec.namespace, namespace
                        ),
                    ));
                }
                if spec.name != service {
                    return Err(Error::operation(
                        "remote_start_candidate",
                        format!(
                            "spec service '{}' did not match request service '{}'",
                            spec.name, service
                        ),
                    ));
                }
                let revision_hash = spec
                    .revision_hash()
                    .map_err(|e| Error::operation("remote_start_candidate", e))?;
                let runtime = self.new_runtime()?;
                let instance = runtime
                    .start_candidate(
                        &spec,
                        &deploy_id,
                        &instance_id,
                        &slot_id,
                        &self.local_machine_id,
                        &revision_hash,
                    )
                    .await?;
                runtime.wait_ready(&spec, &instance).await?;
                let status = build_instance_status_record(
                    &namespace,
                    &service,
                    &slot_id,
                    &self.local_machine_id,
                    &revision_hash,
                    &deploy_id,
                    &instance,
                    InstancePhase::Ready,
                    true,
                    DrainState::None,
                    None,
                );
                self.store.upsert_instance_status(&status).await?;
                Ok(RemoteDeployResponse::CandidateStarted { status })
            }
            RemoteDeployRequest::DrainInstance {
                namespace,
                instance_id,
            } => {
                let namespace = Namespace(namespace);
                let instance_id = InstanceId(instance_id);
                let Some(mut status) = self
                    .find_local_instance_status(&namespace, &instance_id)
                    .await?
                else {
                    return Err(Error::operation(
                        "remote_drain_instance",
                        format!("instance '{}' was not found on this machine", instance_id),
                    ));
                };
                status.phase = InstancePhase::Draining;
                status.ready = false;
                status.drain_state = DrainState::Requested;
                status.updated_at = super::now_unix_secs();
                self.store.upsert_instance_status(&status).await?;
                Ok(RemoteDeployResponse::Ack {
                    message: format!("draining instance '{}'", status.instance_id),
                })
            }
            RemoteDeployRequest::RemoveInstance {
                namespace,
                instance_id,
            } => {
                let namespace = Namespace(namespace);
                let instance_id = InstanceId(instance_id);
                let Some(status) = self
                    .find_local_instance_status(&namespace, &instance_id)
                    .await?
                else {
                    return Err(Error::operation(
                        "remote_remove_instance",
                        format!("instance '{}' was not found on this machine", instance_id),
                    ));
                };
                let runtime = self.new_runtime()?;
                runtime
                    .remove_instance(&status.instance_id, &namespace, &status.service)
                    .await?;
                self.store
                    .delete_instance_status(&status.instance_id)
                    .await?;
                Ok(RemoteDeployResponse::Ack {
                    message: format!("removed instance '{}'", status.instance_id),
                })
            }
            RemoteDeployRequest::ApplyRouteEpoch {
                namespace,
                deploy_id,
            } => Ok(RemoteDeployResponse::Ack {
                message: format!(
                    "applied route epoch for namespace '{}' deploy '{}'",
                    namespace, deploy_id
                ),
            }),
            RemoteDeployRequest::Unlock {
                namespace,
                deploy_id,
            } => {
                held_locks.remove(&namespace);
                Ok(RemoteDeployResponse::Ack {
                    message: format!(
                        "released namespace lock for '{namespace}' deploy '{deploy_id}'"
                    ),
                })
            }
        }
    }

    fn new_runtime(&self) -> Result<LocalDeployRuntime> {
        LocalDeployRuntime::new(self.overlay_network_name.clone())
    }

    async fn find_local_instance_status(
        &self,
        namespace: &Namespace,
        instance_id: &InstanceId,
    ) -> Result<Option<InstanceStatusRecord>> {
        Ok(
            list_local_instance_status(&self.store, namespace, &self.local_machine_id)
                .await?
                .into_iter()
                .find(|record| record.instance_id == *instance_id),
        )
    }
}

pub async fn start_remote_control_listener(
    bind_ip: OverlayIp,
    port: u16,
    store: StoreDriver,
    namespace_locks: NamespaceLockManager,
    local_machine_id: MachineId,
    overlay_network_name: Option<String>,
) -> Result<RemoteControlHandle> {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V6(bind_ip.0), port))
        .await
        .map_err(|e| Error::operation("remote_control_listener", format!("bind: {e}")))?;
    let cancel = CancellationToken::new();
    let service = RemoteDeployService::new(
        store,
        namespace_locks,
        local_machine_id,
        overlay_network_name,
    );
    let listener_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = listener_cancel.cancelled() => break,
                accepted = listener.accept() => {
                    let (stream, _) = match accepted {
                        Ok(value) => value,
                        Err(err) => {
                            tracing::warn!(?err, "remote deploy listener accept failed");
                            continue;
                        }
                    };
                    let service = service.clone();
                    let connection_cancel = listener_cancel.clone();
                    tokio::spawn(async move {
                        if let Err(err) = handle_connection(stream, service, connection_cancel).await {
                            tracing::warn!(?err, "remote deploy connection failed");
                        }
                    });
                }
            }
        }
    });

    Ok(RemoteControlHandle { cancel, task })
}

async fn handle_connection(
    stream: TcpStream,
    service: RemoteDeployService,
    cancel: CancellationToken,
) -> Result<()> {
    let mut stream = BufStream::new(stream);
    let mut held_locks = HashMap::new();

    loop {
        let mut line = String::new();
        tokio::select! {
            _ = cancel.cancelled() => break,
            read = stream.read_line(&mut line) => {
                let bytes = read.map_err(|e| Error::operation("remote_control_listener", format!("read: {e}")))?;
                if bytes == 0 {
                    break;
                }
            }
        }

        let request: RemoteDeployRequest = serde_json::from_str(&line).map_err(|e| {
            Error::operation("remote_control_listener", format!("decode request: {e}"))
        })?;
        let response = service.handle_request(request, &mut held_locks).await;
        write_response(&mut stream, &response).await?;
    }

    Ok(())
}

async fn write_response(
    stream: &mut BufStream<TcpStream>,
    response: &RemoteDeployResponse,
) -> Result<()> {
    let mut line = serde_json::to_string(response).map_err(|e| {
        Error::operation("remote_control_listener", format!("encode response: {e}"))
    })?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .await
        .map_err(|e| Error::operation("remote_control_listener", format!("write: {e}")))?;
    stream
        .flush()
        .await
        .map_err(|e| Error::operation("remote_control_listener", format!("flush: {e}")))?;
    Ok(())
}

pub struct RemoteDeploySession {
    machine_id: MachineId,
    namespace: Namespace,
    deploy_id: DeployId,
    stream: Arc<Mutex<BufStream<TcpStream>>>,
    heartbeat_cancel: CancellationToken,
    heartbeat_task: JoinHandle<()>,
}

impl RemoteDeploySession {
    pub async fn connect(
        machine: &MachineRecord,
        port: u16,
        namespace: &Namespace,
        deploy_id: &DeployId,
        coordinator_id: &MachineId,
        participant_fingerprint: &[MachineId],
    ) -> Result<Self> {
        let address = SocketAddr::new(IpAddr::V6(machine.overlay_ip.0), port);
        let stream = TcpStream::connect(address)
            .await
            .map_err(|e| Error::operation("remote_deploy_connect", format!("{address}: {e}")))?;
        let stream = Arc::new(Mutex::new(BufStream::new(stream)));
        send_request(
            &stream,
            &RemoteDeployRequest::LockAcquire {
                namespace: namespace.0.clone(),
                deploy_id: deploy_id.0.clone(),
                coordinator_id: coordinator_id.0.clone(),
                participant_fingerprint: participant_fingerprint
                    .iter()
                    .map(|machine_id| machine_id.0.clone())
                    .collect(),
            },
        )
        .await?;

        let heartbeat_cancel = CancellationToken::new();
        let heartbeat_task = spawn_heartbeat(
            Arc::clone(&stream),
            heartbeat_cancel.clone(),
            namespace.clone(),
            deploy_id.clone(),
        );

        Ok(Self {
            machine_id: machine.id.clone(),
            namespace: namespace.clone(),
            deploy_id: deploy_id.clone(),
            stream,
            heartbeat_cancel,
            heartbeat_task,
        })
    }

    pub fn machine_id(&self) -> &MachineId {
        &self.machine_id
    }

    pub async fn inspect_namespace(&self) -> Result<Vec<InstanceStatusRecord>> {
        match send_request(
            &self.stream,
            &RemoteDeployRequest::InspectNamespace {
                namespace: self.namespace.0.clone(),
            },
        )
        .await?
        {
            RemoteDeployResponse::NamespaceSnapshot { instances } => Ok(instances),
            other => Err(unexpected_response("inspect_namespace", &other)),
        }
    }

    pub async fn start_candidate(
        &self,
        namespace: &Namespace,
        service: &str,
        slot_id: &SlotId,
        instance_id: &InstanceId,
        deploy_id: &DeployId,
        spec_json: &str,
    ) -> Result<InstanceStatusRecord> {
        match send_request(
            &self.stream,
            &RemoteDeployRequest::StartCandidate {
                namespace: namespace.0.clone(),
                service: service.to_string(),
                slot_id: slot_id.0.clone(),
                instance_id: instance_id.0.clone(),
                deploy_id: deploy_id.0.clone(),
                spec_json: spec_json.to_string(),
            },
        )
        .await?
        {
            RemoteDeployResponse::CandidateStarted { status } => Ok(status),
            other => Err(unexpected_response("start_candidate", &other)),
        }
    }

    pub async fn drain_instance(&self, instance_id: &InstanceId) -> Result<()> {
        expect_ack(
            "drain_instance",
            send_request(
                &self.stream,
                &RemoteDeployRequest::DrainInstance {
                    namespace: self.namespace.0.clone(),
                    instance_id: instance_id.0.clone(),
                },
            )
            .await?,
        )
    }

    pub async fn remove_instance(&self, instance_id: &InstanceId) -> Result<()> {
        expect_ack(
            "remove_instance",
            send_request(
                &self.stream,
                &RemoteDeployRequest::RemoveInstance {
                    namespace: self.namespace.0.clone(),
                    instance_id: instance_id.0.clone(),
                },
            )
            .await?,
        )
    }

    pub async fn apply_route_epoch(&self) -> Result<()> {
        expect_ack(
            "apply_route_epoch",
            send_request(
                &self.stream,
                &RemoteDeployRequest::ApplyRouteEpoch {
                    namespace: self.namespace.0.clone(),
                    deploy_id: self.deploy_id.0.clone(),
                },
            )
            .await?,
        )
    }

    pub async fn shutdown(self) {
        self.heartbeat_cancel.cancel();
        let _ = self.heartbeat_task.await;
        let _ = send_request(
            &self.stream,
            &RemoteDeployRequest::Unlock {
                namespace: self.namespace.0.clone(),
                deploy_id: self.deploy_id.0.clone(),
            },
        )
        .await;
    }
}

fn spawn_heartbeat(
    stream: Arc<Mutex<BufStream<TcpStream>>>,
    cancel: CancellationToken,
    namespace: Namespace,
    deploy_id: DeployId,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(HEARTBEAT_INTERVAL) => {
                    let result = send_request(
                        &stream,
                        &RemoteDeployRequest::LockHeartbeat {
                            namespace: namespace.0.clone(),
                            deploy_id: deploy_id.0.clone(),
                        },
                    ).await;
                    if result.is_err() {
                        break;
                    }
                }
            }
        }
    })
}

async fn send_request(
    stream: &Arc<Mutex<BufStream<TcpStream>>>,
    request: &RemoteDeployRequest,
) -> Result<RemoteDeployResponse> {
    let mut stream = stream.lock().await;
    let mut line = serde_json::to_string(request)
        .map_err(|e| Error::operation("remote_deploy_request", format!("encode request: {e}")))?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .await
        .map_err(|e| Error::operation("remote_deploy_request", format!("write request: {e}")))?;
    stream
        .flush()
        .await
        .map_err(|e| Error::operation("remote_deploy_request", format!("flush request: {e}")))?;

    let mut response_line = String::new();
    stream
        .read_line(&mut response_line)
        .await
        .map_err(|e| Error::operation("remote_deploy_request", format!("read response: {e}")))?;
    let response: RemoteDeployResponse = serde_json::from_str(&response_line)
        .map_err(|e| Error::operation("remote_deploy_request", format!("decode response: {e}")))?;
    match response {
        RemoteDeployResponse::Error { code, message } => Err(Error::operation(
            "remote_deploy_request",
            format!("{code}: {message}"),
        )),
        other => Ok(other),
    }
}

fn expect_ack(operation: &'static str, response: RemoteDeployResponse) -> Result<()> {
    match response {
        RemoteDeployResponse::Ack { .. } => Ok(()),
        other => Err(unexpected_response(operation, &other)),
    }
}

fn unexpected_response(operation: &'static str, response: &RemoteDeployResponse) -> Error {
    Error::operation(
        operation,
        format!("unexpected remote response: {response:?}"),
    )
}
