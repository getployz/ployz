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

use super::exec_watch::{self, ExecStopError, ExecWatch};
use super::processes::Cgroup;
use super::{ContainerRuntime, docker_error};
use crate::logs::{
    JournalError, LogLineStream, LogSource, RawLogEntry, RpcStream, serve_logs, split_at_space,
};
use crate::machine::{Bound, Seen};
use crate::sealed_env;

const NANOS_PER_SECOND: i64 = 1_000_000_000;
const EXEC_OUTPUT_IDLE: Duration = Duration::from_millis(100);
/// How often a detached exec a management key started is looked at until its process
/// shows, so its watch can follow that process.
const DETACHED_EXEC_POLL: Duration = Duration::from_secs(1);
/// How often an attached exec is looked at until its process shows.
const EXEC_EXIT_POLL: Duration = Duration::from_millis(100);
const EXEC_ENDED: &str = "Exec stopped: this Device no longer has Full access to this Server.";

/// An exec Docker started, with what its caller asked for.
struct StartedExec {
    watch: ExecWatch,
    config: ExecConfig,
    started: StartExecResults,
}

/// Why an exec didn't start, and the exec whose start Docker may have run anyway.
struct Unstarted {
    status: Status,
    /// Set once a start was sent: Docker can start the command and lose the answer.
    sent: Option<ExecWatch>,
}

impl From<Status> for Unstarted {
    fn from(status: Status) -> Self {
        Self { status, sent: None }
    }
}

impl ContainerRuntime {
    /// Run an exec for the caller `bound` ties it to. A task of its own holds `bound` from
    /// before Docker starts the command until it saw the command end, so a caller that goes
    /// away mid-start leaves no command running unwatched. A key change that ends `bound`
    /// closes the stream and stops the command, attached or detached, and learns whether it
    /// stopped.
    pub async fn exec(
        &self,
        requests: Streaming<OpaquePayload>,
        bound: Bound,
    ) -> Result<RpcStream, Status> {
        let (opened, open) = tokio::sync::oneshot::channel();
        tokio::spawn(self.clone().run_exec(requests, bound, opened));
        open.await
            .map_err(|_| Status::internal("the exec's task failed"))?
    }

