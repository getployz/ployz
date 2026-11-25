use ployz_core::labels;
use std::time::Duration;

use bollard::{
    container::LogOutput,
    exec::{CreateExecOptions, ResizeExecOptions, StartExecOptions, StartExecResults},
    query_parameters::LogsOptionsBuilder,
};
use chrono::DateTime;
use futures_util::{Stream, StreamExt};
use ployz_core::scrub::SealedForms;
use ployz_core::{
    ContainerId, ExecConfig, ExecRequestFrame, ExecResponseFrame, LogMetadata, LogOrigin,
    LogsOptions, MachineId, MachineName, OpaquePayload, RpcError, RpcErrorCode,
};
use tokio::{io::AsyncWriteExt, sync::mpsc};
use tonic::{Status, Streaming};

use super::{ContainerRuntime, docker_error};
use crate::logs::{
    JournalError, LogLineStream, LogSource, RawLogEntry, RpcStream, serve_logs, split_at_space,
};
use crate::machine::Bound;
use crate::sealed_env;

const NANOS_PER_SECOND: i64 = 1_000_000_000;
const EXEC_OUTPUT_IDLE: Duration = Duration::from_millis(100);
/// How long the command of an exec whose client went away has to exit after
/// SIGTERM before SIGKILL, as `docker stop` gives a container.
const ABANDONED_EXEC_GRACE: Duration = Duration::from_secs(10);
/// How long a command has to be gone after SIGKILL before its stop is reported unconfirmed.
const KILLED_EXEC_GRACE: Duration = Duration::from_secs(5);
/// How long one Docker call of an exec's stop may take.
const DOCKER_ANSWER: Duration = Duration::from_secs(5);
/// How often a detached exec a management key started is checked for having exited, so its
/// watch ends with it.
const DETACHED_EXEC_POLL: Duration = Duration::from_secs(1);
const EXEC_ENDED: &str = "Exec stopped: this Device no longer has Full access to this Server.";

/// An exec Docker started, with what its caller asked for.
struct StartedExec {
    id: String,
    config: ExecConfig,
    started: StartExecResults,
}

impl ContainerRuntime {
    /// Run an exec for the caller `bound` ties it to. A task of its own holds `bound` from
    /// before Docker starts the command until the command ended, so a caller that goes away
    /// mid-start leaves no command running unwatched. A key change that ends `bound` closes
    /// the stream and stops the command, attached or detached, and learns whether it stopped.
    pub async fn exec(
        &self,
        requests: Streaming<OpaquePayload>,
        bound: Bound,
    ) -> Result<RpcStream, Status> {
        let (opened, open) = tokio::sync::oneshot::channel();
        tokio::spawn(self.clone().run_exec(requests, bound, opened));
        open.await
            .unwrap_or_else(|_| Err(Status::internal("the exec's task failed")))
    }

