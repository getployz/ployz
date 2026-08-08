use super::network::{
    ENDPOINT_NETWORK_NAME, converge_endpoint_network, ensure_endpoint_network,
    is_docker_object_missing, read_endpoint_network_status, require_endpoint_network,
};
use super::v2_labels::{self, V2ManagedContainerLabelError};
use crate::network_mtu::{WireGuardMtuPolicy, resolve_wireguard_mtu};
use crate::roles::api::runner::{
    CreateV2ManagedContainer, ExistingManagedContainerState, ExistingV2ManagedContainer,
    MachineContainerCreateError, MachineContainerListError, MachineContainerRemoveError,
    MachineContainerStartError, MachineContainerStopError, MachineContainerStopOutcome,
    MachineEndpointNetworkConvergenceError, MachineEndpointNetworkConvergenceOutcome,
    MachineEndpointNetworkError, RuntimeLogBatch, RuntimeLogFollow, RuntimeLogLine,
    RuntimeLogStream, V2MachineContainerRunner, V2MachineLogReadError, V2MachineLogReader,
};
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::errors::Error as BollardError;
use bollard::models::{
    ContainerCreateBody, ContainerSummary, ContainerSummaryHealthStatusEnum,
    ContainerSummaryNetworkSettings, ContainerSummaryStateEnum, EndpointSettings, HealthConfig,
    HostConfig, Mount, MountPoint, MountType, NetworkingConfig, PortBinding, PortMap,
    RestartPolicy, RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    InspectContainerOptions, ListContainersOptionsBuilder, LogsOptionsBuilder,
    RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
};
use futures_util::{Stream, StreamExt};
use ployz_core::corrosion::V2ManagedContainerIdentity;
use ployz_core::deploy::{
    ContainerEntrypoint, ContainerHealthcheck, ContainerHealthcheckTest, ContainerRestartPolicy,
    ContainerRuntimeSpec, ImageReference, VolumeName,
};
use ployz_core::ids::{ContainerId, NamespaceRowId, SubjectTokenError};
use ployz_core::machine::runtime::{ContainerHealth, ManagedContainerHealthStatus};
use ployz_core::network::internal_dns::InternalDnsSearchDomain;
use ployz_core::network::{
    EndpointBridgeStatus, MachineEndpointSubnet, endpoint_bridge_gateway_ipv4,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
const MAX_LOG_TAIL_LINES: u16 = 1_000;
const MAX_LOG_TAIL_BYTES: usize = 64 * 1024;
const V2_LOG_READ_TIMEOUT: Duration = Duration::from_secs(10);
const V2_LOG_FOLLOW_CHANNEL_LINES: usize = 32;

#[derive(Debug, Clone)]
pub struct DockerManagedContainerRunner {
    docker: DockerHandle,
    endpoint_network_subnet: String,
    endpoint_bridge_ifname: String,
    endpoint_wg_ifname: String,
    endpoint_mtu_policy: WireGuardMtuPolicy,
}

/// How the runner reaches the Docker daemon: an already-built client, or a
/// client built from local defaults on first use so the machine runtime can
/// start before Docker is reachable.
#[derive(Debug, Clone)]
enum DockerHandle {
    #[cfg(test)]
    Connected(Docker),
    LazyLocalDefaults(Arc<tokio::sync::OnceCell<Docker>>),
}

impl DockerManagedContainerRunner {
    pub(crate) async fn converge_endpoint_network(
        &self,
        expected_subnet: &MachineEndpointSubnet,
    ) -> Result<MachineEndpointNetworkConvergenceOutcome, MachineEndpointNetworkConvergenceError>
    {
        let docker = self.docker().await.map_err(|error| {
            MachineEndpointNetworkConvergenceError::Inspect {
                message: error.to_string(),
            }
        })?;
        let endpoint_mtu =
            resolve_wireguard_mtu(self.endpoint_mtu_policy, &self.endpoint_wg_ifname).await;
        converge_endpoint_network(
            docker,
            expected_subnet,
            &self.endpoint_bridge_ifname,
            endpoint_mtu,
        )
        .await
    }

    /// Reports which of the requested named volumes exist locally under
    /// their deterministic v2 storage names.
    pub(crate) async fn held_v2_volumes(
        &self,
        namespace_id: &NamespaceRowId,
        volumes: &BTreeSet<VolumeName>,
    ) -> Result<BTreeSet<VolumeName>, HeldVolumesError> {
        let docker = self
            .docker()
            .await
            .map_err(|error| HeldVolumesError::DockerUnavailable {
                message: error.to_string(),
            })?;
        let mut held = BTreeSet::new();
        for volume in volumes {
            match docker
                .inspect_volume(&v2_volume_storage_name(namespace_id, volume))
                .await
            {
                Ok(_) => {
                    held.insert(volume.clone());
                }
                Err(error) if is_docker_object_missing(&error) => {}
                Err(error) => {
                    return Err(HeldVolumesError::Inspect {
                        volume: volume.clone(),
                        message: error.to_string(),
                    });
                }
            }
        }
        Ok(held)
    }

    #[cfg(test)]
    pub fn local_defaults(
        endpoint_network_subnet: impl Into<String>,
        endpoint_bridge_ifname: impl Into<String>,
        endpoint_wg_ifname: impl Into<String>,
        endpoint_mtu_policy: WireGuardMtuPolicy,
    ) -> Result<Self, DockerManagedContainerRunnerConnectError> {
        let docker = connect_local_defaults()?;
        Ok(Self {
            docker: DockerHandle::Connected(docker),
            endpoint_network_subnet: endpoint_network_subnet.into(),
            endpoint_bridge_ifname: endpoint_bridge_ifname.into(),
            endpoint_wg_ifname: endpoint_wg_ifname.into(),
            endpoint_mtu_policy,
        })
    }

    #[cfg(test)]
    pub(super) fn connected_for_test(
        docker: Docker,
        endpoint_network_subnet: impl Into<String>,
    ) -> Self {
        Self {
            docker: DockerHandle::Connected(docker),
            endpoint_network_subnet: endpoint_network_subnet.into(),
            endpoint_bridge_ifname: "br-test".to_owned(),
            endpoint_wg_ifname: "wg-test".to_owned(),
            endpoint_mtu_policy: WireGuardMtuPolicy::Fixed(1_420),
        }
    }

    #[must_use]
    pub fn lazy_local_defaults(
        endpoint_network_subnet: String,
        endpoint_bridge_ifname: String,
        endpoint_wg_ifname: String,
        endpoint_mtu_policy: WireGuardMtuPolicy,
    ) -> Self {
        Self {
            docker: DockerHandle::LazyLocalDefaults(Arc::new(tokio::sync::OnceCell::new())),
            endpoint_network_subnet,
            endpoint_bridge_ifname,
            endpoint_wg_ifname,
            endpoint_mtu_policy,
        }
    }

    pub(super) async fn docker(&self) -> Result<&Docker, DockerManagedContainerRunnerConnectError> {
        match &self.docker {
            #[cfg(test)]
            DockerHandle::Connected(docker) => Ok(docker),
            DockerHandle::LazyLocalDefaults(cell) => {
                cell.get_or_try_init(|| async { connect_local_defaults() })
                    .await
            }
        }
    }
}

fn connect_local_defaults() -> Result<Docker, DockerManagedContainerRunnerConnectError> {
    Docker::connect_with_local_defaults().map_err(|source| {
        DockerManagedContainerRunnerConnectError {
            message: source.to_string(),
        }
    })
}

impl DockerManagedContainerRunner {
    pub(crate) async fn ensure_projection_endpoint_network(
        &self,
        expected_subnet: &MachineEndpointSubnet,
    ) -> Result<(), MachineEndpointNetworkError> {
        let observed =
            MachineEndpointSubnet::try_new(&self.endpoint_network_subnet).map_err(|error| {
                MachineEndpointNetworkError::EnsureEndpointNetwork {
                    message: error.to_string(),
                }
            })?;
        if &observed != expected_subnet {
            return Err(MachineEndpointNetworkError::EndpointNetworkSubnetMismatch {
                expected: expected_subnet.clone(),
                observed,
            });
        }

        let docker = self.docker().await.map_err(|error| {
            MachineEndpointNetworkError::EnsureEndpointNetwork {
                message: error.to_string(),
            }
        })?;
        let endpoint_mtu =
            resolve_wireguard_mtu(self.endpoint_mtu_policy, &self.endpoint_wg_ifname).await;
        ensure_endpoint_network(
            docker,
            expected_subnet,
            &self.endpoint_bridge_ifname,
            endpoint_mtu,
        )
        .await
    }

    pub(crate) async fn read_endpoint_network_status(&self) -> EndpointBridgeStatus {
        let expected = match MachineEndpointSubnet::try_new(&self.endpoint_network_subnet) {
            Ok(expected) => expected,
            Err(_) => {
                return EndpointBridgeStatus::InvalidSubnet {
                    observed: self.endpoint_network_subnet.clone(),
                };
            }
        };
        let docker = match self.docker().await {
            Ok(docker) => docker,
            Err(error) => {
                return EndpointBridgeStatus::Unavailable {
                    message: ployz_core::operation::FailureMessage::try_new(error.to_string())
                        .expect("Docker connection failure is non-empty"),
                };
            }
        };
        let endpoint_mtu =
            resolve_wireguard_mtu(self.endpoint_mtu_policy, &self.endpoint_wg_ifname).await;
        read_endpoint_network_status(docker, expected, &self.endpoint_bridge_ifname, endpoint_mtu)
            .await
    }
}

impl V2MachineContainerRunner for DockerManagedContainerRunner {
    async fn existing_v2_managed_containers(
        &self,
    ) -> Result<Vec<ExistingV2ManagedContainer>, MachineContainerListError> {
        let docker =
            self.docker()
                .await
                .map_err(|error| MachineContainerListError::ListExisting {
                    message: error.to_string(),
                })?;
        docker
            .list_containers(Some(v2_managed_container_list_options()))
            .await
            .map_err(|error| MachineContainerListError::ListExisting {
                message: error.to_string(),
            })?
            .into_iter()
            .map(existing_v2_container_from_summary)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| MachineContainerListError::ListExisting {
                message: error.to_string(),
            })
    }

    async fn create_v2_managed_container(
        &self,
        command: CreateV2ManagedContainer,
    ) -> Result<ContainerId, MachineContainerCreateError> {
        let endpoint_network_subnet = MachineEndpointSubnet::try_new(&self.endpoint_network_subnet)
            .map_err(|error| MachineContainerCreateError::EnsureEndpointNetwork {
                message: error.to_string(),
            })?;
        let docker = self
            .docker()
            .await
            .map_err(|error| MachineContainerCreateError::Create {
                message: error.to_string(),
            })?;
        let endpoint_mtu =
            resolve_wireguard_mtu(self.endpoint_mtu_policy, &self.endpoint_wg_ifname).await;
        require_endpoint_network(
            docker,
            &endpoint_network_subnet,
            &self.endpoint_bridge_ifname,
            endpoint_mtu,
        )
        .await?;
        let response = docker
            .create_container(None, create_v2_body(command, &self.endpoint_network_subnet))
            .await
            .map_err(|error| MachineContainerCreateError::Create {
                message: error.to_string(),
            })?;
        ContainerId::try_new(response.id).map_err(|error| MachineContainerCreateError::Create {
            message: error.to_string(),
        })
    }

    async fn start_v2_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> Result<(), MachineContainerStartError> {
        let docker = self
            .docker()
            .await
            .map_err(|error| MachineContainerStartError::Start {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        docker
            .start_container(container_id.as_str(), None)
            .await
            .map_err(|error| MachineContainerStartError::Start {
                container_id: container_id.clone(),
                message: error.to_string(),
            })
    }

    async fn stop_v2_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &V2ManagedContainerIdentity,
    ) -> Result<MachineContainerStopOutcome, MachineContainerStopError> {
        let docker = self
            .docker()
            .await
            .map_err(|error| MachineContainerStopError::Stop {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        let inspect = match docker
            .inspect_container(container_id.as_str(), None::<InspectContainerOptions>)
            .await
        {
            Ok(inspect) => inspect,
            Err(error) if is_docker_object_missing(&error) => {
                return Ok(MachineContainerStopOutcome::Missing);
            }
            Err(error) => {
                return Err(MachineContainerStopError::Stop {
                    container_id: container_id.clone(),
                    message: error.to_string(),
                });
            }
        };
        let labels = inspect
            .config
            .and_then(|config| config.labels)
            .ok_or_else(|| MachineContainerStopError::Stop {
                container_id: container_id.clone(),
                message: "Docker inspect omitted container labels".to_owned(),
            })?;
        let observed_identity = v2_labels::parse(&btree_from_hashmap(labels)).map_err(|error| {
            MachineContainerStopError::Stop {
                container_id: container_id.clone(),
                message: format!(
                    "container labels did not contain a valid v2 row identity: {error:?}"
                ),
            }
        })?;
        if observed_identity != *expected_identity {
            return Err(MachineContainerStopError::Stop {
                container_id: container_id.clone(),
                message: format!(
                    "container identity did not match stop target: expected {:?}, found {:?}",
                    expected_identity, observed_identity
                ),
            });
        }
        let running = inspect
            .state
            .and_then(|state| state.running)
            .ok_or_else(|| MachineContainerStopError::Stop {
                container_id: container_id.clone(),
                message: "Docker inspect omitted container running state".to_owned(),
            })?;
        if !running {
            return Ok(MachineContainerStopOutcome::AlreadyStopped);
        }
        // No timeout override: Docker honors the container's configured
        // StopTimeout from creation.
        let options = StopContainerOptionsBuilder::new().build();
        match docker
            .stop_container(container_id.as_str(), Some(options))
            .await
        {
            Ok(()) => Ok(MachineContainerStopOutcome::StoppedRunning),
            Err(error) if is_docker_object_missing(&error) => {
                Ok(MachineContainerStopOutcome::Missing)
            }
            Err(BollardError::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(MachineContainerStopOutcome::AlreadyStopped),
            Err(error) => Err(MachineContainerStopError::Stop {
                container_id: container_id.clone(),
                message: error.to_string(),
            }),
        }
    }

    async fn remove_v2_managed_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &V2ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRemoveError> {
        let existing = self
            .existing_v2_managed_containers()
            .await
            .map_err(|error| match error {
                MachineContainerListError::ListExisting { message } => {
                    MachineContainerRemoveError::ListExisting { message }
                }
            })?
            .into_iter()
            .find(|container| container.container_id == *container_id);
        let Some(existing) = existing else {
            return Ok(());
        };
        if existing.identity != *expected_identity {
            return Err(MachineContainerRemoveError::Remove {
                container_id: container_id.clone(),
                message: format!(
                    "container identity did not match cleanup target: expected {:?}, found {:?}",
                    expected_identity, existing.identity
                ),
            });
        }

        let docker = self
            .docker()
            .await
            .map_err(|error| MachineContainerRemoveError::Remove {
                container_id: container_id.clone(),
                message: error.to_string(),
            })?;
        // Volumes are never removed with their container: a superseded
        // incumbent's data must survive for the successor to mount.
        let options = RemoveContainerOptionsBuilder::new().force(true).build();
        match docker
            .remove_container(container_id.as_str(), Some(options))
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if is_docker_object_missing(&error) => Ok(()),
            Err(error) => Err(MachineContainerRemoveError::Remove {
                container_id: container_id.clone(),
                message: error.to_string(),
            }),
        }
    }
}

impl V2MachineLogReader for DockerManagedContainerRunner {
    async fn tail_v2_container_logs(
        &self,
        container_id: &ContainerId,
        tail_lines: u16,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<RuntimeLogBatch, V2MachineLogReadError> {
        let docker = self
            .docker()
            .await
            .map_err(|_| V2MachineLogReadError::RuntimeUnavailable)?;
        let requested_lines = tail_lines.min(MAX_LOG_TAIL_LINES);
        let stream = docker.logs(
            container_id.as_str(),
            Some(v2_logs_options(requested_lines, V2LogAttachMode::Tail)),
        );
        collect_v2_log_stream(
            stream,
            container_id,
            usize::from(requested_lines),
            MAX_LOG_TAIL_BYTES,
            V2_LOG_READ_TIMEOUT,
            shutdown,
        )
        .await
    }

    async fn open_v2_container_log_follow(
        &self,
        container_id: &ContainerId,
        tail_lines: u16,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<RuntimeLogFollow, V2MachineLogReadError> {
        if *shutdown.borrow() {
            return Err(V2MachineLogReadError::Cancelled);
        }
        let docker = self
            .docker()
            .await
            .map_err(|_| V2MachineLogReadError::RuntimeUnavailable)?
            .clone();
        let container_id = container_id.clone();
        let (sender, receiver) = tokio::sync::mpsc::channel(V2_LOG_FOLLOW_CHANNEL_LINES);
        tokio::spawn(run_v2_log_follow(
            docker,
            container_id,
            tail_lines.min(MAX_LOG_TAIL_LINES),
            sender,
            shutdown,
        ));
        Ok(RuntimeLogFollow::new(receiver))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum V2LogAttachMode {
    Tail,
    Follow,
}

async fn collect_v2_log_stream<LogStream>(
    stream: LogStream,
    container_id: &ContainerId,
    max_lines: usize,
    max_bytes: usize,
    deadline: Duration,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<RuntimeLogBatch, V2MachineLogReadError>
where
    LogStream: Stream<Item = Result<LogOutput, BollardError>> + Send,
{
    if *shutdown.borrow() {
        return Err(V2MachineLogReadError::Cancelled);
    }
    let mut collector = CompleteLogLineCollector::new(max_lines, max_bytes);
    let deadline = tokio::time::sleep(deadline);
    tokio::pin!(deadline);
    tokio::pin!(stream);
    loop {
        tokio::select! {
            () = &mut deadline => {
                return Err(V2MachineLogReadError::TimedOut);
            }
            () = wait_for_log_shutdown(&mut shutdown) => {
                return Err(V2MachineLogReadError::Cancelled);
            }
            item = stream.next() => {
                let Some(item) = item else {
                    return Ok(collector.finish_eof());
                };
                match item {
                    Ok(output) => {
                        if !collector.push(output)? {
                            return Ok(collector.finish_interrupted());
                        }
                    }
                    Err(error) => return Err(classify_v2_log_error(container_id, &error)),
                }
            }
        }
    }
}

async fn run_v2_log_follow(
    docker: Docker,
    container_id: ContainerId,
    tail_lines: u16,
    sender: tokio::sync::mpsc::Sender<Result<RuntimeLogLine, V2MachineLogReadError>>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let stream = docker.logs(
        container_id.as_str(),
        Some(v2_logs_options(tail_lines, V2LogAttachMode::Follow)),
    );
    run_v2_log_follow_stream(stream, container_id, sender, shutdown).await;
}

async fn run_v2_log_follow_stream<LogStream>(
    stream: LogStream,
    container_id: ContainerId,
    sender: tokio::sync::mpsc::Sender<Result<RuntimeLogLine, V2MachineLogReadError>>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) where
    LogStream: Stream<Item = Result<LogOutput, BollardError>> + Send,
{
    tokio::pin!(stream);
    let mut framer = FollowLogLineFramer::new(MAX_LOG_TAIL_BYTES);
    loop {
        tokio::select! {
            () = sender.closed() => return,
            () = wait_for_log_shutdown(&mut shutdown) => {
                let _ = sender.try_send(Err(V2MachineLogReadError::Cancelled));
                return;
            }
            item = stream.next() => {
                let Some(item) = item else {
                    for line in framer.finish_eof() {
                        if !send_v2_follow_item(&sender, Ok(line), &mut shutdown).await {
                            return;
                        }
                    }
                    return;
                };
                match item {
                    Ok(output) => {
                        if !frame_and_send_v2_follow_output(
                            &mut framer,
                            output,
                            &sender,
                            &mut shutdown,
                        )
                        .await
                        {
                            return;
                        }
                    }
                    Err(error) => {
                        let error = classify_v2_log_error(&container_id, &error);
                        let _ = send_v2_follow_item(&sender, Err(error), &mut shutdown).await;
                        return;
                    }
                }
            }
        }
    }
}

async fn frame_and_send_v2_follow_output(
    framer: &mut FollowLogLineFramer,
    output: LogOutput,
    sender: &tokio::sync::mpsc::Sender<Result<RuntimeLogLine, V2MachineLogReadError>>,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> bool {
    let (stream, message) = match output {
        LogOutput::StdOut { message } => (RuntimeLogStream::Stdout, message),
        LogOutput::StdErr { message } => (RuntimeLogStream::Stderr, message),
        LogOutput::StdIn { .. } | LogOutput::Console { .. } => {
            let _ =
                send_v2_follow_item(sender, Err(V2MachineLogReadError::ReadFailed), shutdown).await;
            return false;
        }
    };
    for segment in message.split_inclusive(|byte| *byte == b'\n') {
        let line = match framer.push_segment(stream, segment) {
            Ok(line) => line,
            Err(error) => {
                let _ = send_v2_follow_item(sender, Err(error), shutdown).await;
                return false;
            }
        };
        if let Some(line) = line
            && !send_v2_follow_item(sender, Ok(line), shutdown).await
        {
            return false;
        }
    }
    true
}

async fn send_v2_follow_item(
    sender: &tokio::sync::mpsc::Sender<Result<RuntimeLogLine, V2MachineLogReadError>>,
    item: Result<RuntimeLogLine, V2MachineLogReadError>,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> bool {
    tokio::select! {
        result = sender.send(item) => result.is_ok(),
        () = wait_for_log_shutdown(shutdown) => false,
    }
}

async fn wait_for_log_shutdown(shutdown: &mut tokio::sync::watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() || shutdown.changed().await.is_err() {
            return;
        }
    }
}

fn classify_v2_log_error(
    container_id: &ContainerId,
    error: &BollardError,
) -> V2MachineLogReadError {
    if is_docker_object_missing(error) {
        V2MachineLogReadError::NotFound {
            container_id: container_id.clone(),
        }
    } else {
        V2MachineLogReadError::ReadFailed
    }
}

#[derive(Debug)]
struct FollowLogLineFramer {
    stdout: PendingLogLine,
    stderr: PendingLogLine,
    max_pending_bytes: usize,
    next_pending_order: u64,
}

impl FollowLogLineFramer {
    fn new(max_pending_bytes: usize) -> Self {
        Self {
            stdout: PendingLogLine::new(),
            stderr: PendingLogLine::new(),
            max_pending_bytes,
            next_pending_order: 0,
        }
    }

    fn push_segment(
        &mut self,
        stream: RuntimeLogStream,
        segment: &[u8],
    ) -> Result<Option<RuntimeLogLine>, V2MachineLogReadError> {
        let complete = segment.last() == Some(&b'\n');
        let content = if complete {
            segment
                .get(..segment.len().saturating_sub(1))
                .expect("newline-delimited segment has a bounded prefix")
        } else {
            segment
        };
        let pending_bytes = self
            .stdout
            .bytes
            .len()
            .saturating_add(self.stderr.bytes.len());
        let Some(next_pending_bytes) = pending_bytes.checked_add(content.len()) else {
            return Err(V2MachineLogReadError::ReadFailed);
        };
        if next_pending_bytes > self.max_pending_bytes {
            return Err(V2MachineLogReadError::ReadFailed);
        }
        self.mark_pending(stream);
        self.pending_mut(stream).bytes.extend_from_slice(content);
        if !complete {
            return Ok(None);
        }
        let pending = self.take_pending(stream);
        Ok(Some(RuntimeLogLine {
            stream,
            line: render_log_line(pending.bytes),
        }))
    }

    fn finish_eof(mut self) -> Vec<RuntimeLogLine> {
        let mut pending = [
            (RuntimeLogStream::Stdout, self.stdout.order),
            (RuntimeLogStream::Stderr, self.stderr.order),
        ];
        pending.sort_by_key(|(_, order)| *order);
        pending
            .into_iter()
            .filter_map(|(stream, order)| {
                if order.is_none() || self.pending(stream).bytes.is_empty() {
                    return None;
                }
                let pending = self.take_pending(stream);
                Some(RuntimeLogLine {
                    stream,
                    line: render_log_line(pending.bytes),
                })
            })
            .collect()
    }

    fn mark_pending(&mut self, stream: RuntimeLogStream) {
        if self.pending(stream).order.is_none() {
            let order = self.next_pending_order;
            self.next_pending_order = self.next_pending_order.saturating_add(1);
            self.pending_mut(stream).order = Some(order);
        }
    }

    fn pending(&self, stream: RuntimeLogStream) -> &PendingLogLine {
        match stream {
            RuntimeLogStream::Stdout => &self.stdout,
            RuntimeLogStream::Stderr => &self.stderr,
        }
    }

    fn pending_mut(&mut self, stream: RuntimeLogStream) -> &mut PendingLogLine {
        match stream {
            RuntimeLogStream::Stdout => &mut self.stdout,
            RuntimeLogStream::Stderr => &mut self.stderr,
        }
    }

    fn take_pending(&mut self, stream: RuntimeLogStream) -> PendingLogLine {
        match stream {
            RuntimeLogStream::Stdout => std::mem::replace(&mut self.stdout, PendingLogLine::new()),
            RuntimeLogStream::Stderr => std::mem::replace(&mut self.stderr, PendingLogLine::new()),
        }
    }
}

#[derive(Debug)]
struct CompleteLogLineCollector {
    lines: Vec<RuntimeLogLine>,
    stdout: PendingLogLine,
    stderr: PendingLogLine,
    retained_bytes: usize,
    max_lines: usize,
    max_bytes: usize,
    next_pending_order: u64,
    truncated: bool,
}

impl CompleteLogLineCollector {
    fn new(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            lines: Vec::new(),
            stdout: PendingLogLine::new(),
            stderr: PendingLogLine::new(),
            retained_bytes: 0,
            max_lines,
            max_bytes,
            next_pending_order: 0,
            truncated: false,
        }
    }

    fn push(&mut self, output: LogOutput) -> Result<bool, V2MachineLogReadError> {
        let (stream, message) = match output {
            LogOutput::StdOut { message } => (RuntimeLogStream::Stdout, message),
            LogOutput::StdErr { message } => (RuntimeLogStream::Stderr, message),
            LogOutput::StdIn { .. } | LogOutput::Console { .. } => {
                return Err(V2MachineLogReadError::ReadFailed);
            }
        };
        for segment in message.split_inclusive(|byte| *byte == b'\n') {
            let complete = segment.last() == Some(&b'\n');
            let content = if complete {
                segment
                    .get(..segment.len().saturating_sub(1))
                    .expect("newline-delimited segment has a bounded prefix")
            } else {
                segment
            };
            if !self.append(stream, content) {
                return Ok(false);
            }
            if complete && !self.complete(stream) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn append(&mut self, stream: RuntimeLogStream, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            self.mark_pending(stream);
            return true;
        }
        let Some(next_size) = self.retained_bytes.checked_add(bytes.len()) else {
            self.truncated = true;
            return false;
        };
        if next_size > self.max_bytes {
            self.truncated = true;
            return false;
        }
        self.mark_pending(stream);
        self.pending_mut(stream).bytes.extend_from_slice(bytes);
        self.retained_bytes = next_size;
        true
    }

    fn mark_pending(&mut self, stream: RuntimeLogStream) {
        if self.pending(stream).order.is_none() {
            let order = self.next_pending_order;
            self.next_pending_order = self.next_pending_order.saturating_add(1);
            self.pending_mut(stream).order = Some(order);
        }
    }

    fn complete(&mut self, stream: RuntimeLogStream) -> bool {
        if self.lines.len() >= self.max_lines {
            self.truncated = true;
            return false;
        }
        let pending = self.take_pending(stream);
        self.lines.push(RuntimeLogLine {
            stream,
            line: render_log_line(pending.bytes),
        });
        true
    }

    fn finish_eof(mut self) -> RuntimeLogBatch {
        let mut pending = [
            (RuntimeLogStream::Stdout, self.stdout.order),
            (RuntimeLogStream::Stderr, self.stderr.order),
        ];
        pending.sort_by_key(|(_, order)| *order);
        for (stream, order) in pending {
            if order.is_some() && !self.pending(stream).bytes.is_empty() && !self.complete(stream) {
                break;
            }
        }
        RuntimeLogBatch {
            lines: self.lines,
            truncated: self.truncated,
        }
    }

    fn finish_interrupted(mut self) -> RuntimeLogBatch {
        if !self.stdout.bytes.is_empty() || !self.stderr.bytes.is_empty() {
            self.truncated = true;
        }
        RuntimeLogBatch {
            lines: self.lines,
            truncated: self.truncated,
        }
    }

    fn pending(&self, stream: RuntimeLogStream) -> &PendingLogLine {
        match stream {
            RuntimeLogStream::Stdout => &self.stdout,
            RuntimeLogStream::Stderr => &self.stderr,
        }
    }

    fn pending_mut(&mut self, stream: RuntimeLogStream) -> &mut PendingLogLine {
        match stream {
            RuntimeLogStream::Stdout => &mut self.stdout,
            RuntimeLogStream::Stderr => &mut self.stderr,
        }
    }

    fn take_pending(&mut self, stream: RuntimeLogStream) -> PendingLogLine {
        match stream {
            RuntimeLogStream::Stdout => std::mem::replace(&mut self.stdout, PendingLogLine::new()),
            RuntimeLogStream::Stderr => std::mem::replace(&mut self.stderr, PendingLogLine::new()),
        }
    }
}

#[derive(Debug)]
struct PendingLogLine {
    bytes: Vec<u8>,
    order: Option<u64>,
}

impl PendingLogLine {
    const fn new() -> Self {
        Self {
            bytes: Vec::new(),
            order: None,
        }
    }
}

fn render_log_line(mut bytes: Vec<u8>) -> String {
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn docker_container_state(
    state: ContainerSummaryStateEnum,
    network_settings: Option<ContainerSummaryNetworkSettings>,
) -> Result<ExistingManagedContainerState, DockerManagedContainerSummaryError> {
    match state {
        ContainerSummaryStateEnum::RUNNING => Ok(ExistingManagedContainerState::Running {
            ip: container_ip(network_settings)?,
            health: ContainerHealth::None,
            started_at_unix_ms: None,
        }),
        ContainerSummaryStateEnum::CREATED | ContainerSummaryStateEnum::EXITED => {
            Ok(ExistingManagedContainerState::StartableStopped)
        }
        ContainerSummaryStateEnum::PAUSED
        | ContainerSummaryStateEnum::RESTARTING
        | ContainerSummaryStateEnum::REMOVING
        | ContainerSummaryStateEnum::DEAD
        | ContainerSummaryStateEnum::STOPPING => Ok(ExistingManagedContainerState::NotStartable {
            description: state.to_string(),
        }),
        ContainerSummaryStateEnum::EMPTY => Err(DockerManagedContainerSummaryError::MissingState),
    }
}

fn v2_named_volume_names_from_mounts(
    mounts: &[MountPoint],
    namespace_id: &NamespaceRowId,
) -> Result<BTreeSet<VolumeName>, String> {
    collect_named_volume_names(mounts, |name| {
        v2_volume_name_from_storage_name(name, namespace_id)
    })
}

fn collect_named_volume_names(
    mounts: &[MountPoint],
    parse: impl Fn(&str) -> Result<VolumeName, String>,
) -> Result<BTreeSet<VolumeName>, String> {
    let mut names = BTreeSet::new();
    for mount in mounts {
        let Some(kind) = mount.typ.as_deref() else {
            return Err("Docker inspect mount omitted its type".to_owned());
        };
        if kind != "volume" {
            continue;
        }
        let Some(name) = mount.name.as_deref().filter(|name| !name.is_empty()) else {
            return Err("Docker inspect named-volume mount omitted its name".to_owned());
        };
        names.insert(parse(name)?);
    }
    Ok(names)
}

/// A held-volume inventory failure, split by whether the Docker daemon was
/// reachable at all or a specific volume inspect failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HeldVolumesError {
    #[error("docker unavailable: {message}")]
    DockerUnavailable { message: String },
    #[error("inspecting volume {volume} failed: {message}", volume = .volume.as_str())]
    Inspect { volume: VolumeName, message: String },
}

/// Storage identity of a row-scoped v2 named volume on the local Docker host.
fn v2_volume_storage_name(namespace_id: &NamespaceRowId, volume_name: &VolumeName) -> String {
    let namespace = namespace_id.as_str();
    format!(
        "ployz-v2-n{}-{namespace}-v{}-{}",
        namespace.len(),
        volume_name.as_str().len(),
        volume_name.as_str()
    )
}

/// Decodes a v2 storage name back to its volume name, accepting only names
/// that re-render byte-for-byte for the expected namespace row.
fn v2_volume_name_from_storage_name(
    storage_name: &str,
    namespace_id: &NamespaceRowId,
) -> Result<VolumeName, String> {
    let namespace = namespace_id.as_str();
    let prefix = format!("ployz-v2-n{}-{namespace}-v", namespace.len());
    let volume_name = storage_name
        .strip_prefix(prefix.as_str())
        .and_then(|rest| rest.split_once('-'))
        .and_then(|(_, name)| VolumeName::try_new(name).ok())
        .filter(|name| v2_volume_storage_name(namespace_id, name) == storage_name);
    volume_name.ok_or_else(|| format!("invalid v2 Docker named-volume identity `{storage_name}`"))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("failed to connect to local Docker: {message}")]
pub struct DockerManagedContainerRunnerConnectError {
    message: String,
}

fn v2_managed_container_list_options() -> bollard::query_parameters::ListContainersOptions {
    let filters = HashMap::from([(
        "label".to_owned(),
        vec![format!(
            "{}={}",
            v2_labels::IDENTITY_SCHEMA_LABEL,
            v2_labels::V2_IDENTITY_SCHEMA
        )],
    )]);
    ListContainersOptionsBuilder::new()
        .all(true)
        .filters(&filters)
        .build()
}

fn v2_logs_options(
    tail_lines: u16,
    attach: V2LogAttachMode,
) -> bollard::query_parameters::LogsOptions {
    let tail = tail_lines.to_string();
    LogsOptionsBuilder::default()
        .follow(matches!(attach, V2LogAttachMode::Follow))
        .stdout(true)
        .stderr(true)
        .timestamps(false)
        .tail(&tail)
        .build()
}

fn create_v2_body(
    command: CreateV2ManagedContainer,
    endpoint_network_subnet: &str,
) -> ContainerCreateBody {
    let CreateV2ManagedContainer {
        image,
        runtime,
        dns_search_domain,
        identity,
        host_ports,
    } = command;
    create_body_for_identity(
        image,
        runtime,
        identity,
        dns_search_domain,
        endpoint_network_subnet,
        &host_ports,
    )
}

fn create_body_for_identity(
    image: ImageReference,
    runtime: ContainerRuntimeSpec,
    identity: V2ManagedContainerIdentity,
    dns_search_domain: InternalDnsSearchDomain,
    endpoint_network_subnet: &str,
    host_ports: &ployz_core::corrosion::HostPortBindings,
) -> ContainerCreateBody {
    let image = image.as_str().to_owned();
    let env = if runtime.environment.is_empty() {
        None
    } else {
        Some(
            runtime
                .environment
                .iter()
                .map(|(name, value)| format!("{}={}", name.as_str(), value.as_str()))
                .collect(),
        )
    };
    let cmd = runtime.command.map(Vec::from);
    let entrypoint = runtime.entrypoint.map(|entrypoint| match entrypoint {
        ContainerEntrypoint::Clear => Vec::new(),
        ContainerEntrypoint::Argv(argv) => Vec::from(argv),
    });
    let healthcheck = runtime.healthcheck.as_ref().map(health_config);
    let restart_policy = restart_policy(runtime.restart_policy);
    let cap_add = capabilities(&runtime.cap_add);
    let cap_drop = capabilities(&runtime.cap_drop);
    let memory = runtime
        .resources
        .memory_bytes
        .map(|value| saturating_i64(value.get()));
    let nano_cpus = runtime
        .resources
        .nano_cpus
        .map(|value| saturating_i64(value.get()));
    let pids_limit = runtime.resources.pids.map(|value| value.get());
    // ponytail: invalid endpoint subnet omits the resolver; validated machine
    // configuration is the upgrade path.
    let dns = endpoint_bridge_gateway_ipv4(endpoint_network_subnet)
        .map(|gateway| vec![gateway.to_string()]);
    let dns_search = Some(vec![dns_search_domain.as_str().to_owned()]);
    let (exposed_ports, port_bindings) = docker_port_publications(host_ports);
    ContainerCreateBody {
        image: Some(image),
        env,
        cmd,
        entrypoint,
        healthcheck,
        stop_timeout: Some(i64::from(runtime.stop_grace_period.as_seconds())),
        labels: Some(hashmap_from_btree(v2_labels::render(&identity))),
        exposed_ports,
        host_config: Some(HostConfig {
            network_mode: Some(ENDPOINT_NETWORK_NAME.to_owned()),
            mounts: docker_volume_mounts(&identity, &runtime.volume_mounts),
            restart_policy,
            cap_add,
            cap_drop,
            memory,
            nano_cpus,
            pids_limit,
            port_bindings,
            dns,
            dns_search,
            // musl applies the namespace search domain only when a query has fewer dots
            // than ndots; an inherited host ndots:0 disables search for plain service names.
            dns_options: Some(vec!["ndots:1".to_owned()]),
            ..Default::default()
        }),
        networking_config: Some(NetworkingConfig {
            endpoints_config: Some(HashMap::from([(
                ENDPOINT_NETWORK_NAME.to_owned(),
                EndpointSettings::default(),
            )])),
        }),
        ..Default::default()
    }
}

/// Renders host-published ports into Docker's exposed-port and binding maps.
/// Empty bindings yield `None`, keeping non-publishing creates byte-identical
/// to the pre-placement body.
fn docker_port_publications(
    host_ports: &ployz_core::corrosion::HostPortBindings,
) -> (Option<Vec<String>>, Option<PortMap>) {
    if host_ports.is_empty() {
        return (None, None);
    }
    // Exposed ports are keyed by container port, so two host ports
    // forwarding the same container port share one exposed entry.
    let mut exposed = std::collections::BTreeSet::new();
    let mut bindings: PortMap = HashMap::new();
    for binding in host_ports.iter() {
        let protocol = match binding.protocol {
            ployz_core::corrosion::HostPortProtocol::Tcp => "tcp",
            ployz_core::corrosion::HostPortProtocol::Udp => "udp",
        };
        let container_key = format!("{}/{protocol}", binding.container_port);
        exposed.insert(container_key.clone());
        bindings
            .entry(container_key)
            .or_insert_with(|| Some(Vec::new()))
            .get_or_insert_with(Vec::new)
            .push(PortBinding {
                host_ip: None,
                host_port: Some(binding.host_port.to_string()),
            });
    }
    (Some(exposed.into_iter().collect()), Some(bindings))
}

fn docker_volume_mounts(
    identity: &V2ManagedContainerIdentity,
    mounts: &[ployz_core::deploy::ServiceVolumeMount],
) -> Option<Vec<Mount>> {
    if mounts.is_empty() {
        return None;
    }
    Some(
        mounts
            .iter()
            .map(|mount| Mount {
                target: Some(mount.target.as_str().to_owned()),
                source: Some(v2_volume_storage_name(
                    &identity.namespace_id,
                    &mount.volume_name,
                )),
                typ: Some(MountType::VOLUME),
                read_only: None,
                consistency: None,
                bind_options: None,
                volume_options: None,
                image_options: None,
                tmpfs_options: None,
            })
            .collect(),
    )
}

fn health_config(healthcheck: &ContainerHealthcheck) -> HealthConfig {
    HealthConfig {
        test: Some(match &healthcheck.test {
            ContainerHealthcheckTest::Inherit => Vec::new(),
            ContainerHealthcheckTest::Disable => vec!["NONE".to_owned()],
            ContainerHealthcheckTest::Exec(command) => std::iter::once("CMD".to_owned())
                .chain(command.as_slice().iter().cloned())
                .collect(),
            ContainerHealthcheckTest::Shell(command) => {
                vec!["CMD-SHELL".to_owned(), command.as_str().to_owned()]
            }
        }),
        interval: healthcheck
            .interval
            .map(|value| saturating_i64(value.as_nanos())),
        timeout: healthcheck
            .timeout
            .map(|value| saturating_i64(value.as_nanos())),
        retries: healthcheck.retries.map(|value| i64::from(value.get())),
        start_period: healthcheck
            .start_period
            .map(|value| saturating_i64(value.as_nanos())),
        start_interval: None,
    }
}

/// Docker's API carries byte and nanosecond quantities as `i64`; product
/// values are `u64`, so anything past `i64::MAX` (physically impossible
/// limits) clamps instead of failing the create call.
fn saturating_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn restart_policy(policy: ContainerRestartPolicy) -> Option<RestartPolicy> {
    let name = match policy {
        ContainerRestartPolicy::DockerDefault => return None,
        ContainerRestartPolicy::No => RestartPolicyNameEnum::NO,
        ContainerRestartPolicy::Always => RestartPolicyNameEnum::ALWAYS,
        ContainerRestartPolicy::OnFailure => RestartPolicyNameEnum::ON_FAILURE,
        ContainerRestartPolicy::UnlessStopped => RestartPolicyNameEnum::UNLESS_STOPPED,
    };
    Some(RestartPolicy {
        name: Some(name),
        maximum_retry_count: None,
    })
}

fn capabilities(capabilities: &[ployz_core::deploy::LinuxCapability]) -> Option<Vec<String>> {
    if capabilities.is_empty() {
        return None;
    }
    Some(
        ployz_core::deploy::canonical_capabilities(capabilities)
            .into_iter()
            .map(|capability| capability.as_str().to_owned())
            .collect(),
    )
}

fn existing_v2_container_from_summary(
    summary: ContainerSummary,
) -> Result<ExistingV2ManagedContainer, DockerManagedContainerSummaryError> {
    let id = summary
        .id
        .ok_or(DockerManagedContainerSummaryError::MissingId)?;
    let labels = summary
        .labels
        .ok_or(DockerManagedContainerSummaryError::MissingLabels)?;
    let state = summary
        .state
        .ok_or(DockerManagedContainerSummaryError::MissingState)?;
    let health_status = summary
        .health
        .and_then(|health| health.status)
        .and_then(docker_health_status);
    let identity = v2_labels::parse(&btree_from_hashmap(labels))
        .map_err(DockerManagedContainerSummaryError::InvalidV2Labels)?;
    let mounts = summary
        .mounts
        .as_deref()
        .ok_or(DockerManagedContainerSummaryError::MissingMounts)?;
    let named_volume_names = v2_named_volume_names_from_mounts(mounts, &identity.namespace_id)
        .map_err(DockerManagedContainerSummaryError::InvalidNamedVolumeMounts)?;

    Ok(ExistingV2ManagedContainer {
        container_id: ContainerId::try_new(id)
            .map_err(DockerManagedContainerSummaryError::InvalidContainerId)?,
        identity,
        state: docker_container_state(state, summary.network_settings)?,
        health_status: health_status.or_else(|| summary.status.as_deref().and_then(status_health)),
        resolved_image_identity: summary.image_id,
        created_at_unix_seconds: summary.created,
        named_volume_names,
    })
}

fn docker_health_status(
    status: ContainerSummaryHealthStatusEnum,
) -> Option<ManagedContainerHealthStatus> {
    match status {
        ContainerSummaryHealthStatusEnum::EMPTY | ContainerSummaryHealthStatusEnum::NONE => None,
        ContainerSummaryHealthStatusEnum::STARTING => Some(ManagedContainerHealthStatus::Starting),
        ContainerSummaryHealthStatusEnum::HEALTHY => Some(ManagedContainerHealthStatus::Healthy),
        ContainerSummaryHealthStatusEnum::UNHEALTHY => {
            Some(ManagedContainerHealthStatus::Unhealthy)
        }
    }
}

fn status_health(status: &str) -> Option<ManagedContainerHealthStatus> {
    if status.contains("unhealthy") {
        Some(ManagedContainerHealthStatus::Unhealthy)
    } else if status.contains("healthy") {
        Some(ManagedContainerHealthStatus::Healthy)
    } else if status.contains("health: starting") {
        Some(ManagedContainerHealthStatus::Starting)
    } else {
        None
    }
}

fn container_ip(
    network_settings: Option<ContainerSummaryNetworkSettings>,
) -> Result<Option<IpAddr>, DockerManagedContainerSummaryError> {
    let Some(network_settings) = network_settings else {
        return Ok(None);
    };
    let Some(networks) = network_settings.networks else {
        return Ok(None);
    };

    let Some(endpoint) = networks.get(ENDPOINT_NETWORK_NAME) else {
        return Ok(None);
    };
    let Some(ip) = endpoint
        .ip_address
        .as_ref()
        .or(endpoint.global_ipv6_address.as_ref())
        .filter(|ip| !ip.is_empty())
    else {
        return Ok(None);
    };

    Ok(Some(ip.parse::<IpAddr>().map_err(|_| {
        DockerManagedContainerSummaryError::InvalidEndpointIp {
            value: ip.to_owned(),
        }
    })?))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum DockerManagedContainerSummaryError {
    #[error("managed Docker container is missing id")]
    MissingId,
    #[error("managed Docker container is missing labels")]
    MissingLabels,
    #[error("managed Docker container is missing state")]
    MissingState,
    #[error("managed Docker container has invalid endpoint ip: {value}")]
    InvalidEndpointIp { value: String },
    #[error("managed Docker container has invalid id: {0}")]
    InvalidContainerId(SubjectTokenError),
    #[error("managed Docker container has invalid v2 labels: {0:?}")]
    InvalidV2Labels(V2ManagedContainerLabelError),
    #[error("managed Docker container is missing mounts")]
    MissingMounts,
    #[error("managed Docker container has invalid named-volume mounts: {0}")]
    InvalidNamedVolumeMounts(String),
}

fn hashmap_from_btree(map: BTreeMap<String, String>) -> HashMap<String, String> {
    map.into_iter().collect()
}

fn btree_from_hashmap(map: HashMap<String, String>) -> BTreeMap<String, String> {
    map.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roles::api::execution::docker::test_support::{
        TEST_ENDPOINT_SUBNET, image, recording_runner_with_responses, runner_with_responses,
    };
    use ployz_core::deploy::{
        ContainerCommand, ContainerEntrypoint, ContainerMountPath, ContainerRuntimeSpec, EnvName,
        EnvValue, ServiceEnvironment, ServiceVolumeMount, StopGracePeriod, VolumeName,
    };
    use std::sync::atomic::Ordering;

    fn log_container_id() -> ContainerId {
        ContainerId::try_new("log-container").expect("container id")
    }

    fn open_shutdown() -> (
        tokio::sync::watch::Sender<bool>,
        tokio::sync::watch::Receiver<bool>,
    ) {
        tokio::sync::watch::channel(false)
    }

    #[tokio::test]
    async fn v2_log_reader_preserves_streams_and_assembles_complete_lines() {
        let stream = futures_util::stream::iter([
            Ok(LogOutput::StdOut {
                message: bytes::Bytes::from_static(b"hel"),
            }),
            Ok(LogOutput::StdErr {
                message: bytes::Bytes::from_static(b"warning\r\n"),
            }),
            Ok(LogOutput::StdOut {
                message: bytes::Bytes::from_static(b"lo\nlast"),
            }),
        ]);

        let (_shutdown_send, shutdown_receive) = open_shutdown();
        let batch = collect_v2_log_stream(
            stream,
            &log_container_id(),
            10,
            1_024,
            Duration::from_secs(1),
            shutdown_receive,
        )
        .await
        .expect("log read");

        assert_eq!(
            batch,
            RuntimeLogBatch {
                lines: vec![
                    RuntimeLogLine {
                        stream: RuntimeLogStream::Stderr,
                        line: "warning".to_owned(),
                    },
                    RuntimeLogLine {
                        stream: RuntimeLogStream::Stdout,
                        line: "hello".to_owned(),
                    },
                    RuntimeLogLine {
                        stream: RuntimeLogStream::Stdout,
                        line: "last".to_owned(),
                    },
                ],
                truncated: false,
            }
        );
    }

    #[tokio::test]
    async fn v2_log_reader_caps_complete_lines_and_raw_bytes() {
        let lines = futures_util::stream::iter([Ok(LogOutput::StdOut {
            message: bytes::Bytes::from_static(b"one\ntwo\nthree\n"),
        })]);
        let (_line_shutdown_send, line_shutdown_receive) = open_shutdown();
        let line_capped = collect_v2_log_stream(
            lines,
            &log_container_id(),
            2,
            1_024,
            Duration::from_secs(1),
            line_shutdown_receive,
        )
        .await
        .expect("line-capped read");
        assert_eq!(
            line_capped
                .lines
                .iter()
                .map(|line| line.line.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert!(line_capped.truncated);

        let bytes = futures_util::stream::iter([Ok(LogOutput::StdOut {
            message: bytes::Bytes::from_static(b"1234\n5\n"),
        })]);
        let (_byte_shutdown_send, byte_shutdown_receive) = open_shutdown();
        let byte_capped = collect_v2_log_stream(
            bytes,
            &log_container_id(),
            10,
            4,
            Duration::from_secs(1),
            byte_shutdown_receive,
        )
        .await
        .expect("byte-capped read");
        assert_eq!(byte_capped.lines.len(), 1);
        assert_eq!(byte_capped.lines.first().expect("first line").line, "1234");
        assert!(byte_capped.truncated);
    }

    #[tokio::test(start_paused = true)]
    async fn v2_log_follow_emits_lines_immediately_and_stops_when_the_receiver_drops() {
        let follow_stream = futures_util::stream::iter([
            Ok(LogOutput::StdOut {
                message: bytes::Bytes::from_static(b"rea"),
            }),
            Ok(LogOutput::StdErr {
                message: bytes::Bytes::from_static(b"warning\r\n"),
            }),
            Ok(LogOutput::StdOut {
                message: bytes::Bytes::from_static(b"dy\n"),
            }),
        ])
        .chain(futures_util::stream::pending());
        let (follow_shutdown_send, follow_shutdown_receive) = open_shutdown();
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let task = tokio::spawn(async move {
            run_v2_log_follow_stream(
                follow_stream,
                log_container_id(),
                sender,
                follow_shutdown_receive,
            )
            .await
        });
        let _follow_shutdown_send = follow_shutdown_send;
        let mut follow = RuntimeLogFollow::new(receiver);
        assert_eq!(
            follow.next_line().await.expect("stderr line"),
            Some(RuntimeLogLine {
                stream: RuntimeLogStream::Stderr,
                line: "warning".to_owned(),
            })
        );
        assert_eq!(
            follow.next_line().await.expect("stdout line"),
            Some(RuntimeLogLine {
                stream: RuntimeLogStream::Stdout,
                line: "ready".to_owned(),
            })
        );
        drop(follow);
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("reader task observes receiver drop")
            .expect("reader task");
    }

    #[tokio::test]
    async fn v2_log_follow_distinguishes_stream_failure_from_clean_eof() {
        let error_stream =
            futures_util::stream::iter([Err(BollardError::DockerResponseServerError {
                status_code: 404,
                message: "secret-bearing daemon detail".to_owned(),
            })]);
        let (error_shutdown_send, error_shutdown_receive) = open_shutdown();
        let (error_sender, error_receiver) = tokio::sync::mpsc::channel(1);
        let error_task = tokio::spawn(run_v2_log_follow_stream(
            error_stream,
            log_container_id(),
            error_sender,
            error_shutdown_receive,
        ));
        let _error_shutdown_send = error_shutdown_send;
        let mut error_follow = RuntimeLogFollow::new(error_receiver);
        assert_eq!(
            error_follow.next_line().await,
            Err(V2MachineLogReadError::NotFound {
                container_id: log_container_id(),
            })
        );
        error_task.await.expect("error reader task");

        let eof_stream = futures_util::stream::iter([Ok(LogOutput::StdOut {
            message: bytes::Bytes::from_static(b"last line"),
        })]);
        let (eof_shutdown_send, eof_shutdown_receive) = open_shutdown();
        let (eof_sender, eof_receiver) = tokio::sync::mpsc::channel(1);
        let eof_task = tokio::spawn(run_v2_log_follow_stream(
            eof_stream,
            log_container_id(),
            eof_sender,
            eof_shutdown_receive,
        ));
        let _eof_shutdown_send = eof_shutdown_send;
        let mut eof_follow = RuntimeLogFollow::new(eof_receiver);
        assert_eq!(
            eof_follow.next_line().await.expect("final line"),
            Some(RuntimeLogLine {
                stream: RuntimeLogStream::Stdout,
                line: "last line".to_owned(),
            })
        );
        assert_eq!(eof_follow.next_line().await.expect("clean EOF"), None);
        eof_task.await.expect("EOF reader task");
    }

    #[tokio::test(start_paused = true)]
    async fn v2_log_tail_deadline_is_typed() {
        let (tail_shutdown_send, tail_shutdown_receive) = open_shutdown();
        let tail = tokio::spawn(async move {
            collect_v2_log_stream(
                futures_util::stream::pending(),
                &log_container_id(),
                10,
                1_024,
                Duration::from_secs(1),
                tail_shutdown_receive,
            )
            .await
        });
        let _tail_shutdown_send = tail_shutdown_send;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(
            tail.await.expect("tail task"),
            Err(V2MachineLogReadError::TimedOut)
        );
    }

    #[tokio::test]
    async fn v2_log_reader_classifies_cancellation_not_found_and_unknown_streams() {
        let (shutdown_send, shutdown_receive) = tokio::sync::watch::channel(false);
        shutdown_send.send(true).expect("shutdown");
        assert_eq!(
            collect_v2_log_stream(
                futures_util::stream::pending(),
                &log_container_id(),
                10,
                1_024,
                Duration::from_secs(1),
                shutdown_receive,
            )
            .await,
            Err(V2MachineLogReadError::Cancelled)
        );

        let not_found = BollardError::DockerResponseServerError {
            status_code: 404,
            message: "secret-bearing daemon detail".to_owned(),
        };
        assert_eq!(
            classify_v2_log_error(&log_container_id(), &not_found),
            V2MachineLogReadError::NotFound {
                container_id: log_container_id(),
            }
        );
        let read_failure = BollardError::DockerResponseServerError {
            status_code: 500,
            message: "secret-bearing daemon detail".to_owned(),
        };
        let classified = classify_v2_log_error(&log_container_id(), &read_failure);
        assert_eq!(classified, V2MachineLogReadError::ReadFailed);
        assert!(
            !classified
                .to_string()
                .contains("secret-bearing daemon detail")
        );
        assert_eq!(
            CompleteLogLineCollector::new(10, 1_024).push(LogOutput::Console {
                message: bytes::Bytes::from_static(b"ambiguous"),
            }),
            Err(V2MachineLogReadError::ReadFailed)
        );
    }

    #[test]
    fn v2_log_options_separate_tail_from_follow() {
        let tail = v2_logs_options(17, V2LogAttachMode::Tail);
        assert!(!tail.follow);
        assert_eq!(tail.tail.as_str(), "17");

        let follow = v2_logs_options(17, V2LogAttachMode::Follow);
        assert!(follow.follow);
        assert_eq!(follow.tail.as_str(), "17");
    }

    #[test]
    fn v2_list_options_filter_to_the_row_identity_schema() {
        let options = v2_managed_container_list_options();

        assert!(options.all);
        assert_eq!(
            options.filters,
            Some(HashMap::from([(
                "label".to_owned(),
                vec!["plz.identity_schema=corrosion_v2".to_owned()]
            )]))
        );
    }

    #[test]
    fn v2_create_body_uses_only_row_identity_labels_and_human_dns_namespace() {
        let body = create_v2_body(
            CreateV2ManagedContainer {
                image: image("ghcr.io/acme/api:rev-2"),
                runtime: ContainerRuntimeSpec::image_defaults(),
                dns_search_domain: dns_search_domain("production"),
                identity: v2_managed_identity(),
                host_ports: ployz_core::corrosion::HostPortBindings::default(),
            },
            TEST_ENDPOINT_SUBNET,
        );

        assert_eq!(body.image, Some("ghcr.io/acme/api:rev-2".to_owned()));
        assert_eq!(
            body.labels,
            Some(hashmap_from_btree(
                v2_labels::render(&v2_managed_identity())
            ))
        );
        assert_eq!(
            body.host_config.and_then(|config| config.dns_search),
            Some(vec!["production.internal".to_owned()])
        );
        // No published ports: the body stays byte-identical to the
        // pre-placement create.
        assert_eq!(body.exposed_ports, None);
    }

    #[test]
    fn v2_create_body_publishes_host_ports_only_when_bound() {
        let host_ports = ployz_core::corrosion::HostPortBindings::try_new([
            ployz_core::corrosion::HostPortBinding {
                host_port: std::num::NonZeroU16::new(8443).expect("port"),
                container_port: std::num::NonZeroU16::new(443).expect("port"),
                protocol: ployz_core::corrosion::HostPortProtocol::Tcp,
            },
            ployz_core::corrosion::HostPortBinding {
                host_port: std::num::NonZeroU16::new(5353).expect("port"),
                container_port: std::num::NonZeroU16::new(53).expect("port"),
                protocol: ployz_core::corrosion::HostPortProtocol::Udp,
            },
        ])
        .expect("host ports");
        let body = create_v2_body(
            CreateV2ManagedContainer {
                image: image("ghcr.io/acme/api:rev-2"),
                runtime: ContainerRuntimeSpec::image_defaults(),
                dns_search_domain: dns_search_domain("production"),
                identity: v2_managed_identity(),
                host_ports,
            },
            TEST_ENDPOINT_SUBNET,
        );

        let exposed = body.exposed_ports.expect("exposed ports");
        assert_eq!(exposed, vec!["443/tcp".to_owned(), "53/udp".to_owned()]);
        let bindings = body
            .host_config
            .and_then(|config| config.port_bindings)
            .expect("port bindings");
        let tcp = bindings
            .get("443/tcp")
            .cloned()
            .flatten()
            .expect("tcp binding");
        assert_eq!(
            tcp.iter()
                .map(|binding| binding.host_port.clone())
                .collect::<Vec<_>>(),
            vec![Some("8443".to_owned())]
        );
        let udp = bindings
            .get("53/udp")
            .cloned()
            .flatten()
            .expect("udp binding");
        assert_eq!(
            udp.iter()
                .map(|binding| binding.host_port.clone())
                .collect::<Vec<_>>(),
            vec![Some("5353".to_owned())]
        );
    }

    #[test]
    fn v2_create_body_preserves_runtime_and_uses_row_scoped_volume_names() {
        let mut runtime = runtime_spec();
        runtime.volume_mounts = vec![ServiceVolumeMount {
            volume_name: VolumeName::try_new("data").expect("volume"),
            target: ContainerMountPath::try_new("/srv/data").expect("mount path"),
        }];
        let body = create_v2_body(
            CreateV2ManagedContainer {
                image: image("ghcr.io/acme/api:rev-2"),
                runtime,
                dns_search_domain: dns_search_domain("production"),
                identity: v2_managed_identity(),
                host_ports: ployz_core::corrosion::HostPortBindings::default(),
            },
            TEST_ENDPOINT_SUBNET,
        );

        assert_eq!(
            body.env,
            Some(vec!["ALPHA=1".to_owned(), "BETA=two".to_owned()])
        );
        assert_eq!(body.cmd, Some(vec!["serve".to_owned(), "api".to_owned()]));
        let host = body.host_config.expect("host config");
        let mounts = host.mounts.expect("mounts");
        let [mount] = mounts.as_slice() else {
            panic!("one mount expected");
        };
        assert_eq!(
            mount.source.as_deref(),
            Some("ployz-v2-n26-01K00000000000000000000001-v4-data")
        );
    }

    #[tokio::test]
    async fn v2_create_requires_the_endpoint_network_and_returns_the_docker_id() {
        let network = r#"{"Driver":"bridge","Labels":{"plz.managed":"true"},"Options":{"com.docker.network.bridge.name":"br-test","com.docker.network.driver.mtu":"1420"},"IPAM":{"Config":[{"Subnet":"10.42.7.0/24","Gateway":"10.42.7.1"}]}}"#;
        let (runner, attempts, _socket_dir) = runner_with_responses(vec![
            (200, network.to_owned()),
            (201, r#"{"Id":"0123456789abcdef","Warnings":[]}"#.to_owned()),
        ])
        .await;

        let created = runner
            .create_v2_managed_container(CreateV2ManagedContainer {
                image: image("ghcr.io/acme/api:rev-2"),
                runtime: ContainerRuntimeSpec::image_defaults(),
                dns_search_domain: dns_search_domain("production"),
                identity: v2_managed_identity(),
                host_ports: ployz_core::corrosion::HostPortBindings::default(),
            })
            .await
            .expect("v2 create succeeds");

        assert_eq!(created, container_id("0123456789abcdef"));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn v2_container_ids_are_directly_startable() {
        let (runner, attempts, _socket_dir) =
            runner_with_responses(vec![(204, String::new())]).await;

        runner
            .start_v2_managed_container(&container_id("0123456789abcdef"))
            .await
            .expect("v2 start succeeds");

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    fn v2_labels_json() -> String {
        serde_json::to_string(&v2_labels::render(&v2_managed_identity()))
            .expect("v2 labels serialize")
    }

    #[tokio::test]
    async fn v2_stop_defers_to_the_containers_configured_stop_timeout() {
        let inspect = format!(
            r#"{{"Id":"0123456789abcdef","Config":{{"Labels":{}}},"State":{{"Running":true}}}}"#,
            v2_labels_json()
        );
        let (runner, requests, _socket_dir) =
            recording_runner_with_responses(vec![(200, inspect), (204, String::new())]).await;

        let outcome = runner
            .stop_v2_managed_container(&container_id("0123456789abcdef"), &v2_managed_identity())
            .await
            .expect("v2 stop succeeds");

        assert_eq!(outcome, MachineContainerStopOutcome::StoppedRunning);
        let requests = requests.lock().expect("recorded Docker requests");
        let [inspect_request, stop_request] = requests.as_slice() else {
            panic!("expected inspect then stop, got {requests:?}");
        };
        assert!(
            inspect_request.contains("/containers/0123456789abcdef/json"),
            "{inspect_request}"
        );
        let stop_line = stop_request.lines().next().expect("stop request line");
        assert!(stop_line.starts_with("POST "), "{stop_line}");
        assert!(
            stop_line.contains("/containers/0123456789abcdef/stop"),
            "{stop_line}"
        );
        assert!(
            !stop_line.contains("t="),
            "stop must carry no timeout override so Docker honors the \
             container's configured StopTimeout: {stop_line}"
        );
    }

    #[tokio::test]
    async fn v2_stop_reports_an_already_stopped_container_without_a_stop_call() {
        let inspect = format!(
            r#"{{"Id":"0123456789abcdef","Config":{{"Labels":{}}},"State":{{"Running":false}}}}"#,
            v2_labels_json()
        );
        let (runner, requests, _socket_dir) =
            recording_runner_with_responses(vec![(200, inspect)]).await;

        let outcome = runner
            .stop_v2_managed_container(&container_id("0123456789abcdef"), &v2_managed_identity())
            .await
            .expect("v2 stop succeeds");

        assert_eq!(outcome, MachineContainerStopOutcome::AlreadyStopped);
        let requests = requests.lock().expect("recorded Docker requests");
        assert_eq!(requests.len(), 1, "{requests:?}");
    }

    #[tokio::test]
    async fn v2_remove_forces_removal_but_never_removes_volumes() {
        let list = format!(
            r#"[{{"Id":"0123456789abcdef","Labels":{},"State":"exited","Mounts":[]}}]"#,
            v2_labels_json()
        );
        let (runner, requests, _socket_dir) =
            recording_runner_with_responses(vec![(200, list), (204, String::new())]).await;

        runner
            .remove_v2_managed_container(&container_id("0123456789abcdef"), &v2_managed_identity())
            .await
            .expect("v2 remove succeeds");

        let requests = requests.lock().expect("recorded Docker requests");
        let [_list_request, remove_request] = requests.as_slice() else {
            panic!("expected list then remove, got {requests:?}");
        };
        assert!(remove_request.starts_with("DELETE "), "{remove_request}");
        assert!(remove_request.contains("v=false"), "{remove_request}");
        assert!(remove_request.contains("force=true"), "{remove_request}");
    }

    #[tokio::test]
    async fn v2_remove_of_a_mismatched_identity_refuses_without_a_delete_call() {
        let list = format!(
            r#"[{{"Id":"0123456789abcdef","Labels":{},"State":"exited","Mounts":[]}}]"#,
            v2_labels_json()
        );
        let (runner, requests, _socket_dir) =
            recording_runner_with_responses(vec![(200, list)]).await;
        let mut wrong = v2_managed_identity();
        wrong.operation_id = ployz_core::ids::OperationRowId::try_new("01K00000000000000000000009")
            .expect("operation row");

        let error = runner
            .remove_v2_managed_container(&container_id("0123456789abcdef"), &wrong)
            .await
            .expect_err("mismatched identity refuses removal");

        assert!(matches!(error, MachineContainerRemoveError::Remove { .. }));
        let requests = requests.lock().expect("recorded Docker requests");
        assert_eq!(requests.len(), 1, "{requests:?}");
    }

    #[tokio::test]
    async fn projection_network_rejects_configured_subnet_before_touching_docker() {
        let runner = DockerManagedContainerRunner::lazy_local_defaults(
            "10.198.1.0/24".to_owned(),
            "br-ployz".to_owned(),
            "ployz-wg0".to_owned(),
            WireGuardMtuPolicy::Auto,
        );
        let expected = MachineEndpointSubnet::try_new("10.198.2.0/24").expect("subnet");

        assert!(matches!(
            runner.ensure_projection_endpoint_network(&expected).await,
            Err(MachineEndpointNetworkError::EndpointNetworkSubnetMismatch { .. })
        ));
    }

    #[test]
    fn v2_summary_recovers_the_row_only_identity() {
        let summary = ContainerSummary {
            id: Some("0123456789abcdef".to_owned()),
            labels: Some(hashmap_from_btree(
                v2_labels::render(&v2_managed_identity()),
            )),
            state: Some(ContainerSummaryStateEnum::CREATED),
            image_id: Some("sha256:resolved".to_owned()),
            created: Some(42),
            mounts: Some(Vec::new()),
            ..Default::default()
        };

        assert_eq!(
            existing_v2_container_from_summary(summary).expect("summary parses"),
            ExistingV2ManagedContainer {
                container_id: container_id("0123456789abcdef"),
                identity: v2_managed_identity(),
                state: ExistingManagedContainerState::StartableStopped,
                health_status: None,
                resolved_image_identity: Some("sha256:resolved".to_owned()),
                created_at_unix_seconds: Some(42),
                named_volume_names: BTreeSet::new(),
            }
        );
    }

    #[test]
    fn v2_summary_maps_mounts_to_named_volumes_and_surfaces_running_state() {
        let identity = v2_managed_identity();
        let data = VolumeName::try_new("data").expect("volume");
        let logs = VolumeName::try_new("logs").expect("volume");
        let summary = ContainerSummary {
            id: Some("0123456789abcdef".to_owned()),
            labels: Some(hashmap_from_btree(v2_labels::render(&identity))),
            state: Some(ContainerSummaryStateEnum::RUNNING),
            mounts: Some(vec![
                MountPoint {
                    typ: Some("volume".to_owned()),
                    name: Some(v2_volume_storage_name(&identity.namespace_id, &logs)),
                    ..Default::default()
                },
                MountPoint {
                    typ: Some("bind".to_owned()),
                    name: Some("host-path".to_owned()),
                    ..Default::default()
                },
                MountPoint {
                    typ: Some("volume".to_owned()),
                    name: Some(v2_volume_storage_name(&identity.namespace_id, &data)),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };

        let existing = existing_v2_container_from_summary(summary).expect("summary parses");

        assert_eq!(existing.named_volume_names, BTreeSet::from([data, logs]));
        assert!(matches!(
            existing.state,
            ExistingManagedContainerState::Running { .. }
        ));
    }

    #[test]
    fn v2_summary_without_mounts_is_not_empty_volume_testimony() {
        let summary = ContainerSummary {
            id: Some("0123456789abcdef".to_owned()),
            labels: Some(hashmap_from_btree(
                v2_labels::render(&v2_managed_identity()),
            )),
            state: Some(ContainerSummaryStateEnum::RUNNING),
            ..Default::default()
        };

        assert_eq!(
            existing_v2_container_from_summary(summary),
            Err(DockerManagedContainerSummaryError::MissingMounts)
        );
    }

    #[test]
    fn v2_named_volume_testimony_rejects_legacy_and_cross_namespace_storage_names() {
        let identity = v2_managed_identity();
        let volume = VolumeName::try_new("data").expect("volume");
        let other_namespace =
            ployz_core::ids::NamespaceRowId::try_new("01K00000000000000000000009")
                .expect("namespace row");

        for storage_name in [
            "data".to_owned(),
            "ployz-n7-default-v4-data".to_owned(),
            v2_volume_storage_name(&other_namespace, &volume),
        ] {
            let error = v2_named_volume_names_from_mounts(
                &[MountPoint {
                    typ: Some("volume".to_owned()),
                    name: Some(storage_name.clone()),
                    ..Default::default()
                }],
                &identity.namespace_id,
            )
            .expect_err("invalid physical identity must fail testimony");
            assert!(error.contains(&storage_name), "{error}");
        }
    }

    #[test]
    fn v2_volume_storage_names_round_trip_only_for_their_namespace_row() {
        let identity = v2_managed_identity();
        let volume = VolumeName::try_new("pg-data").expect("volume");
        let storage_name = v2_volume_storage_name(&identity.namespace_id, &volume);

        assert_eq!(
            v2_volume_name_from_storage_name(&storage_name, &identity.namespace_id),
            Ok(volume)
        );
    }

    fn v2_managed_identity() -> V2ManagedContainerIdentity {
        V2ManagedContainerIdentity {
            namespace_id: ployz_core::ids::NamespaceRowId::try_new("01K00000000000000000000001")
                .expect("namespace row"),
            service_id: ployz_core::ids::ServiceRowId::try_new("01K00000000000000000000002")
                .expect("service row"),
            operation_id: ployz_core::ids::OperationRowId::try_new("01K00000000000000000000003")
                .expect("operation row"),
        }
    }

    fn dns_search_domain(
        namespace: &str,
    ) -> ployz_core::network::internal_dns::InternalDnsSearchDomain {
        ployz_core::network::internal_dns::InternalDnsSearchDomain::try_from_namespace_label(
            namespace,
        )
        .expect("valid human namespace label")
    }

    fn container_id(value: &str) -> ContainerId {
        ContainerId::try_new(value).expect("valid container id")
    }

    fn runtime_spec() -> ContainerRuntimeSpec {
        let mut runtime = ContainerRuntimeSpec::image_defaults();
        runtime.command = Some(
            ContainerCommand::try_new(vec!["serve".to_owned(), "api".to_owned()])
                .expect("valid command"),
        );
        runtime.entrypoint = Some(ContainerEntrypoint::Argv(
            ContainerCommand::try_new(vec!["/init".to_owned()]).expect("valid entrypoint"),
        ));
        runtime.environment = ServiceEnvironment::from(BTreeMap::from([
            (
                EnvName::try_new("BETA").expect("valid env name"),
                EnvValue::try_new("two").expect("valid env value"),
            ),
            (
                EnvName::try_new("ALPHA").expect("valid env name"),
                EnvValue::try_new("1").expect("valid env value"),
            ),
        ]));
        runtime.stop_grace_period = StopGracePeriod::from(30);
        runtime
    }
}
