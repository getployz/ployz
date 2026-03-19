use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ployz_runtime_api::{DeployFrame, RuntimeHandle, StartCandidateRequest};
use ployz_runtime_backends::deploy::local::{
    LocalDeployRuntime, ManagedInstance, StartCandidate, now_unix_secs,
};
use ployz_store_api::internal::StoreDriver;
use ployz_store_api::{DeployReadStore, DeployWriteStore};
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    DeployId, DrainState, InstanceId, InstancePhase, InstanceStatusRecord, MachineId,
    MachineRecord, SlotId,
};
use ployz_types::spec::{Namespace, ServiceSpec};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufStream};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::locks::{NamespaceLock, NamespaceLockManager};

async fn adopt_instances(
    store: &StoreDriver,
    runtime: &LocalDeployRuntime,
    namespace: &Namespace,
) -> Result<()> {
    let existing = store.list_instance_status(namespace).await?;
    let known: BTreeSet<String> = existing
        .iter()
        .map(|record| record.instance_id.0.clone())
        .collect();
    for instance in runtime.list_instances(namespace).await? {
        if known.contains(&instance.instance_id.0) {
            continue;
        }
        let record = instance.to_status_record(
            namespace,
            InstancePhase::Ready,
            true,
            DrainState::None,
            None,
        );
        store.upsert_instance_status(&record).await?;
    }
    Ok(())
}

fn build_instance_status_record(
    namespace: &Namespace,
    instance: &ManagedInstance,
    phase: InstancePhase,
    ready: bool,
    drain_state: DrainState,
    error: Option<String>,
) -> InstanceStatusRecord {
    instance.to_status_record(namespace, phase, ready, drain_state, error)
}

async fn list_local_instance_status(
    store: &StoreDriver,
    namespace: &Namespace,
    local_machine_id: &MachineId,
) -> Result<Vec<InstanceStatusRecord>> {
    Ok(store
        .list_instance_status(namespace)
        .await?
        .into_iter()
        .filter(|record| &record.machine_id == local_machine_id)
        .collect())
}

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

#[async_trait::async_trait]
impl RuntimeHandle for RemoteControlHandle {
    async fn shutdown(self: Box<Self>) -> std::result::Result<(), String> {
        RemoteControlHandle::shutdown(*self).await;
        Ok(())
    }
}

#[derive(Clone)]
pub struct DeployAgent {
    store: StoreDriver,
    locks: NamespaceLockManager,
    local_machine_id: MachineId,
    overlay_network_name: Option<String>,
    overlay_dns_server: Option<Ipv4Addr>,
}

pub struct SessionState {
    namespace: Namespace,
    deploy_id: DeployId,
    _lock: NamespaceLock,
}

impl SessionState {
    pub(super) fn deploy_id(&self) -> &DeployId {
        &self.deploy_id
    }
}

impl DeployAgent {
    #[must_use]
    pub fn new(
        store: StoreDriver,
        locks: NamespaceLockManager,
        local_machine_id: MachineId,
        overlay_network_name: Option<String>,
        overlay_dns_server: Option<Ipv4Addr>,
    ) -> Self {
        Self {
            store,
            locks,
            local_machine_id,
            overlay_network_name,
            overlay_dns_server,
        }
    }

    pub async fn open_session(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
    ) -> Result<(SessionState, Vec<InstanceStatusRecord>)> {
        let lock = self.locks.try_acquire(namespace, deploy_id)?;
        if let Ok(runtime) = self.new_runtime() {
            adopt_instances(&self.store, &runtime, namespace).await?;
        }
        let instances =
            list_local_instance_status(&self.store, namespace, &self.local_machine_id).await?;
        let state = SessionState {
            namespace: namespace.clone(),
            deploy_id: deploy_id.clone(),
            _lock: lock,
        };
        Ok((state, instances))
    }

    pub async fn inspect_namespace(
        &self,
        session: &SessionState,
    ) -> Result<Vec<InstanceStatusRecord>> {
        if let Ok(runtime) = self.new_runtime() {
            adopt_instances(&self.store, &runtime, &session.namespace).await?;
        }
        list_local_instance_status(&self.store, &session.namespace, &self.local_machine_id).await
    }

