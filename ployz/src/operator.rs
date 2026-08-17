use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::{BinaryHeap, HashSet},
    future::Future,
    pin::Pin,
    time::Duration,
};

use futures_util::{Stream, StreamExt, stream};
use ployz_core::{
    ContainerId, ContainerLogsRequest, ContainerRef, ContainerSelector, ExecConfig, ExecOptions,
    ExecRequestFrame, FanoutSelector, HealthObservation, LogEntry, LogStream, LogsOptions,
    MachineId, MachineLogService, MachineLogsRequest, MachineName, MachineObservation,
    MachineTarget, OpaquePayload, ServiceContainer, ServiceObservation, ServiceSelector,
    StreamProtocolError, op, resolve_container_selector, resolve_machine_selectors, select_service,
};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tonic::Streaming;

use crate::connect::{Client, ConnectError, TransportError};

pub const DEFAULT_EXEC_COMMAND: &[&str] = &[
    "sh",
    "-c",
    "command -v bash >/dev/null 2>&1 && exec bash || exec sh",
];

const LOG_STALL_TIMEOUT: Duration = Duration::from_secs(10);
const LOG_STALL_CHECK: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum LogError {
    #[error("{0}")]
    Transport(Box<tonic::Status>),
    #[error("{0}")]
    Protocol(#[from] StreamProtocolError),
    #[error("{0}")]
    Message(Cow<'static, str>),
}

impl From<tonic::Status> for LogError {
    fn from(status: tonic::Status) -> Self {
        Self::Transport(Box::new(status))
    }
}

pub type LogSource = Pin<Box<dyn Stream<Item = Result<LogEntry, LogError>> + Send>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceArg {
    pub service: ServiceSelector,
    pub containers: Vec<ContainerSelector>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProxyPorts {
    pub local: u16,
    pub remote: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecMode {
    pub detach: bool,
    pub no_tty: bool,
    pub stdout_terminal: bool,
    pub stdin_terminal: bool,
}

#[derive(Debug, Error)]
pub enum OperatorError {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Selector(#[from] ployz_core::ServiceSelectorError),
    #[error(transparent)]
    MachineSelector(#[from] ployz_core::MachineSelectorError),
    #[error(transparent)]
    Value(#[from] ployz_core::ValueError),
    #[error(transparent)]
    Container(#[from] ployz_core::ContainerSelectorError),
    #[error("Service has no regular containers")]
    NoRegularContainer,
    #[error("Machine RPC failed: {0}")]
    Rpc(TransportError),
    #[error("stream protocol failed: {0}")]
    Protocol(#[from] StreamProtocolError),
    #[error(transparent)]
    Codec(#[from] ployz_core::CodecError),
    #[error("exec request stream closed")]
    StreamClosed,
    #[error("cannot attach a terminal exec to non-terminal stdin; use -T")]
    TtyRequiresStdin,
    #[error("invalid Service log selector {0:?}")]
    InvalidServiceSelector(String),
    #[error("invalid log tail {0:?}: expected a non-negative integer or all")]
    InvalidTail(String),
    #[error("invalid proxy port: expected [LOCAL_PORT:]REMOTE_PORT")]
    InvalidProxyPort,
    #[error("invalid local port {0:?}: expected 0-65535")]
    InvalidLocalPort(String),
    #[error("invalid remote port {0:?}: expected 1-65535")]
    InvalidRemotePort(String),
    #[error("no running healthy regular container found for Service")]
    NoHealthyContainer,
    #[error("no containers for Service \"{service}\" found on the selected Machines")]
    NoContainersOnMachines { service: ServiceSelector },
    #[error("none of the selected Services exist in the Cluster")]
    NoSelectedServices,
    #[error("no Machines found")]
    NoMachines,
    #[error("selected Machine disappeared from the snapshot")]
    SnapshotStale,
    #[error("unsupported Machine log service {service:?}; {expected}")]
    UnsupportedLogService {
        service: String,
        expected: &'static str,
    },
    #[error("open logs for Container {container_id} on Machine {machine_id}: {source}")]
    OpenContainerLogs {
        container_id: ContainerId,
        machine_id: MachineId,
        #[source]
        source: Box<OperatorError>,
    },
    #[error("open {service} logs on Machine {machine_name}: {source}")]
    OpenMachineLogs {
        service: MachineLogService,
        machine_name: MachineName,
        #[source]
        source: Box<OperatorError>,
    },
}

impl From<tonic::Status> for OperatorError {
    fn from(status: tonic::Status) -> Self {
        Self::Rpc(TransportError::from(status))
    }
}

impl From<TransportError> for OperatorError {
    fn from(error: TransportError) -> Self {
        Self::Rpc(error)
    }
}

pub fn exec_options(command: Vec<String>, mode: ExecMode) -> Result<ExecOptions, OperatorError> {
    // TODO(UT-039): preserve the Compose-style stdout-driven TTY rule.
    let tty = !mode.detach && mode.stdout_terminal && !mode.no_tty;
    if tty && !mode.stdin_terminal {
        return Err(OperatorError::TtyRequiresStdin);
    }
    Ok(ExecOptions {
        command: if command.is_empty() {
            DEFAULT_EXEC_COMMAND
                .iter()
                .map(ToString::to_string)
                .collect()
        } else {
            command
        },
        attach_stdin: !mode.detach,
        attach_stdout: !mode.detach,
        attach_stderr: !mode.detach,
        tty,
        detach: mode.detach,
    })
}

pub fn select_exec_container<'a>(
    service: &'a ServiceObservation,
    selector: Option<&ContainerSelector>,
) -> Result<&'a ServiceContainer, OperatorError> {
    match selector {
        None => service
            .containers
            .first()
            .ok_or(OperatorError::NoRegularContainer),
        Some(selector) => Ok(resolve_container_selector(
            service.containers.iter(),
            selector,
        )?),
    }
}

pub fn parse_service_args(values: &[String]) -> Result<Vec<ServiceArg>, OperatorError> {
    let mut parsed: Vec<ServiceArg> = Vec::new();
    for value in values {
        let value = value.trim();
        let (service, container) = value
            .split_once('/')
            .map_or((value, None), |(service, container)| {
                (service, Some(container))
            });
        if service.is_empty() || container.is_some_and(str::is_empty) {
            return Err(OperatorError::InvalidServiceSelector(value.to_owned()));
        }
        let service = ServiceSelector::parse(service)?;
        let container = container.map(ContainerSelector::parse).transpose()?;
        if let Some(existing) = parsed.iter_mut().find(|arg| arg.service == service) {
            match container {
                None => existing.containers.clear(),
                Some(container) if !existing.containers.is_empty() => {
                    if !existing.containers.iter().any(|value| value == &container) {
                        existing.containers.push(container);
                    }
                }
                Some(_) => {}
            }
        } else {
            parsed.push(ServiceArg {
                service,
                containers: container.into_iter().collect(),
            });
        }
    }
    Ok(parsed)
}

pub fn parse_tail(value: &str) -> Result<i32, OperatorError> {
    if value == "all" {
        return Ok(-1);
    }
    value
        .parse::<i32>()
        .ok()
        .filter(|tail| *tail >= 0)
        .ok_or_else(|| OperatorError::InvalidTail(value.to_owned()))
}

#[must_use]
pub fn service_logs_use_compose(explicit: &[String]) -> bool {
    explicit.is_empty()
}

pub fn parse_proxy_ports(value: &str) -> Result<ProxyPorts, OperatorError> {
    let (local, remote) = value.split_once(':').map_or(("0", value), |parts| parts);
    if remote.contains(':') {
        return Err(OperatorError::InvalidProxyPort);
    }
    let local = local
        .parse::<u16>()
        .map_err(|_| OperatorError::InvalidLocalPort(local.to_owned()))?;
    let remote = remote
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| OperatorError::InvalidRemotePort(remote.to_owned()))?;
    Ok(ProxyPorts { local, remote })
}

pub fn select_proxy_container(
    service: &ServiceObservation,
) -> Result<&ServiceContainer, OperatorError> {
    service
        .containers
        .iter()
        .find(|container| {
            matches!(
                container.as_observation().runtime,
                ployz_core::ContainerRuntimeObservation::Running {
                    health: HealthObservation::Healthy | HealthObservation::NotConfigured
                }
            )
        })
        .ok_or(OperatorError::NoHealthyContainer)
}

pub struct ExecSession {
    pub input: mpsc::Sender<OpaquePayload>,
    pub output: Streaming<OpaquePayload>,
}

pub struct LogInput {
    pub identity: String,
    pub stream: LogSource,
}

pub struct ServiceLogInputs {
    pub inputs: Vec<LogInput>,
    pub skipped_services: Vec<ServiceSelector>,
}

pub async fn open_exec(
    client: &mut Client,
    service_selector: &ServiceSelector,
    container_selector: Option<&ContainerSelector>,
    options: ExecOptions,
) -> Result<ExecSession, OperatorError> {
    let live = client.live_services().await?;
    let service = select_service(&live.services, service_selector)?;
    let container = select_exec_container(service, container_selector)?.as_observation();
    let machine_id = container.machine_id;
    let config = ExecRequestFrame::Config(ExecConfig {
        container_id: container.container_id,
        options,
    })
    .encode()?;
    let (sender, receiver) = mpsc::channel(32);
    sender
        .send(config)
        .await
        .map_err(|_| OperatorError::StreamClosed)?;
    let output = client
        .exec_stream(
            &MachineTarget::from(&machine_id),
            tokio_stream::wrappers::ReceiverStream::new(receiver),
        )
        .await?;
    Ok(ExecSession {
        input: sender,
        output,
    })
}

pub async fn open_service_logs(
    client: &mut Client,
    args: &[ServiceArg],
    machine_selectors: &[FanoutSelector],
    options: LogsOptions,
    compose_selection: bool,
    cancellation: CancellationToken,
) -> Result<ServiceLogInputs, OperatorError> {
    let machines = client.machines().await?;
    let selected_machines = select_machines(&machines, machine_selectors)?;
    let machine_ids = selected_machines
        .iter()
        .map(|machine| machine.machine.id)
        .collect::<HashSet<_>>();
    let live = client.live_services().await?;
    let mut inputs = Vec::new();
    let mut skipped_services = Vec::new();
    for arg in args {
        let service = match select_service(&live.services, &arg.service) {
            Ok(service) => service,
            Err(ployz_core::ServiceSelectorError::NotFound { .. }) if compose_selection => {
                skipped_services.push(arg.service.clone());
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let containers = select_log_containers(service, &arg.containers)?;
        let containers = containers
            .into_iter()
            .filter(|container| machine_ids.contains(&container.as_observation().machine_id))
            .collect::<Vec<_>>();
        if containers.is_empty() {
            return Err(OperatorError::NoContainersOnMachines {
                service: arg.service.clone(),
            });
        }
        for container in containers {
            let observation = container.as_observation();
            let request = op::ContainerLogs::into_request(ContainerLogsRequest {
                container_id: observation.container_id,
                options: options.clone(),
            })
            .encode()?;
            let identity = format!("{}/{}", arg.service, observation.display_name);
            let target = MachineTarget::from(&observation.machine_id);
            // TODO(UT-082): earlier Container log streams intentionally survive until the
            // parent cancellation token is cancelled.
            if let Err(error) = open_log_input(&mut inputs, &cancellation, async {
                client
                    .container_logs_stream(&target, request)
                    .await
                    .map(|stream| stream_input(identity, stream))
            })
            .await
            {
                return Err(OperatorError::OpenContainerLogs {
                    container_id: observation.container_id,
                    machine_id: observation.machine_id,
                    source: Box::new(error.into()),
                });
            }
        }
    }
    if inputs.is_empty() {
        return Err(OperatorError::NoSelectedServices);
    }
    Ok(ServiceLogInputs {
        inputs,
        skipped_services,
    })
}

pub async fn open_machine_logs(
    client: &mut Client,
    services: &[String],
    machine_selectors: &[FanoutSelector],
    options: LogsOptions,
    cancellation: CancellationToken,
) -> Result<Vec<LogInput>, OperatorError> {
    let services = if services.is_empty() {
        vec![MachineLogService::Ployz]
    } else {
        services
            .iter()
            .map(|service| {
                service.parse::<MachineLogService>().map_err(|expected| {
                    OperatorError::UnsupportedLogService {
                        service: service.clone(),
                        expected,
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    let machines = client.machines().await?;
    let machines = select_machines(&machines, machine_selectors)?;
    let mut inputs = Vec::new();
    for service in services {
        for machine in &machines {
            let request = op::MachineLogs::into_request(MachineLogsRequest {
                service,
                options: options.clone(),
            })
            .encode()?;
            let identity = format!("{service}@{}", machine.machine.name);
            let target = MachineTarget::from(&machine.machine.id);
            // TODO(UT-083): earlier Machine log streams intentionally survive until the
            // parent cancellation token is cancelled.
            if let Err(error) = open_log_input(&mut inputs, &cancellation, async {
                client
                    .machine_logs_stream(&target, request)
                    .await
                    .map(|stream| stream_input(identity, stream))
            })
            .await
            {
                return Err(OperatorError::OpenMachineLogs {
                    service,
                    machine_name: machine.machine.name.clone(),
                    source: Box::new(error.into()),
                });
            }
        }
    }
    Ok(inputs)
}

pub(crate) fn stream_input(identity: String, stream: Streaming<OpaquePayload>) -> LogInput {
    LogInput {
        identity,
        stream: Box::pin(stream.map(|result| {
            result
                .map_err(LogError::from)
                .and_then(|payload| LogEntry::decode(&payload).map_err(LogError::from))
        })),
    }
}

fn preserve_open_inputs(inputs: Vec<LogInput>, cancellation: CancellationToken) {
    for mut input in inputs {
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = cancellation.cancelled() => return,
                    entry = input.stream.next() => if entry.is_none() { return },
                }
            }
        });
    }
}

pub(crate) async fn open_log_input(
    inputs: &mut Vec<LogInput>,
    cancellation: &CancellationToken,
    open: impl Future<Output = Result<LogInput, TransportError>>,
) -> Result<(), TransportError> {
    match open.await {
        Ok(input) => {
            inputs.push(input);
            Ok(())
        }
        Err(error) => {
            preserve_open_inputs(std::mem::take(inputs), cancellation.clone());
            Err(error)
        }
    }
}

pub(crate) fn select_log_containers<'a>(
    service: &'a ServiceObservation,
    selectors: &[ContainerSelector],
) -> Result<Vec<ContainerRef<'a>>, OperatorError> {
    let all = service.members().collect::<Vec<_>>();
    if selectors.is_empty() {
        return Ok(all);
    }
    let mut selected = Vec::new();
    for selector in selectors {
        let container = *resolve_container_selector(all.iter(), selector)?;
        if !selected.iter().any(|selected: &ContainerRef<'a>| {
            selected.as_observation().container_id == container.as_observation().container_id
        }) {
            selected.push(container);
        }
    }
    Ok(selected)
}

pub(crate) fn select_machines<'a>(
    machines: &'a [MachineObservation],
    selectors: &[FanoutSelector],
) -> Result<Vec<&'a MachineObservation>, OperatorError> {
    let eligible = machines
        .iter()
        .filter(|machine| machine.membership.invites_rpc())
        .collect::<Vec<_>>();
    if selectors.is_empty() {
        if eligible.is_empty() {
            return Err(OperatorError::NoMachines);
        }
        return Ok(eligible);
    }
    let visible = eligible
        .iter()
        .map(|observation| observation.machine.clone())
        .collect::<Vec<_>>();
    resolve_machine_selectors(&visible, selectors)?
        .into_iter()
        .map(|machine| {
            eligible
                .iter()
                .copied()
                .find(|observation| observation.machine.id == machine.id)
                .ok_or(OperatorError::SnapshotStale)
        })
        .collect()
}

struct LogEvent {
    index: usize,
    entry: Option<Result<LogEntry, LogError>>,
}

struct LogState {
    identity: String,
    watermark: i64,
    last_activity: tokio::time::Instant,
    closed: bool,
    stalled: bool,
}

struct QueuedLog {
    timestamp: i64,
    sequence: u64,
    entry: LogEntry,
}

impl Eq for QueuedLog {}

impl PartialEq for QueuedLog {
    fn eq(&self, other: &Self) -> bool {
        self.timestamp == other.timestamp && self.sequence == other.sequence
    }
}

impl Ord for QueuedLog {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .timestamp
            .cmp(&self.timestamp)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for QueuedLog {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[must_use]
pub fn merge_logs(
    inputs: Vec<LogInput>,
    cancellation: CancellationToken,
) -> mpsc::Receiver<Result<LogEntry, String>> {
    merge_logs_with_options(inputs, cancellation, LOG_STALL_TIMEOUT, LOG_STALL_CHECK)
}

fn merge_logs_with_options(
    inputs: Vec<LogInput>,
    cancellation: CancellationToken,
    stall_timeout: Duration,
    stall_check: Duration,
) -> mpsc::Receiver<Result<LogEntry, String>> {
    let (output_sender, output) = mpsc::channel(100);
    if inputs.is_empty() {
        return output;
    }
    let now = tokio::time::Instant::now();
    let mut states = Vec::with_capacity(inputs.len());
    let mut events: stream::SelectAll<Pin<Box<dyn Stream<Item = LogEvent> + Send>>> =
        stream::select_all(Vec::new());
    for (index, input) in inputs.into_iter().enumerate() {
        states.push(LogState {
            identity: input.identity,
            watermark: 0,
            last_activity: now,
            closed: false,
            stalled: false,
        });
        events.push(Box::pin(
            input
                .stream
                .map(move |entry| LogEvent {
                    index,
                    entry: Some(entry),
                })
                .chain(stream::once(async move { LogEvent { index, entry: None } })),
        ));
    }
    tokio::spawn(async move {
        let mut queue = BinaryHeap::new();
        let mut sequence = 0_u64;
        let mut ticker = tokio::time::interval(stall_check);
        ticker.tick().await;
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return,
                event = events.next() => match event {
                    Some(LogEvent { index, entry: Some(entry) }) => {
                        let Some(state) = states.get_mut(index) else { continue };
                        state.last_activity = tokio::time::Instant::now();
                        state.stalled = false;
                        match entry {
                            Err(error) => if output_sender.send(Err(format!("{}: {error}", state.identity))).await.is_err() { return },
                            Ok(entry) if entry.stream == LogStream::Error => {
                                let error = entry.error.clone().unwrap_or_else(|| "log stream failed".into());
                                if output_sender.send(Err(format!("{}: {error}", state.identity))).await.is_err() { return }
                            }
                            Ok(entry) => {
                                state.watermark = state.watermark.max(entry.timestamp_unix_nanos);
                                if matches!(entry.stream, LogStream::Stdout | LogStream::Stderr) {
                                    if entry.timestamp_unix_nanos == 0 {
                                        if output_sender.send(Ok(entry)).await.is_err() { return }
                                    } else {
                                        queue.push(QueuedLog { timestamp: entry.timestamp_unix_nanos, sequence, entry });
                                        sequence = sequence.wrapping_add(1);
                                    }
                                }
                            }
                        }
                        if flush_ready(&states, &mut queue, &output_sender).await.is_err() { return }
                    }
                    Some(LogEvent { index, entry: None }) => {
                        if let Some(state) = states.get_mut(index) { state.closed = true; }
                        if flush_ready(&states, &mut queue, &output_sender).await.is_err() { return }
                        if states.iter().all(|state| state.closed) {
                            while let Some(queued) = queue.pop() {
                                if output_sender.send(Ok(queued.entry)).await.is_err() { return }
                            }
                            return;
                        }
                    }
                    None => return,
                },
                now = ticker.tick() => {
                    for state in &mut states {
                        if !state.closed && !state.stalled && now.duration_since(state.last_activity) > stall_timeout {
                            state.stalled = true;
                            if output_sender.send(Err(format!("log stream {} stalled", state.identity))).await.is_err() { return }
                        }
                    }
                    if flush_ready(&states, &mut queue, &output_sender).await.is_err() { return }
                }
            }
        }
    });
    output
}

async fn flush_ready(
    states: &[LogState],
    queue: &mut BinaryHeap<QueuedLog>,
    output: &mpsc::Sender<Result<LogEntry, String>>,
) -> Result<(), ()> {
    let watermark = states
        .iter()
        .filter(|state| !state.closed && !state.stalled)
        .map(|state| state.watermark)
        .min();
    let Some(watermark) = watermark else {
        while let Some(queued) = queue.pop() {
            output.send(Ok(queued.entry)).await.map_err(|_| ())?;
        }
        return Ok(());
    };
    if watermark == 0 {
        return Ok(());
    }
    while queue
        .peek()
        .is_some_and(|entry| entry.timestamp <= watermark)
    {
        let entry = queue.pop().expect("queue was checked above").entry;
        output.send(Ok(entry)).await.map_err(|_| ())?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "operator_tests.rs"]
mod tests;