    async fn run_exec(
        self,
        mut requests: Streaming<OpaquePayload>,
        bound: Bound,
        opened: tokio::sync::oneshot::Sender<Result<RpcStream, Status>>,
    ) {
        let started = match self.start_exec(&mut requests, &bound, &opened).await {
            Ok(started) => started,
            Err(status) => {
                let _ = opened.send(Err(status));
                return;
            }
        };
        let docker = self.docker.client.clone();
        let exec_id = started.id;
        let (sender, receiver) = mpsc::channel(32);
        if send_exec(&sender, ExecResponseFrame::ExecId(exec_id.clone()))
            .await
            .is_err()
        {
            return;
        }
        // A caller gone by now drops the stream with the send, and an attached exec then
        // stops as an abandoned one does.
        let _ = opened.send(Ok(tokio_stream::wrappers::ReceiverStream::new(receiver)));
        match started.started {
            StartExecResults::Detached => {
                if bound.may_end() {
                    tokio::select! {
                        () = detached_exec_exit(&docker, &exec_id) => {}
                        () = bound.ended() => confirm_stopped(bound, stop_exec(&docker, &exec_id).await),
                    }
                }
            }
            StartExecResults::Attached {
                mut output,
                mut input,
            } => {
                let tty = started.config.options.tty;
                let stdin_task = started.config.options.attach_stdin.then(|| {
                    let docker = docker.clone();
                    let exec_id = exec_id.clone();
                    let sender = sender.clone();
                    tokio::spawn(async move {
                        while let Ok(Some(payload)) = requests.message().await {
                            match ExecRequestFrame::decode(&payload) {
                                Ok(ExecRequestFrame::Stdin(bytes)) => {
                                    if let Err(error) = input.write_all(&bytes).await {
                                        let _ = send_exec_error(&sender, error).await;
                                        return;
                                    }
                                }
                                Ok(ExecRequestFrame::Resize { width, height }) if tty => {
                                    if let Err(error) = docker
                                        .resize_exec(&exec_id, ResizeExecOptions { width, height })
                                        .await
                                    {
                                        let _ = send_exec_error(&sender, error).await;
                                        return;
                                    }
                                }
                                Ok(ExecRequestFrame::Resize { .. }) => {}
                                Ok(ExecRequestFrame::Config(_)) => {
                                    let _ = send_exec_error(
                                        &sender,
                                        "exec config may only be sent as the first frame",
                                    )
                                    .await;
                                    return;
                                }
                                Err(error) => {
                                    let _ = send_exec_error(&sender, error).await;
                                    return;
                                }
                            }
                        }
                        let _ = input.shutdown().await;
                    })
                });
                let stdin = stdin_task
                    .as_ref()
                    .map(tokio::task::JoinHandle::abort_handle);
                tokio::select! {
                    _ = forward_attached_exec(
                        &docker,
                        &exec_id,
                        &mut output,
                        tty,
                        &sender,
                        stdin_task,
                    ) => {}
                    () = bound.ended() => {
                        if let Some(stdin) = stdin {
                            stdin.abort();
                        }
                        if let Ok(frame) = ExecResponseFrame::Error(RpcError::new(RpcErrorCode::Unauthenticated, EXEC_ENDED)).encode() {
                            let _ = sender.try_send(Ok(frame));
                        }
                        drop(sender);
                        confirm_stopped(bound, stop_exec(&docker, &exec_id).await);
                    }
                }
            }
        }
    }

    /// Create and start the exec the first frame asks for. Starts nothing once `bound` ended
    /// or the caller went away; a start under way when either happens runs to its answer, so
    /// the command Docker started is never left unwatched.
    async fn start_exec(
        &self,
        requests: &mut Streaming<OpaquePayload>,
        bound: &Bound,
        caller: &tokio::sync::oneshot::Sender<Result<RpcStream, Status>>,
    ) -> Result<StartedExec, Status> {
        let first = tokio::select! {
            first = requests.message() => first?,
            () = bound.ended() => return Err(Status::unauthenticated(EXEC_ENDED)),
        }
        .ok_or_else(|| Status::invalid_argument("exec stream is empty"))?;
        let ExecRequestFrame::Config(config) =
            ExecRequestFrame::decode(&first).map_err(|error| {
                ployz_core::rpc::caused_status(tonic::Code::InvalidArgument, &error)
            })?
        else {
            return Err(Status::invalid_argument(
                "first exec stream frame must contain config",
            ));
        };
        if config.options.command.is_empty() {
            return Err(Status::invalid_argument("exec command cannot be empty"));
        }
        self.observation()
            .ensure_managed(&config.container_id)
            .await
            .map_err(docker_status)?;
        let create = CreateExecOptions {
            attach_stdin: Some(config.options.attach_stdin),
            attach_stdout: Some(config.options.attach_stdout),
            attach_stderr: Some(config.options.attach_stderr),
            tty: Some(config.options.tty),
            cmd: Some(config.options.command.clone()),
            ..Default::default()
        };
        let created = self
            .docker
            .client
            .create_exec(config.container_id.as_str(), create)
            .await
            .map_err(|error| docker_status(docker_error(&config.container_id, error)))?;
        if bound.tie().is_ended() {
            return Err(Status::unauthenticated(EXEC_ENDED));
        }
        if caller.is_closed() {
            return Err(Status::cancelled("exec client disconnected"));
        }
        let started = self
            .docker
            .client
            .start_exec(
                &created.id,
                Some(StartExecOptions {
                    detach: config.options.detach,
                    tty: config.options.tty,
                    output_capacity: None,
                }),
            )
            .await
            .map_err(|error| docker_status(docker_error(&config.container_id, error)))?;
        Ok(StartedExec {
            id: created.id,
            config,
            started,
        })
    }

