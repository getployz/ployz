use std::time::Duration;

use bollard::{
    container::LogOutput,
    exec::{CreateExecOptions, ResizeExecOptions, StartExecOptions, StartExecResults},
    query_parameters::LogsOptionsBuilder,
};
use chrono::DateTime;
use futures_util::{Stream, StreamExt};
use ployz_core::{
    ContainerLogsRequest, ExecRequestFrame, ExecResponseFrame, LogMetadata, LogOrigin, LogsOptions,
    MachineId, MachineName, OpaquePayload, RpcError, RpcErrorCode,
};
use serde_json::Value;
use tokio::{io::AsyncWriteExt, sync::mpsc};
use tonic::{Status, Streaming};

use super::{ContainerRuntime, docker_error};
use crate::logs::{
    JournalError, LogLineStream, LogSource, RawLogEntry, RpcStream, serve_logs, split_at_space,
};

impl ContainerRuntime {
    pub async fn exec(&self, mut requests: Streaming<OpaquePayload>) -> Result<RpcStream, Status> {
        let first = requests
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("exec stream is empty"))?;
        let ExecRequestFrame::Config(config) = ExecRequestFrame::decode(&first)
            .map_err(|error| Status::invalid_argument(error.to_string()))?
        else {
            return Err(Status::invalid_argument(
                "first exec stream frame must contain config",
            ));
        };
        if config.options.command.is_empty() {
            return Err(Status::invalid_argument("exec command cannot be empty"));
        }
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
        let (sender, receiver) = mpsc::channel(32);
        send_exec(&sender, ExecResponseFrame::ExecId(created.id.clone())).await?;
        match started {
            StartExecResults::Detached => {
                return Ok(tokio_stream::wrappers::ReceiverStream::new(receiver));
            }
            StartExecResults::Attached {
                mut output,
                mut input,
            } => {
                let stdin_task = if config.options.attach_stdin {
                    let docker = self.docker.client.clone();
                    let exec_id = created.id.clone();
                    let sender = sender.clone();
                    let tty = config.options.tty;
                    Some(tokio::spawn(async move {
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
                    }))
                } else {
                    None
                };
                let docker = self.docker.client.clone();
                let exec_id = created.id;
                let tty = config.options.tty;
                tokio::spawn(async move {
                    let _ = forward_attached_exec(
                        &docker,
                        &exec_id,
                        &mut output,
                        tty,
                        &sender,
                        stdin_task,
                    )
                    .await;
                });
            }
        }
        Ok(tokio_stream::wrappers::ReceiverStream::new(receiver))
    }

    pub async fn container_logs(
        &self,
        machine_id: &MachineId,
        machine_name: &MachineName,
        request: ContainerLogsRequest,
    ) -> Result<RpcStream, Status> {
        let observation = self
            .inspect_managed(&request.container_id, machine_id)
            .await
            .map_err(docker_status)?;
        let metadata = LogMetadata {
            origin: LogOrigin::Service {
                service_id: observation.service_id(),
                service_name: observation.resolved_spec.name.clone(),
                container_id: observation.container_id,
                hook: observation.labels.get(super::LABEL_HOOK).cloned(),
            },
            machine_id: *machine_id,
            machine_name: machine_name.clone(),
        };
        let source = self.raw_logs(request.container_id.as_str(), &request.options)?;
        Ok(serve_logs(source, metadata, request.options.follow))
    }

    #[allow(clippy::result_large_err)]
    pub fn raw_logs(&self, container: &str, options: &LogsOptions) -> Result<LogSource, Status> {
        let options = docker_log_options(options)?;
        let stream = self
            .docker
            .client
            .logs(container, Some(options))
            .map(|entry| {
                entry
                    .map(parse_log_output)
                    .map_err(|error| JournalError::Docker(error.to_string()))
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
            next = output.next() => match next {
                Some(Ok(output)) => {
                    if let Err(error) = send_exec(sender, exec_output(output, tty)).await {
                        break Err(error);
                    }
                }
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
            // ponytail: one bounded drain preserves buffered frames without waiting on a held hijack.
            let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout_at(deadline, output.next()).await {
                    Ok(Some(Ok(output))) => send_exec(sender, exec_output(output, tty)).await?,
                    Ok(Some(Err(error))) => return send_exec_error(sender, error).await,
                    Ok(None) | Err(_) => break,
                }
            }
            send_exec_exit(sender, exit_code).await
        }
    }
}

async fn wait_for_exec_exit(
    docker: &bollard::Docker,
    exec_id: &str,
) -> Result<Option<i64>, bollard::errors::Error> {
    loop {
        // ponytail: one inspect per 100 ms; use a Docker exec wait API if one appears.
        tokio::time::sleep(Duration::from_millis(100)).await;
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
        .map_err(|error| Status::internal(error.to_string()))?;
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
        ExecResponseFrame::Error(RpcError {
            code: RpcErrorCode::Internal,
            message: error.to_string(),
            details: Value::Null,
        }),
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
        RpcErrorCode::Ambiguous => tonic::Code::FailedPrecondition,
        RpcErrorCode::Conflict => tonic::Code::AlreadyExists,
        RpcErrorCode::Unavailable => tonic::Code::Unavailable,
        RpcErrorCode::Unsupported => tonic::Code::Unimplemented,
        RpcErrorCode::Internal | RpcErrorCode::Unknown(_) => tonic::Code::Internal,
        RpcErrorCode::Unauthenticated => tonic::Code::Unauthenticated,
    };
    Status::new(code, error.to_string())
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

#[allow(clippy::result_large_err)]
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
    if let Some(since) = options.since_unix_seconds {
        builder = builder.since(docker_log_timestamp(since)?);
    }
    if let Some(until) = options.until_unix_seconds {
        builder = builder.until(docker_log_timestamp(until)?);
    }
    Ok(builder.build())
}

#[allow(clippy::result_large_err)]
fn docker_log_timestamp(timestamp: i64) -> Result<i32, Status> {
    i32::try_from(timestamp)
        .map_err(|_| Status::invalid_argument(format!("log time {timestamp} is out of range")))
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;

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
    fn docker_log_bounds_are_absolute_unix_seconds() {
        let options = docker_log_options(&LogsOptions {
            follow: false,
            tail: 100,
            since_unix_seconds: Some(1_786_698_000),
            until_unix_seconds: Some(1_786_701_600),
        })
        .unwrap();

        assert_eq!(options.since, 1_786_698_000);
        assert_eq!(options.until, 1_786_701_600);
    }
}