    pub async fn start_candidate(
        &self,
        session: &SessionState,
        service: &str,
        slot_id: &SlotId,
        instance_id: &InstanceId,
        deploy_id: &DeployId,
        spec_json: &str,
    ) -> Result<InstanceStatusRecord> {
        if let Some(existing) = self
            .find_local_instance_status(&session.namespace, instance_id)
            .await?
        {
            return Ok(existing);
        }

        let spec: ServiceSpec = serde_json::from_str(spec_json)
            .map_err(|e| Error::operation("start_candidate", format!("decode spec: {e}")))?;
        if spec.name != service {
            return Err(Error::operation(
                "start_candidate",
                format!(
                    "spec service '{}' did not match request service '{}'",
                    spec.name, service
                ),
            ));
        }
        let revision_hash = spec
            .revision_hash()
            .map_err(|e| Error::operation("start_candidate", e))?;
        let runtime = self.new_runtime()?;
        let instance = runtime
            .start_candidate(StartCandidate {
                namespace: &session.namespace,
                spec: &spec,
                deploy_id,
                instance_id,
                slot_id,
                machine_id: &self.local_machine_id,
                revision_hash: &revision_hash,
            })
            .await?;
        runtime.wait_ready(&spec, &instance).await?;
        let status = build_instance_status_record(
            &session.namespace,
            &instance,
            InstancePhase::Ready,
            true,
            DrainState::None,
            None,
        );
        self.store.upsert_instance_status(&status).await?;
        Ok(status)
    }

    pub async fn drain_instance(
        &self,
        session: &SessionState,
        instance_id: &InstanceId,
    ) -> Result<()> {
        let Some(mut status) = self
            .find_local_instance_status(&session.namespace, instance_id)
            .await?
        else {
            return Ok(());
        };
        if status.phase == InstancePhase::Draining {
            return Ok(());
        }
        status.phase = InstancePhase::Draining;
        status.ready = false;
        status.drain_state = DrainState::Requested;
        status.updated_at = now_unix_secs();
        self.store.upsert_instance_status(&status).await?;
        Ok(())
    }

    pub async fn remove_instance(
        &self,
        session: &SessionState,
        instance_id: &InstanceId,
    ) -> Result<()> {
        let Some(status) = self
            .find_local_instance_status(&session.namespace, instance_id)
            .await?
        else {
            return Ok(());
        };
        let runtime = self.new_runtime()?;
        runtime
            .remove_instance(&status.instance_id, &session.namespace, &status.service)
            .await?;
        self.store
            .delete_instance_status(&status.instance_id)
            .await?;
        Ok(())
    }