    pub async fn container_logs(
        &self,
        machine_id: &MachineId,
        machine_name: &MachineName,
        container_id: ContainerId,
        options: &LogsOptions,
    ) -> Result<RpcStream, Status> {
        let observation = self
            .observation()
            .inspect_managed(&container_id, machine_id)
            .await
            .map_err(docker_status)?;
        let metadata = LogMetadata {
            origin: LogOrigin::Service {
                service_id: observation.service_id(),
                service_name: observation.resolved_spec.name.clone(),
                container_id: observation.container_id,
                hook: observation.labels.get(labels::HOOK).cloned(),
            },
            machine_id: *machine_id,
            machine_name: machine_name.clone(),
        };
        let record = sealed_env::record_from_label(
            observation
                .labels
                .get(labels::SEALED_ENV)
                .map(String::as_str),
        )
        .map_err(|error| docker_status(error.into()))?;
        let mut source = self.raw_logs(container_id.as_str(), options)?;
        if let Some(record) = record {
            let secrets = self
                .sealing()
                .secrets(&record)
                .map_err(|error| docker_status(error.into()))?;
            source = sealed_env::scrub_source(source, SealedForms::compile(&secrets));
        }
        Ok(serve_logs(source, metadata, options.follow))
    }

    #[expect(
        clippy::result_large_err,
        reason = "tonic::Status is the RPC error type"
    )]
    pub fn raw_logs(&self, container: &str, options: &LogsOptions) -> Result<LogSource, Status> {
        let since = options.since_nanos;
        let docker_options = docker_log_options(options)?;
        let stream = self
            .docker
            .client
            .logs(container, Some(docker_options))
            .map(|entry| entry.map(parse_log_output).map_err(JournalError::from))
            .filter(move |entry| {
                std::future::ready(
                    entry
                        .as_ref()
                        .map_or(true, |entry| not_before(entry.timestamp_unix_nanos, since)),
                )
            });
        Ok(Box::pin(stream))
    }
}

async fn forward_attached_exec(
    docker: &bollard::Docker,
    exec_id: &str,
    output: &mut (impl Stream<Item = Result<LogOutput, bollard::errors::Error>> + Unpin),
    tty: bool,
    sender: &mpsc::Sender<Result<OpaquePayload, Status>>,
    stdin_task: Option<tokio::task::JoinHandle<()>>,
) -> Result<(), Status> {
    enum Completion {
        StreamEnded,
        ProcessExited(Option<i64>),
        Failed(bollard::errors::Error),
        ClientGone,
    }

    let process_exited = wait_for_exec_exit(docker, exec_id);
    tokio::pin!(process_exited);
    let completion = loop {
        tokio::select! {
            biased;
            result = &mut process_exited => match result {
                Ok(exit_code) => break Ok(Completion::ProcessExited(exit_code)),
                Err(error) => break Ok(Completion::Failed(error)),
            },
            () = sender.closed() => break Ok(Completion::ClientGone),
            next = output.next() => match next {
                Some(Ok(output)) => match send_exec(sender, exec_output(output, tty)).await {
                    Ok(()) => {}
                    Err(_) if sender.is_closed() => break Ok(Completion::ClientGone),
                    Err(error) => break Err(error),
                },
                Some(Err(error)) => break Ok(Completion::Failed(error)),
                None => break Ok(Completion::StreamEnded),
            },
        }
    };
    if let Some(task) = stdin_task {
        task.abort();
    }
    let completion = completion?;

    match completion {
        Completion::StreamEnded => match docker.inspect_exec(exec_id).await {
            Ok(inspected) => send_exec_exit(sender, inspected.exit_code).await,
            Err(error) => send_exec_error(sender, error).await,
        },
        Completion::Failed(error) => send_exec_error(sender, error).await,
        Completion::ProcessExited(exit_code) => {
            finish_exec_output(output, tty, sender, exit_code).await
        }
        Completion::ClientGone => {
            stop_abandoned_exec(docker, exec_id).await;
            Ok(())
        }
    }
}

