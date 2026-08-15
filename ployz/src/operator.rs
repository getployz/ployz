use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashSet},
    future::Future,
    pin::Pin,
    time::Duration,
};

use futures_util::{Stream, StreamExt, stream};
use ployz_core::{
    ContainerId, ContainerKind, ContainerLogsRequest, ContainerObservation, ExecConfig,
    ExecOptions, ExecRequestFrame, HealthObservation, ListMachinesRequest, LogEntry, LogStream,
    LogsOptions, MachineLogService, MachineLogsRequest, MachineObservation, MachineSelector,
    MembershipObservation, OpaquePayload, ServiceObservation, op, resolve_machine_selectors,
    select_service,
};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tonic::Streaming;

use crate::connect::Client;

pub const DEFAULT_EXEC_COMMAND: &[&str] = &[
    "sh",
    "-c",
    "command -v bash >/dev/null 2>&1 && exec bash || exec sh",
];

const LOG_STALL_TIMEOUT: Duration = Duration::from_secs(10);
const LOG_STALL_CHECK: Duration = Duration::from_secs(1);

pub type LogSource = Pin<Box<dyn Stream<Item = Result<LogEntry, String>> + Send>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceArg {
    pub service: String,
    pub containers: Vec<String>,
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

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ContainerSelectorError {
    #[error("Service has no regular containers")]
    NoRegularContainer,
    #[error("Container {selector:?} was not found in the Service")]
    NotFound { selector: String },
    #[error("Container {selector:?} matches multiple containers: {container_ids:?}")]
    Ambiguous {
        selector: String,
        container_ids: Vec<ContainerId>,
    },
}

#[derive(Debug, Error)]
pub enum OperatorError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Container(#[from] ContainerSelectorError),
    #[error("Machine RPC failed: {0}")]
    Rpc(Box<tonic::Status>),
    #[error("stream protocol failed: {0}")]
    Protocol(#[from] ployz_core::StreamProtocolError),
}

impl From<String> for OperatorError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for OperatorError {
    fn from(message: &str) -> Self {
        Self::Message(message.to_owned())
    }
}

impl From<tonic::Status> for OperatorError {
    fn from(status: tonic::Status) -> Self {
        Self::Rpc(Box::new(status))
    }
}

pub fn exec_options(command: Vec<String>, mode: ExecMode) -> Result<ExecOptions, OperatorError> {
    // TODO(UT-039): preserve the Compose-style stdout-driven TTY rule.
    let tty = !mode.detach && mode.stdout_terminal && !mode.no_tty;
    if tty && !mode.stdin_terminal {
        return Err("cannot attach a terminal exec to non-terminal stdin; use -T".into());
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
    selector: Option<&str>,
) -> Result<&'a ContainerObservation, ContainerSelectorError> {
    let regular = service
        .containers
        .iter()
        .filter(|container| container.kind == ContainerKind::ServiceContainer)
        .collect::<Vec<_>>();
    let Some(selector) = selector.filter(|selector| !selector.is_empty()) else {
        return regular
            .into_iter()
            .next()
            .ok_or(ContainerSelectorError::NoRegularContainer);
    };
    resolve_container_selector(&regular, selector)
}

fn resolve_container_selector<'a>(
    containers: &[&'a ContainerObservation],
    selector: &str,
) -> Result<&'a ContainerObservation, ContainerSelectorError> {
    if let Some(container) = containers
        .iter()
        .copied()
        .find(|container| container.container_id.as_str() == selector)
    {
        return Ok(container);
    }
    let named = containers
        .iter()
        .copied()
        .filter(|container| container.display_name == selector)
        .collect::<Vec<_>>();
    match named.as_slice() {
        [container] => return Ok(container),
        [] => {}
        _ => {
            return Err(ContainerSelectorError::Ambiguous {
                selector: selector.to_owned(),
                container_ids: named
                    .into_iter()
                    .map(|container| container.container_id.clone())
                    .collect(),
            });
        }
    }
    let prefixed = containers
        .iter()
        .copied()
        .filter(|container| container.container_id.as_str().starts_with(selector))
        .collect::<Vec<_>>();
    match prefixed.as_slice() {
        [] => Err(ContainerSelectorError::NotFound {
            selector: selector.to_owned(),
        }),
        [container] => Ok(container),
        _ => Err(ContainerSelectorError::Ambiguous {
            selector: selector.to_owned(),
            container_ids: prefixed
                .into_iter()
                .map(|container| container.container_id.clone())
                .collect(),
        }),
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
            return Err(format!("invalid Service log selector {value:?}").into());
        }
        if let Some(existing) = parsed.iter_mut().find(|arg| arg.service == service) {
            match container {
                None => existing.containers.clear(),
                Some(container) if !existing.containers.is_empty() => {
                    if !existing.containers.iter().any(|value| value == container) {
                        existing.containers.push(container.to_owned());
                    }
                }
                Some(_) => {}
            }
        } else {
            parsed.push(ServiceArg {
                service: service.to_owned(),
                containers: container.into_iter().map(ToOwned::to_owned).collect(),
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
        .ok_or_else(|| {
            format!("invalid log tail {value:?}: expected a non-negative integer or all").into()
        })
}

#[must_use]
pub fn service_logs_use_compose(explicit: &[String]) -> bool {
    explicit.is_empty()
}

pub fn parse_proxy_ports(value: &str) -> Result<ProxyPorts, OperatorError> {
    let (local, remote) = value.split_once(':').map_or(("0", value), |parts| parts);
    if remote.contains(':') {
        return Err("invalid proxy port: expected [LOCAL_PORT:]REMOTE_PORT".into());
    }
    let local = local
        .parse::<u16>()
        .map_err(|_| format!("invalid local port {local:?}: expected 0-65535"))?;
    let remote = remote
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| format!("invalid remote port {remote:?}: expected 1-65535"))?;
    Ok(ProxyPorts { local, remote })
}

pub fn select_proxy_container(
    service: &ServiceObservation,
) -> Result<&ContainerObservation, OperatorError> {
    service
        .containers
        .iter()
        .find(|container| {
            container.kind == ContainerKind::ServiceContainer
                && matches!(
                    container.runtime,
                    ployz_core::ContainerRuntimeObservation::Running {
                        health: HealthObservation::Healthy | HealthObservation::NotConfigured
                    }
                )
        })
        .ok_or_else(|| "no running healthy regular container found for Service".into())
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
    pub skipped_services: Vec<String>,
}

impl Client {
    pub async fn open_exec(
        &mut self,
        service_selector: &str,
        container_selector: Option<&str>,
        options: ExecOptions,
    ) -> Result<ExecSession, OperatorError> {
        let live = self
            .live_services()
            .await
            .map_err(|error| OperatorError::Message(error.to_string()))?;
        let service = select_service(&live.services, service_selector)
            .map_err(|error| OperatorError::Message(error.to_string()))?;
        let container = select_exec_container(service, container_selector)?;
        let machine_id = container.machine_id.clone();
        let config = ExecRequestFrame::Config(ExecConfig {
            container_id: container.container_id.clone(),
            options,
        })
        .encode()?;
        let (sender, receiver) = mpsc::channel(32);
        sender
            .send(config)
            .await
            .map_err(|_| OperatorError::Message("exec request stream closed".into()))?;
        let output = self
            .exec_stream(
                &MachineSelector::from(&machine_id),
                tokio_stream::wrappers::ReceiverStream::new(receiver),
            )
            .await?;
        Ok(ExecSession {
            input: sender,
            output,
        })
    }

    pub async fn open_service_logs(
        &mut self,
        args: &[ServiceArg],
        machine_selectors: &[String],
        options: LogsOptions,
        compose_selection: bool,
        cancellation: CancellationToken,
    ) -> Result<ServiceLogInputs, OperatorError> {
        let machines = self
            .call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await
            .map_err(|error| OperatorError::Message(error.to_string()))?
            .machines;
        let selected_machines = select_machines(&machines, machine_selectors)?;
        let machine_ids = selected_machines
            .iter()
            .map(|machine| machine.machine.id.clone())
            .collect::<HashSet<_>>();
        let live = self
            .live_services()
            .await
            .map_err(|error| OperatorError::Message(error.to_string()))?;
        let mut inputs = Vec::new();
        let mut skipped_services = Vec::new();
        for arg in args {
            let service = match select_service(&live.services, &arg.service) {
                Ok(service) => service,
                Err(ployz_core::ServiceSelectorError::NotFound { .. }) if compose_selection => {
                    skipped_services.push(arg.service.clone());
                    continue;
                }
                Err(error) => return Err(error.to_string().into()),
            };
            let containers = select_log_containers(service, &arg.containers)?;
            let containers = containers
                .into_iter()
                .filter(|container| machine_ids.contains(&container.machine_id))
                .collect::<Vec<_>>();
            if containers.is_empty() {
                return Err(format!(
                    "no containers for Service {:?} found on the selected Machines",
                    arg.service
                )
                .into());
            }
            for container in containers {
                let request = op::ContainerLogs::into_request(ContainerLogsRequest {
                    container_id: container.container_id.clone(),
                    options: options.clone(),
                })
                .encode()
                .map_err(|error| OperatorError::Message(error.to_string()))?;
                let identity = format!("{}/{}", arg.service, container.display_name);
                let target = MachineSelector::from(&container.machine_id);
                // TODO(UT-082): earlier Container log streams intentionally survive until the
                // parent cancellation token is cancelled.
                if let Err(error) = open_log_input(&mut inputs, &cancellation, async {
                    self.container_logs_stream(&target, request)
                        .await
                        .map(|stream| stream_input(identity, stream))
                })
                .await
                {
                    return Err(format!(
                        "open logs for Container {} on Machine {}: {error}",
                        container.container_id, container.machine_id
                    )
                    .into());
                }
            }
        }
        if inputs.is_empty() {
            return Err("none of the selected Services exist in the Cluster".into());
        }
        Ok(ServiceLogInputs {
            inputs,
            skipped_services,
        })
    }

    pub async fn open_machine_logs(
        &mut self,
        services: &[String],
        machine_selectors: &[String],
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
                        OperatorError::Message(format!(
                            "unsupported Machine log service {service:?}; {expected}"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        let machines = self
            .call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await
            .map_err(|error| OperatorError::Message(error.to_string()))?
            .machines;
        let machines = select_machines(&machines, machine_selectors)?;
        let mut inputs = Vec::new();
        for service in services {
            for machine in &machines {
                let request = op::MachineLogs::into_request(MachineLogsRequest {
                    service,
                    options: options.clone(),
                })
                .encode()
                .map_err(|error| OperatorError::Message(error.to_string()))?;
                let identity = format!("{service}@{}", machine.machine.name);
                let target = MachineSelector::from(&machine.machine.id);
                // TODO(UT-083): earlier Machine log streams intentionally survive until the
                // parent cancellation token is cancelled.
                if let Err(error) = open_log_input(&mut inputs, &cancellation, async {
                    self.machine_logs_stream(&target, request)
                        .await
                        .map(|stream| stream_input(identity, stream))
                })
                .await
                {
                    return Err(format!(
                        "open {service} logs on Machine {}: {error}",
                        machine.machine.name
                    )
                    .into());
                }
            }
        }
        Ok(inputs)
    }
}

fn stream_input(identity: String, stream: Streaming<OpaquePayload>) -> LogInput {
    LogInput {
        identity,
        stream: Box::pin(stream.map(|result| {
            result
                .map_err(|error| error.to_string())
                .and_then(|payload| LogEntry::decode(&payload).map_err(|error| error.to_string()))
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

async fn open_log_input(
    inputs: &mut Vec<LogInput>,
    cancellation: &CancellationToken,
    open: impl Future<Output = Result<LogInput, tonic::Status>>,
) -> Result<(), tonic::Status> {
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

fn select_log_containers<'a>(
    service: &'a ServiceObservation,
    selectors: &[String],
) -> Result<Vec<&'a ContainerObservation>, OperatorError> {
    let all = service
        .containers
        .iter()
        .chain(&service.hook_containers)
        .collect::<Vec<_>>();
    if selectors.is_empty() {
        return Ok(all);
    }
    let mut selected = Vec::new();
    for selector in selectors {
        let container = resolve_container_selector(&all, selector)?;
        if !selected
            .iter()
            .any(|selected: &&ContainerObservation| selected.container_id == container.container_id)
        {
            selected.push(container);
        }
    }
    Ok(selected)
}

fn select_machines<'a>(
    machines: &'a [MachineObservation],
    selectors: &[String],
) -> Result<Vec<&'a MachineObservation>, OperatorError> {
    let eligible = machines
        .iter()
        .filter(|machine| {
            matches!(
                machine.membership,
                MembershipObservation::Up | MembershipObservation::Suspect
            )
        })
        .collect::<Vec<_>>();
    if selectors.is_empty() {
        if eligible.is_empty() {
            return Err("no Machines found".into());
        }
        return Ok(eligible);
    }
    let selectors = selectors
        .iter()
        .map(|selector| MachineSelector::parse(selector.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| OperatorError::Message(error.to_string()))?;
    let visible = eligible
        .iter()
        .map(|observation| observation.machine.clone())
        .collect::<Vec<_>>();
    resolve_machine_selectors(&visible, &selectors)
        .map_err(|error| OperatorError::Message(error.to_string()))?
        .into_iter()
        .map(|machine| {
            eligible
                .iter()
                .copied()
                .find(|observation| observation.machine.id == machine.id)
                .ok_or_else(|| "selected Machine disappeared from the snapshot".into())
        })
        .collect()
}

struct LogEvent {
    index: usize,
    entry: Option<Result<LogEntry, String>>,
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
