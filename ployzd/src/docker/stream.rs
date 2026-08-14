use std::str::FromStr;

use bollard::{
    container::LogOutput,
    exec::{CreateExecOptions, ResizeExecOptions, StartExecOptions, StartExecResults},
    query_parameters::LogsOptionsBuilder,
};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use futures_util::StreamExt;
use ployz_core::{
    ContainerLogsRequest, ExecRequestFrame, ExecResponseFrame, LogMetadata, LogOrigin, LogStream,
    LogsOptions, MachineId, MachineName, OpaquePayload, RpcError, RpcErrorCode,
};
use serde_json::Value;
use tokio::{io::AsyncWriteExt, sync::mpsc};
use tonic::{Status, Streaming};

use super::{LocalDocker, MachineSpecStore, docker_error};
use crate::logs::{LogSource, RawLogEntry, RpcStream, serve_logs, split_at_space};

impl LocalDocker {
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
            .client
            .create_exec(config.container_id.as_str(), create)
            .await
            .map_err(|error| docker_status(docker_error(&config.container_id, error)))?;
        let started = self
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
                    let docker = self.client.clone();
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
                let docker = self.client.clone();
                let exec_id = created.id;
                let tty = config.options.tty;
                tokio::spawn(async move {
                    let _ = async {
                        while let Some(output) = output.next().await {
                            match output {
                                Ok(output) => {
                                    send_exec(&sender, exec_output(output, tty)).await?;
                                }
                                Err(error) => return send_exec_error(&sender, error).await,
                            }
                        }
                        match docker.inspect_exec(&exec_id).await {
                            Ok(inspected) => {
                                let code = inspected.exit_code.unwrap_or(1);
                                let code = i32::try_from(code).unwrap_or(1);
                                send_exec(&sender, ExecResponseFrame::Exit(code)).await
                            }
                            Err(error) => send_exec_error(&sender, error).await,
                        }
                    }
                    .await;
                    if let Some(task) = stdin_task {
                        task.abort();
                    }
                });
            }
        }
        Ok(tokio_stream::wrappers::ReceiverStream::new(receiver))
    }

    pub async fn container_logs(
        &self,
        machine_id: &MachineId,
        machine_name: &MachineName,
        specs: &MachineSpecStore,
        request: ContainerLogsRequest,
    ) -> Result<RpcStream, Status> {
        let observation = self
            .inspect_managed(&request.container_id, machine_id, specs)
            .await
            .map_err(docker_status)?;
        let metadata = LogMetadata {
            origin: LogOrigin::Service {
                service_id: observation.service_id,
                service_name: observation.service_name,
                container_id: observation.container_id.clone(),
                hook: observation.labels.get(super::LABEL_HOOK).cloned(),
            },
            machine_id: machine_id.clone(),
            machine_name: machine_name.clone(),
        };
        let source = self.raw_logs(request.container_id.as_str(), &request.options)?;
        Ok(serve_logs(source, metadata, request.options.follow))
    }

    #[allow(clippy::result_large_err)]
    pub fn raw_logs(&self, container: &str, options: &LogsOptions) -> Result<LogSource, Status> {
        let options = docker_log_options(options)?;
        let stream = self.client.logs(container, Some(options)).map(|entry| {
            entry
                .map(parse_log_output)
                .map_err(|error| error.to_string())
        });
        Ok(Box::pin(stream))
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

fn docker_status(error: super::Error) -> Status {
    let code = match error.rpc_code() {
        RpcErrorCode::NotFound => tonic::Code::NotFound,
        RpcErrorCode::InvalidArgument => tonic::Code::InvalidArgument,
        RpcErrorCode::Ambiguous => tonic::Code::FailedPrecondition,
        RpcErrorCode::Conflict => tonic::Code::AlreadyExists,
        RpcErrorCode::Unavailable => tonic::Code::Unavailable,
        RpcErrorCode::Unsupported => tonic::Code::Unimplemented,
        RpcErrorCode::Internal | RpcErrorCode::Unknown(_) => tonic::Code::Internal,
    };
    Status::new(code, error.to_string())
}

fn parse_log_output(output: LogOutput) -> RawLogEntry {
    let (stream, bytes) = match output {
        LogOutput::StdErr { message } => (LogStream::Stderr, message),
        LogOutput::StdOut { message }
        | LogOutput::StdIn { message }
        | LogOutput::Console { message } => (LogStream::Stdout, message),
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
    if !options.since.is_empty() {
        builder = builder.since(parse_log_time(&options.since)?);
    }
    if !options.until.is_empty() {
        builder = builder.until(parse_log_time(&options.until)?);
    }
    Ok(builder.build())
}

#[allow(clippy::result_large_err)]
fn parse_log_time(value: &str) -> Result<i32, Status> {
    let timestamp = value
        .parse::<i64>()
        .ok()
        .or_else(|| {
            DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|time| time.timestamp())
        })
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .and_then(|time| Local.from_local_datetime(&time).single())
                .map(|time| time.timestamp())
        })
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .and_then(|time| Local.from_local_datetime(&time).single())
                .map(|time| time.timestamp())
        })
        .or_else(|| parse_duration(value).map(|duration| Utc::now().timestamp() - duration))
        .ok_or_else(|| Status::invalid_argument(format!("invalid log time {value:?}")))?;
    i32::try_from(timestamp)
        .map_err(|_| Status::invalid_argument(format!("log time {value:?} is out of range")))
}

fn parse_duration(value: &str) -> Option<i64> {
    let mut remaining = value;
    let mut seconds = 0_f64;
    while !remaining.is_empty() {
        let number_end =
            remaining.find(|character: char| !character.is_ascii_digit() && character != '.')?;
        let number = f64::from_str(&remaining[..number_end]).ok()?;
        remaining = &remaining[number_end..];
        let unit_end = remaining
            .find(|character: char| character.is_ascii_digit() || character == '.')
            .unwrap_or(remaining.len());
        let (unit, rest) = remaining.split_at(unit_end);
        let multiplier = match unit {
            "h" => 3_600_f64,
            "m" => 60_f64,
            "s" => 1_f64,
            "ms" => 0.001_f64,
            "us" | "µs" => 0.000_001_f64,
            "ns" => 0.000_000_001_f64,
            _ => return None,
        };
        seconds += number * multiplier;
        remaining = rest;
    }
    (seconds.is_finite() && seconds >= 0.0).then_some(seconds as i64)
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
                stream: LogStream::Stderr,
                timestamp_unix_nanos: 1_786_698_611_123_456_789,
                message: b"\xff\n".to_vec(),
            }
        );
    }

    #[test]
    fn relative_log_duration_accepts_compound_values() {
        assert_eq!(parse_duration("2m30s"), Some(150));
        assert_eq!(parse_duration("bad"), None);
    }
}