/// Stop the command of an exec whose client went away, as an interrupted
/// `ployz exec` with a TTY already does. No one waits on the outcome, so a stop that
/// can't be confirmed is only logged.
async fn stop_abandoned_exec(docker: &bollard::Docker, exec_id: &str) {
    if let Err(error) = stop_exec(docker, exec_id).await {
        tracing::warn!(error = %ployz_core::error_chain::inline(&error), "an abandoned exec did not confirm it stopped");
    }
}

/// Hand the key change that ended an exec its stop: confirmed, or why not.
fn confirm_stopped(bound: Bound, stopped: Result<ExecStopped, ExecStopError>) {
    match stopped {
        Ok(_) => drop(bound),
        Err(error) => bound.unconfirmed(error),
    }
}

/// How an exec's command ended once this daemon asked it to stop.
#[derive(Debug, Eq, PartialEq)]
enum ExecStopped {
    /// It had already exited, or Docker no longer knows the exec because its Container went.
    Exited,
    Terminated,
    Killed,
}

#[derive(Debug, thiserror::Error)]
enum ExecStopError {
    #[error("Docker didn't say whether exec {exec}'s command still runs")]
    Unread {
        exec: String,
        #[source]
        cause: Unanswered,
    },
    #[error("Could not signal exec {exec}'s command")]
    Signal {
        exec: String,
        #[source]
        cause: std::io::Error,
    },
    #[error(
        "Exec {exec}'s command was still running {} s after SIGKILL",
        KILLED_EXEC_GRACE.as_secs()
    )]
    Unkilled { exec: String },
}