    async fn run_exec(
        self,
        mut requests: Streaming<OpaquePayload>,
        bound: Bound,
        opened: tokio::sync::oneshot::Sender<Result<RpcStream, Status>>,
    ) {
        let started = match self.start_exec(&mut requests, &bound, &opened).await {
            Ok(started) => started,
            Err(Unstarted { status, sent }) => {
                let _ = opened.send(Err(status));
                match sent {
                    None => bound.stopped(Seen::NeverSent),
                    Some(watch) => {
                        bound.name(watch.name());
                        exec_watch::settle(bound, &watch, watch.stop().await);
                    }
                }
                return;
            }
        };
        let watch = started.watch;
        bound.name(watch.name());
        let (sender, receiver) = mpsc::channel(32);
        // The receiver is still here, so only an unencodable frame fails, and a caller that
        // never reads it then leaves as one gone away does.
        let _ = send_exec(&sender, ExecResponseFrame::ExecId(watch.id().to_owned())).await;
        // A caller gone by now drops the stream with the send, and an attached exec then
        // stops as an abandoned one does.
        let _ = opened.send(Ok(tokio_stream::wrappers::ReceiverStream::new(receiver)));
        let stopped = match started.started {
            // Work no key started is no key change's to wait for, so nothing watches it, and
            // its Bound leaves nothing behind.
            StartExecResults::Detached if !bound.may_end() => return drop(bound),
            StartExecResults::Detached => {
                tokio::select! {
                    exited = watch.exited(DETACHED_EXEC_POLL) => exited.map(|ended| ended.seen),
                    () = bound.ended() => watch.stop().await,
                }
            }
            StartExecResults::Attached {
                mut output,
                mut input,
            } => {
                let tty = started.config.options.tty;
                let stdin_task = started.config.options.attach_stdin.then(|| {
                    let docker = self.docker.client.clone();
                    let exec_id = watch.id().to_owned();
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
                    stopped = forward_attached_exec(
                        &watch,
                        &mut output,
                        tty,
                        &sender,
                        stdin_task,
                    ) => stopped,
                    () = bound.ended() => {
                        if let Some(stdin) = stdin {
                            stdin.abort();
                        }
                        if let Ok(frame) = ExecResponseFrame::Error(RpcError::new(RpcErrorCode::Unauthenticated, EXEC_ENDED)).encode() {
                            let _ = sender.try_send(Ok(frame));
                        }
                        drop(sender);
                        watch.stop().await
                    }
                }
            }
        };
        exec_watch::settle(bound, &watch, stopped);
    }

    /// Create and start the exec the first frame asks for. Starts nothing once `bound` ended
    /// or the caller went away; a start under way when either happens runs to its answer, so
    /// the command Docker started is never left unwatched.
    async fn start_exec(
        &self,
        requests: &mut Streaming<OpaquePayload>,
        bound: &Bound,
        caller: &tokio::sync::oneshot::Sender<Result<RpcStream, Status>>,
    ) -> Result<StartedExec, Unstarted> {
        let first = tokio::select! {
            first = requests.message() => first?,
            () = bound.ended() => return Err(Status::unauthenticated(EXEC_ENDED).into()),
        }
        .ok_or_else(|| Status::invalid_argument("exec stream is empty"))?;
        let ExecRequestFrame::Config(config) =
            ExecRequestFrame::decode(&first).map_err(|error| {
                ployz_core::rpc::caused_status(tonic::Code::InvalidArgument, &error)
            })?
        else {
            return Err(
                Status::invalid_argument("first exec stream frame must contain config").into(),
            );
        };
        if config.options.command.is_empty() {
            return Err(Status::invalid_argument("exec command cannot be empty").into());
        }
        let container = self
            .observation()
            .ensure_managed(&config.container_id)
            .await
            .map_err(docker_status)?;
        let cgroup = exec_cgroup(container.pid()).await?;
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
            return Err(Status::unauthenticated(EXEC_ENDED).into());
        }
        if caller.is_closed() {
            return Err(Status::cancelled("exec client disconnected").into());
        }
        let watch = ExecWatch::new(self.docker.client.clone(), created.id.clone(), cgroup);
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
            .await;
        match started {
            Ok(started) => Ok(StartedExec {
                watch,
                config,
                started,
            }),
            Err(error) => Err(Unstarted {
                status: docker_status(docker_error(&config.container_id, error)),
                sent: Some(watch),
            }),
        }
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

/// An exec's command can't be followed without its Container's processes.
#[derive(Debug, thiserror::Error)]
#[error("Could not read the Container's processes, so an exec in it could not be followed")]
struct Unfollowable(#[source] std::io::Error);

/// The cgroup of the Container whose process Docker reports at `init`, read before an exec
/// in it starts, so the exec's watch can tell the command's process from those already
/// there. An exec that can't be followed doesn't start.
async fn exec_cgroup(init: Option<i64>) -> Result<Cgroup, Unstarted> {
    let init = init
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| Status::failed_precondition("the Container has no process to exec in"))?;
    let read = tokio::task::spawn_blocking(move || Cgroup::read(init))
        .await
        .unwrap_or_else(|failed| Err(std::io::Error::other(failed)));
    read.map_err(|error| {
        ployz_core::rpc::caused_status(tonic::Code::Internal, &Unfollowable(error)).into()
    })
}

/// Forward an attached exec's output until its command ended or its client went away, and
/// stop the command in the latter case: how the command ended, or why that's unknown. Output
/// Docker stops sending, by an error or its end, leaves the command watched until one of
/// those.
async fn forward_attached_exec(
    watch: &ExecWatch,
    output: &mut (impl Stream<Item = Result<LogOutput, bollard::errors::Error>> + Unpin),
    tty: bool,
    sender: &mpsc::Sender<Result<OpaquePayload, Status>>,
    stdin_task: Option<tokio::task::JoinHandle<()>>,
) -> Result<Seen, ExecStopError> {
    enum Completion {
        ProcessExited(Result<exec_watch::Ended, ExecStopError>),
        ClientGone,
    }

    let process_exited = watch.exited(EXEC_EXIT_POLL);
    tokio::pin!(process_exited);
    let mut reading = true;
    let completion = loop {
        tokio::select! {
            biased;
            exited = &mut process_exited => break Completion::ProcessExited(exited),
            () = sender.closed() => break Completion::ClientGone,
            next = output.next(), if reading => match next {
                Some(Ok(output)) => {
                    if send_exec(sender, exec_output(output, tty)).await.is_err() {
                        break Completion::ClientGone;
                    }
                }
                Some(Err(error)) => {
                    reading = false;
                    let _ = send_exec_error(sender, error).await;
                }
                None => reading = false,
            },
        }
    };
    if let Some(task) = stdin_task {
        task.abort();
    }
    match completion {
        Completion::ProcessExited(Ok(ended)) => {
            let exit_code = match ended.exit_code {
                Some(code) => Some(code),
                None => watch.exit_code().await,
            };
            let _ = finish_exec_output(output, reading, tty, sender, exit_code).await;
            Ok(ended.seen)
        }
        // The command may run on, so the client learns that, not an exit.
        Completion::ProcessExited(Err(lost)) => {
            let _ = send_exec_error(sender, &lost).await;
            Err(lost)
        }
        // An interrupted `ployz exec` with a TTY already stops its command; one without
        // goes the same way.
        Completion::ClientGone => watch.stop().await,
    }
}

/// Forward what output is left once the command exited, while `reading`, then its exit.
async fn finish_exec_output(
    output: &mut (impl Stream<Item = Result<LogOutput, bollard::errors::Error>> + Unpin),
    reading: bool,
    tty: bool,
    sender: &mpsc::Sender<Result<OpaquePayload, Status>>,
    exit_code: Option<i64>,
) -> Result<(), Status> {
    // ponytail: 100 ms idle ends a held hijack; use a Docker output-complete signal if added.
    // Forwarding buffered output may take longer under downstream backpressure.
    if reading {
        loop {
            match tokio::time::timeout(EXEC_OUTPUT_IDLE, output.next()).await {
                Ok(Some(Ok(output))) => send_exec(sender, exec_output(output, tty)).await?,
                Ok(Some(Err(error))) => return send_exec_error(sender, error).await,
                Ok(None) | Err(_) => break,
            }
        }
    }
    send_exec_exit(sender, exit_code).await
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
            finish_exec_output(&mut output, true, false, &sender, Some(0)).await
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