    fn new_runtime(&self) -> Result<LocalDeployRuntime> {
        LocalDeployRuntime::new(self.overlay_network_name.clone(), self.overlay_dns_server)
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
    bind_addr: SocketAddr,
    store: StoreDriver,
    namespace_locks: NamespaceLockManager,
    local_machine_id: MachineId,
    overlay_network_name: Option<String>,
    overlay_dns_server: Option<Ipv4Addr>,
) -> Result<RemoteControlHandle> {
    let listener = TcpListener::bind(bind_addr).await.map_err(|e| {
        Error::operation("remote_control_listener", format!("bind {bind_addr}: {e}"))
    })?;
    let cancel = CancellationToken::new();
    let agent = DeployAgent::new(
        store,
        namespace_locks,
        local_machine_id,
        overlay_network_name,
        overlay_dns_server,
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
                    let agent = agent.clone();
                    let connection_cancel = listener_cancel.clone();
                    tokio::spawn(async move {
                        if let Err(err) = handle_connection(stream, agent, connection_cancel).await {
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
    agent: DeployAgent,
    cancel: CancellationToken,
) -> Result<()> {
    let mut stream = BufStream::new(stream);
    let open_frame = read_frame(&mut stream, &cancel).await?;
    let Some(open_frame) = open_frame else {
        return Ok(());
    };

    let DeployFrame::Open {
        namespace,
        deploy_id,
        coordinator_id,
    } = open_frame
    else {
        let response = DeployFrame::Error {
            code: "PROTOCOL_ERROR".into(),
            message: "first frame must be Open".into(),
        };
        write_frame(&mut stream, &response).await?;
        return Ok(());
    };

    let namespace = Namespace(namespace);
    let deploy_id = DeployId(deploy_id);

    let (session, instances) = match agent.open_session(&namespace, &deploy_id).await {
        Ok(result) => result,
        Err(err) => {
            let response = DeployFrame::Error {
                code: "LOCK_FAILED".into(),
                message: format!(
                    "failed to acquire lock for '{}' from coordinator '{}': {err}",
                    namespace, coordinator_id
                ),
            };
            write_frame(&mut stream, &response).await?;
            return Ok(());
        }
    };

    write_frame(&mut stream, &DeployFrame::Opened { instances }).await?;

    loop {
        let Some(frame) = read_frame(&mut stream, &cancel).await? else {
            break;
        };

        let response = handle_session_frame(&agent, &session, frame).await;
        write_frame(&mut stream, &response).await?;

        if matches!(response, DeployFrame::Ack { .. })
            && matches!(
                &response,
                DeployFrame::Ack { message } if message.starts_with("session closed")
            )
        {
            break;
        }
    }

    Ok(())
}

async fn handle_session_frame(
    agent: &DeployAgent,
    session: &SessionState,
    frame: DeployFrame,
) -> DeployFrame {
    match handle_session_frame_inner(agent, session, frame).await {
        Ok(response) => response,
        Err(err) => DeployFrame::Error {
            code: "DEPLOY_FAILED".into(),
            message: err.to_string(),
        },
    }
}

async fn handle_session_frame_inner(
    agent: &DeployAgent,
    session: &SessionState,
    frame: DeployFrame,
) -> Result<DeployFrame> {
    match frame {
        DeployFrame::Open { .. } => Err(Error::operation(
            "deploy_session",
            "duplicate Open frame on an already-open session",
        )),
        DeployFrame::InspectNamespace => {
            let instances = agent.inspect_namespace(session).await?;
            Ok(DeployFrame::NamespaceSnapshot { instances })
        }
        DeployFrame::StartCandidate {
            service,
            slot_id,
            instance_id,
            spec_json,
        } => {
            let status = agent
                .start_candidate(
                    session,
                    &service,
                    &SlotId(slot_id),
                    &InstanceId(instance_id),
                    session.deploy_id(),
                    &spec_json,
                )
                .await?;
            Ok(DeployFrame::CandidateStarted {
                status: Box::new(status),
            })
        }
        DeployFrame::DrainInstance { instance_id } => {
            agent
                .drain_instance(session, &InstanceId(instance_id))
                .await?;
            Ok(DeployFrame::Ack {
                message: "drained".into(),
            })
        }
        DeployFrame::RemoveInstance { instance_id } => {
            agent
                .remove_instance(session, &InstanceId(instance_id))
                .await?;
            Ok(DeployFrame::Ack {
                message: "removed".into(),
            })
        }
        DeployFrame::Close => Ok(DeployFrame::Ack {
            message: "session closed".into(),
        }),
        DeployFrame::Opened { .. }
        | DeployFrame::NamespaceSnapshot { .. }
        | DeployFrame::CandidateStarted { .. }
        | DeployFrame::Ack { .. }
        | DeployFrame::Error { .. } => Err(Error::operation(
            "deploy_session",
            format!("unexpected frame from client: {frame:?}"),
        )),
    }
}

async fn read_frame(
    stream: &mut BufStream<TcpStream>,
    cancel: &CancellationToken,
) -> Result<Option<DeployFrame>> {
    let mut line = String::new();
    tokio::select! {
        _ = cancel.cancelled() => Ok(None),
        read = stream.read_line(&mut line) => {
            let bytes = read.map_err(|e| Error::operation("deploy_session", format!("read: {e}")))?;
            if bytes == 0 {
                return Ok(None);
            }
            let frame: DeployFrame = serde_json::from_str(&line)
                .map_err(|e| Error::operation("deploy_session", format!("decode frame: {e}")))?;
            Ok(Some(frame))
        }
    }
}

async fn write_frame(stream: &mut BufStream<TcpStream>, frame: &DeployFrame) -> Result<()> {
    let mut line = serde_json::to_string(frame)
        .map_err(|e| Error::operation("deploy_session", format!("encode frame: {e}")))?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .await
        .map_err(|e| Error::operation("deploy_session", format!("write: {e}")))?;
    stream
        .flush()
        .await
        .map_err(|e| Error::operation("deploy_session", format!("flush: {e}")))?;
    Ok(())
}

pub struct TcpDeploySession {
    machine_id: MachineId,
    stream: BufStream<TcpStream>,
}

impl TcpDeploySession {
    pub async fn connect(
        machine: &MachineRecord,
        port: u16,
        namespace: &Namespace,
        deploy_id: &DeployId,
        coordinator_id: &MachineId,
    ) -> Result<(Self, Vec<InstanceStatusRecord>)> {
        let address = SocketAddr::new(IpAddr::V6(machine.overlay_ip.0), port);
        let tcp = TcpStream::connect(address)
            .await
            .map_err(|e| Error::operation("deploy_connect", format!("{address}: {e}")))?;
        let mut stream = BufStream::new(tcp);

        let open = DeployFrame::Open {
            namespace: namespace.0.clone(),
            deploy_id: deploy_id.0.clone(),
            coordinator_id: coordinator_id.0.clone(),
        };
        write_frame(&mut stream, &open).await?;

        let cancel = CancellationToken::new();
        let Some(response) = read_frame(&mut stream, &cancel).await? else {
            return Err(Error::operation(
                "deploy_connect",
                "connection closed before Opened response",
            ));
        };
        let DeployFrame::Opened { instances } = response else {
            return Err(Error::operation(
                "deploy_connect",
                format!("expected Opened response, got: {response:?}"),
            ));
        };

        Ok((
            Self {
                machine_id: machine.id.clone(),
                stream,
            },
            instances,
        ))
    }

    async fn send_and_recv(&mut self, frame: &DeployFrame) -> Result<DeployFrame> {
        let cancel = CancellationToken::new();
        write_frame(&mut self.stream, frame).await?;
        let Some(response) = read_frame(&mut self.stream, &cancel).await? else {
            return Err(Error::operation(
                "deploy_session",
                "connection closed while waiting for response",
            ));
        };
        match response {
            DeployFrame::Error { code, message } => Err(Error::operation(
                "deploy_session",
                format!("{code}: {message}"),
            )),
            DeployFrame::Open { .. }
            | DeployFrame::InspectNamespace
            | DeployFrame::StartCandidate { .. }
            | DeployFrame::DrainInstance { .. }
            | DeployFrame::RemoveInstance { .. }
            | DeployFrame::Close
            | DeployFrame::Opened { .. }
            | DeployFrame::NamespaceSnapshot { .. }
            | DeployFrame::CandidateStarted { .. }
            | DeployFrame::Ack { .. } => Ok(response),
        }
    }
}

#[async_trait::async_trait]
impl ployz_runtime_api::DeploySession for TcpDeploySession {
    fn machine_id(&self) -> &MachineId {
        &self.machine_id
    }

    async fn inspect_namespace(&mut self) -> Result<Vec<InstanceStatusRecord>> {
        let response = self.send_and_recv(&DeployFrame::InspectNamespace).await?;
        let DeployFrame::NamespaceSnapshot { instances } = response else {
            return Err(unexpected_response("inspect_namespace", &response));
        };
        Ok(instances)
    }

    async fn start_candidate(
        &mut self,
        req: StartCandidateRequest,
    ) -> Result<InstanceStatusRecord> {
        let response = self
            .send_and_recv(&DeployFrame::StartCandidate {
                service: req.service,
                slot_id: req.slot_id.0,
                instance_id: req.instance_id.0,
                spec_json: req.spec_json,
            })
            .await?;
        let DeployFrame::CandidateStarted { status } = response else {
            return Err(unexpected_response("start_candidate", &response));
        };
        Ok(*status)
    }

    async fn drain_instance(&mut self, instance_id: &InstanceId) -> Result<()> {
        let frame = DeployFrame::DrainInstance {
            instance_id: instance_id.0.clone(),
        };
        expect_ack("drain_instance", self.send_and_recv(&frame).await?)
    }

    async fn remove_instance(&mut self, instance_id: &InstanceId) -> Result<()> {
        let frame = DeployFrame::RemoveInstance {
            instance_id: instance_id.0.clone(),
        };
        expect_ack("remove_instance", self.send_and_recv(&frame).await?)
    }

    async fn close(mut self: Box<Self>) -> Result<()> {
        let _ = self.send_and_recv(&DeployFrame::Close).await;
        Ok(())
    }
}

fn expect_ack(operation: &'static str, response: DeployFrame) -> Result<()> {
    let DeployFrame::Ack { .. } = response else {
        return Err(unexpected_response(operation, &response));
    };
    Ok(())
}

fn unexpected_response(operation: &'static str, response: &DeployFrame) -> Error {
    Error::operation(operation, format!("unexpected response: {response:?}"))
}