#[derive(Debug, thiserror::Error)]
enum Unanswered {
    #[error("Docker did not answer within {} s", DOCKER_ANSWER.as_secs())]
    Late,
    #[error(transparent)]
    Docker(#[from] bollard::errors::Error),
}

/// Stop exec `exec_id`'s command: SIGTERM, then SIGKILL after [`ABANDONED_EXEC_GRACE`],
/// and confirm through the process itself that it exited. Bounded by
/// [`DOCKER_ANSWER`] per Docker call and both graces. Docker has no call to signal an exec,
/// but reports its process, which runs in this daemon's PID namespace on a Server and in
/// the testkit. The pidfd keeps the signals on that process even once it exits and its PID
/// is reused, and reads readable once it exited.
async fn stop_exec(docker: &bollard::Docker, exec_id: &str) -> Result<ExecStopped, ExecStopError> {
    let Some(process) = exec_process(docker, exec_id).await? else {
        return Ok(ExecStopped::Exited);
    };
    let failed = |cause: std::io::Error| ExecStopError::Signal {
        exec: exec_id.to_owned(),
        cause,
    };
    if !signal(&process, rustix::process::Signal::TERM).map_err(failed)? {
        return Ok(ExecStopped::Exited);
    }
    if exited_within(&process, ABANDONED_EXEC_GRACE)
        .await
        .map_err(failed)?
    {
        return Ok(ExecStopped::Terminated);
    }
    if !signal(&process, rustix::process::Signal::KILL).map_err(failed)? {
        return Ok(ExecStopped::Terminated);
    }
    if exited_within(&process, KILLED_EXEC_GRACE)
        .await
        .map_err(failed)?
    {
        Ok(ExecStopped::Killed)
    } else {
        Err(ExecStopError::Unkilled {
            exec: exec_id.to_owned(),
        })
    }
}

/// Send `signal` to `process`: `false` when it had already exited.
fn signal(process: &rustix::fd::OwnedFd, signal: rustix::process::Signal) -> std::io::Result<bool> {
    match rustix::process::pidfd_send_signal(process, signal) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::SRCH) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Whether `process` exited within `grace`, read from its pidfd.
async fn exited_within(process: &rustix::fd::OwnedFd, grace: Duration) -> std::io::Result<bool> {
    use rustix::fd::AsFd as _;
    let exited =
        tokio::io::unix::AsyncFd::with_interest(process.as_fd(), tokio::io::Interest::READABLE)?;
    Ok(tokio::time::timeout(grace, exited.readable()).await.is_ok())
}

/// A pidfd for the running process of exec `exec_id`, checked to still be it once the
/// pidfd holds it, or `None` once it exited. Docker reports the PID until it handles the
/// exit, and by then the PID may name another process, so that process must also be in
/// the cgroup of the exec's container, which an exec joins.
async fn exec_process(
    docker: &bollard::Docker,
    exec_id: &str,
) -> Result<Option<rustix::fd::OwnedFd>, ExecStopError> {
    let unread = |cause| ExecStopError::Unread {
        exec: exec_id.to_owned(),
        cause,
    };
    let inspect = || async {
        match tokio::time::timeout(DOCKER_ANSWER, docker.inspect_exec(exec_id)).await {
            Ok(Ok(inspected)) => Ok(Some(inspected)),
            // Docker forgets an exec whose Container went, and every process with it.
            Ok(Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            })) => Ok(None),
            Ok(Err(error)) => Err(unread(error.into())),
            Err(_) => Err(unread(Unanswered::Late)),
        }
    };
    let running = |inspected: &bollard::models::ExecInspectResponse| {
        inspected
            .running
            .unwrap_or(false)
            .then_some(inspected.pid)
            .flatten()
    };
    let Some(inspected) = inspect().await? else {
        return Ok(None);
    };
    let Some(pid) = running(&inspected) else {
        return Ok(None);
    };
    let Some(raw) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return Ok(None);
    };
    let process = match rustix::process::pidfd_open(raw, rustix::process::PidfdFlags::empty()) {
        Ok(process) => process,
        Err(rustix::io::Errno::SRCH) => return Ok(None),
        Err(error) => {
            return Err(ExecStopError::Signal {
                exec: exec_id.to_owned(),
                cause: error.into(),
            });
        }
    };
    let Some(container) = inspected.container_id else {
        return Ok(None);
    };
    let init = match tokio::time::timeout(
        DOCKER_ANSWER,
        super::inspect::inspect(docker, &container),
    )
    .await
    {
        Ok(Ok(Some(container))) => container.pid(),
        // No such Container: its processes are gone with it.
        Ok(Ok(None)) => return Ok(None),
        Ok(Err(error)) => return Err(unread(error.into())),
        Err(_) => return Err(unread(Unanswered::Late)),
    };
    let Some(init) = init else {
        return Ok(None);
    };
    let cgroup = |pid: i64| std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok();
    let still = inspect().await?.as_ref().and_then(running) == Some(pid);
    // A PID Docker still reports but in another cgroup is a new process: the exec's exited.
    let same = cgroup(pid).is_some_and(|process| Some(process) == cgroup(init));
    Ok((still && same).then_some(process))
}

async fn finish_exec_output(
    output: &mut (impl Stream<Item = Result<LogOutput, bollard::errors::Error>> + Unpin),
    tty: bool,
    sender: &mpsc::Sender<Result<OpaquePayload, Status>>,
    exit_code: Option<i64>,
) -> Result<(), Status> {
    // ponytail: 100 ms idle ends a held hijack; use a Docker output-complete signal if added.
    // Forwarding buffered output may take longer under downstream backpressure.
    loop {
        match tokio::time::timeout(EXEC_OUTPUT_IDLE, output.next()).await {
            Ok(Some(Ok(output))) => send_exec(sender, exec_output(output, tty)).await?,
            Ok(Some(Err(error))) => return send_exec_error(sender, error).await,
            Ok(None) | Err(_) => break,
        }
    }
    send_exec_exit(sender, exit_code).await
}

async fn wait_for_exec_exit(
    docker: &bollard::Docker,
    exec_id: &str,
) -> Result<Option<i64>, bollard::errors::Error> {
    poll_exec_exit(docker, exec_id, Duration::from_millis(100)).await
}

/// Completes once a detached exec's command has exited, or Docker no longer knows the exec
/// because its Container went. The watch of a detached exec lives only that long: it outlives
/// the `exec` call, bounded by the command the caller started, and acts only to stop it.
async fn detached_exec_exit(docker: &bollard::Docker, exec_id: &str) {
    loop {
        match poll_exec_exit(docker, exec_id, DETACHED_EXEC_POLL).await {
            Ok(_)
            | Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => return,
            Err(_) => {}
        }
    }
}

async fn poll_exec_exit(
    docker: &bollard::Docker,
    exec_id: &str,
    every: Duration,
) -> Result<Option<i64>, bollard::errors::Error> {
    loop {
        // ponytail: one inspect per `every`; use a Docker exec wait API if one appears.
        tokio::time::sleep(every).await;
        let inspected = docker.inspect_exec(exec_id).await?;
        if inspected.running == Some(false) {
            return Ok(inspected.exit_code);
        }
    }
}

fn exec_output(output: LogOutput, tty: bool) -> ExecResponseFrame {
    match output {
        LogOutput::StdErr { message } if !tty => ExecResponseFrame::Stderr(message.to_vec()),
        LogOutput::StdErr { message }
        | LogOutput::StdOut { message }
        | LogOutput::StdIn { message }
        | LogOutput::Console { message } => ExecResponseFrame::Stdout(message.to_vec()),
    }
}

async fn send_exec(
    sender: &mpsc::Sender<Result<OpaquePayload, Status>>,
    frame: ExecResponseFrame,
) -> Result<(), Status> {
    let payload = frame
        .encode()
        .map_err(|error| ployz_core::rpc::caused_status(tonic::Code::Internal, &error))?;
    sender
        .send(Ok(payload))
        .await
        .map_err(|_| Status::cancelled("exec client disconnected"))
}

async fn send_exec_error(
    sender: &mpsc::Sender<Result<OpaquePayload, Status>>,
    error: impl std::fmt::Display,
) -> Result<(), Status> {
    send_exec(
        sender,
        ExecResponseFrame::Error(RpcError::internal(error.to_string())),
    )
    .await
}

async fn send_exec_exit(
    sender: &mpsc::Sender<Result<OpaquePayload, Status>>,
    code: Option<i64>,
) -> Result<(), Status> {
    send_exec(
        sender,
        ExecResponseFrame::Exit(code.and_then(|code| i32::try_from(code).ok()).unwrap_or(1)),
    )
    .await
}

fn docker_status(error: super::Error) -> Status {
    let code = match error.rpc_code() {
        RpcErrorCode::NotFound => tonic::Code::NotFound,
        RpcErrorCode::InvalidArgument => tonic::Code::InvalidArgument,
        RpcErrorCode::Ambiguous | RpcErrorCode::ConfirmationRequired => {
            tonic::Code::FailedPrecondition
        }
        RpcErrorCode::Conflict => tonic::Code::AlreadyExists,
        RpcErrorCode::Unavailable => tonic::Code::Unavailable,
        RpcErrorCode::Unsupported => tonic::Code::Unimplemented,
        RpcErrorCode::Internal | RpcErrorCode::Unknown(_) => tonic::Code::Internal,
        RpcErrorCode::Unauthenticated => tonic::Code::Unauthenticated,
    };
    ployz_core::rpc::caused_status(code, &error)
}

fn parse_log_output(output: LogOutput) -> RawLogEntry {
    let (stream, bytes) = match output {
        LogOutput::StdErr { message } => (LogLineStream::Stderr, message),
        LogOutput::StdOut { message }
        | LogOutput::StdIn { message }
        | LogOutput::Console { message } => (LogLineStream::Stdout, message),
    };
    let (timestamp_unix_nanos, message) = split_timestamp(&bytes);
    RawLogEntry {
        stream,
        timestamp_unix_nanos,
        message: message.to_vec(),
    }
}

fn split_timestamp(bytes: &[u8]) -> (i64, &[u8]) {
    let Some((timestamp, message)) = split_at_space(bytes) else {
        return (0, bytes);
    };
    let Ok(timestamp) = std::str::from_utf8(timestamp) else {
        return (0, bytes);
    };
    match DateTime::parse_from_rfc3339(timestamp) {
        Ok(timestamp) => (timestamp.timestamp_nanos_opt().unwrap_or(0), message),
        Err(_) => (0, bytes),
    }
}

#[expect(
    clippy::result_large_err,
    reason = "tonic::Status is the RPC error type"
)]
fn docker_log_options(
    options: &LogsOptions,
) -> Result<bollard::query_parameters::LogsOptions, Status> {
    let tail = if options.tail < 0 {
        "all".to_owned()
    } else {
        options.tail.to_string()
    };
    let mut builder = LogsOptionsBuilder::default()
        .stdout(true)
        .stderr(true)
        .follow(options.follow)
        .timestamps(true)
        .tail(&tail);
    if let Some(since) = options.since_nanos {
        builder = builder.since(docker_log_timestamp(since.div_euclid(NANOS_PER_SECOND))?);
    }
    if let Some(until) = options.until_unix_seconds {
        builder = builder.until(docker_log_timestamp(until)?);
    }
    Ok(builder.build())
}

/// Docker takes `since` in whole seconds, so it sends the rest of that
/// second too; an entry with no readable time is kept.
fn not_before(ts: i64, since: Option<i64>) -> bool {
    ts == 0 || since.is_none_or(|since| ts >= since)
}

#[expect(
    clippy::result_large_err,
    reason = "tonic::Status is the RPC error type"
)]
fn docker_log_timestamp(timestamp: i64) -> Result<i32, Status> {
    i32::try_from(timestamp)
        .map_err(|_| Status::invalid_argument(format!("log time {timestamp} is out of range")))
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn exited_exec_drains_buffered_output_under_backpressure_then_closes_a_held_hijack() {
        let (sender, mut receiver) = mpsc::channel(1);
        let forwarding = tokio::spawn(async move {
            let mut output = tokio_stream::iter((0..5).map(|byte| {
                Ok(LogOutput::StdOut {
                    message: Bytes::from(vec![byte; 4096]),
                })
            }))
            .chain(futures_util::stream::pending());
            finish_exec_output(&mut output, false, &sender, Some(0)).await
        });
        let mut bytes = Vec::new();
        let mut exit = None;
        while let Some(payload) = receiver.recv().await {
            match ExecResponseFrame::decode(&payload.unwrap()).unwrap() {
                ExecResponseFrame::Stdout(output) => {
                    bytes.extend(output);
                    tokio::time::sleep(EXEC_OUTPUT_IDLE + Duration::from_millis(50)).await;
                }
                ExecResponseFrame::Exit(code) => exit = Some(code),
                other @ (ExecResponseFrame::ExecId(_)
                | ExecResponseFrame::Stderr(_)
                | ExecResponseFrame::Error(_)) => panic!("unexpected exec frame: {other:?}"),
            }
        }
        forwarding.await.unwrap().unwrap();
        assert_eq!(bytes.len(), 5 * 4096);
        assert_eq!(
            bytes,
            (0..5).flat_map(|byte| vec![byte; 4096]).collect::<Vec<_>>()
        );
        assert_eq!(exit, Some(0));
    }

    #[test]
    fn docker_log_parser_keeps_binary_messages_and_streams() {
        assert_eq!(
            parse_log_output(LogOutput::StdErr {
                message: Bytes::from_static(b"2026-08-14T09:10:11.123456789Z \xff\n"),
            }),
            RawLogEntry {
                stream: LogLineStream::Stderr,
                timestamp_unix_nanos: 1_786_698_611_123_456_789,
                message: b"\xff\n".to_vec(),
            }
        );
    }

    #[test]
    fn a_nanosecond_since_keeps_exactly_the_output_from_that_instant() {
        let since = 1_786_698_000_123_456_789;
        let options = docker_log_options(&LogsOptions {
            follow: false,
            tail: 100,
            since_nanos: Some(since),
            until_unix_seconds: Some(1_786_701_600),
        })
        .unwrap();

        assert_eq!(options.since, 1_786_698_000);
        assert_eq!(options.until, 1_786_701_600);
        assert!(!not_before(since - 1, Some(since)));
        assert!(not_before(since, Some(since)));
        assert!(not_before(since + 1, Some(since)));
        assert!(not_before(0, Some(since)));
        assert!(not_before(1, None));
    }
}
