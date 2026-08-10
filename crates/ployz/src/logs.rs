//! Human-name service resolution and bounded log tail/follow.

use std::time::{Duration, Instant};

use hyper::Method;
use ployz_core::{
    ServiceLogLine, ServiceLogStream, ServiceLogsFollowEvent, ServiceLogsRefusal,
    ServiceLogsRequest, ServiceLogsTailReply, service_logs_follow_route, service_logs_tail_route,
};

use crate::commands::LogsCommand;
use crate::mesh::http::{
    DEFAULT_MESH_SSE_IDLE_TIMEOUT, JsonReply, MAX_MESH_SSE_FRAME_BYTES, SseReply,
};
use crate::remote::{OperatorRemote, OperatorRemoteError};

const RECONNECT_WINDOW: Duration = Duration::from_secs(15);
const RECONNECT_DELAYS: &[Duration] = &[
    Duration::from_millis(100),
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];
const GAP_LINE: &str = "--- log stream gap: lines may have been lost while reconnecting ---\n";
const TAIL_TRUNCATION_LINE: &str =
    "--- log tail truncated: additional bytes were omitted by the server bound ---\n";

pub async fn execute(command: LogsCommand) -> Result<String, LogsExecutionError> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    execute_to(command, &mut stdout).await?;
    Ok(String::new())
}

pub async fn execute_to(
    command: LogsCommand,
    output: &mut impl std::io::Write,
) -> Result<(), LogsExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let request = ServiceLogsRequest {
        tail_lines: Some(command.tail_lines),
        machine: command.machine.clone(),
    };
    if !command.follow {
        let tail = remote
            .request_json_with_refusal::<_, ServiceLogsTailReply, ServiceLogsRefusal>(
                Method::POST,
                &service_logs_tail_route(&command.namespace_name, &command.service_name),
                Some(&request),
            )
            .await?;
        match tail {
            JsonReply::Success(reply) => {
                render_tail_reply(output, reply)?;
            }
            JsonReply::Refused(refusal) => return Err(refusal.into()),
        }
        return Ok(());
    }

    let route = service_logs_follow_route(&command.namespace_name, &command.service_name);
    // A reconnect keeps its machine selector but replays no existing lines.
    let reconnect_request = ServiceLogsRequest {
        tail_lines: None,
        machine: command.machine.clone(),
    };
    let mut reconnect_started: Option<Instant> = None;
    let mut reconnect = 0usize;
    let mut pending_gap = false;
    let mut first_attach = true;
    loop {
        let body = if first_attach {
            Some(&request)
        } else if command.machine.is_some() {
            Some(&reconnect_request)
        } else {
            None
        };
        let attach = remote.request_sse_with_refusal::<
                ServiceLogsRequest,
                ServiceLogsFollowEvent,
                ServiceLogsRefusal,
            >(
                Method::POST,
                &route,
                body,
            DEFAULT_MESH_SSE_IDLE_TIMEOUT,
            MAX_MESH_SSE_FRAME_BYTES,
            );
        let reply = if let Some(started) = reconnect_started {
            let Some(remaining) = RECONNECT_WINDOW.checked_sub(started.elapsed()) else {
                return Err(LogsExecutionError::StreamDown {
                    last_error: "log stream reconnect window elapsed".to_owned(),
                });
            };
            match tokio::time::timeout(remaining, attach).await {
                Ok(reply) => reply,
                Err(_) => {
                    return Err(LogsExecutionError::StreamDown {
                        last_error: "log stream reconnect window elapsed".to_owned(),
                    });
                }
            }
        } else {
            attach.await
        };
        let mut stream = match reply {
            Ok(SseReply::Stream(stream)) => stream,
            Ok(SseReply::Refused(refusal)) => return Err(refusal.into()),
            Err(error) => {
                wait_to_reconnect(&mut reconnect_started, &mut reconnect, error.to_string())
                    .await?;
                if !first_attach {
                    pending_gap = true;
                }
                continue;
            }
        };
        first_attach = false;

        let disconnect_reason = loop {
            let next_event = if let Some(started) = reconnect_started {
                let Some(remaining) = RECONNECT_WINDOW.checked_sub(started.elapsed()) else {
                    return Err(LogsExecutionError::StreamDown {
                        last_error: "log stream reconnect window elapsed".to_owned(),
                    });
                };
                match tokio::time::timeout(remaining, stream.next_event()).await {
                    Ok(event) => event,
                    Err(_) => {
                        return Err(LogsExecutionError::StreamDown {
                            last_error: "log stream reconnect window elapsed".to_owned(),
                        });
                    }
                }
            } else {
                stream.next_event().await
            };
            match next_event {
                Ok(Some(envelope)) => {
                    let expected_name = envelope.data.event_name();
                    if envelope.event.as_deref() != Some(expected_name) {
                        return Err(LogsExecutionError::UnexpectedEventName {
                            expected: expected_name,
                            found: envelope.event,
                        });
                    }
                    if let Some(refusal) =
                        render_follow_event(output, envelope.data, &mut pending_gap)?
                    {
                        return Err(refusal.into());
                    }
                    reconnect_started = None;
                    reconnect = 0;
                }
                Ok(None) => break "log stream ended before a terminal event".to_owned(),
                Err(error) => break error.to_string(),
            }
        };
        wait_to_reconnect(&mut reconnect_started, &mut reconnect, disconnect_reason).await?;
        pending_gap = true;
    }
}

fn render_tail_reply(
    output: &mut impl std::io::Write,
    reply: ServiceLogsTailReply,
) -> Result<(), LogsExecutionError> {
    for line in reply.lines {
        render_line(output, &line)?;
    }
    if reply.truncated {
        output
            .write_all(TAIL_TRUNCATION_LINE.as_bytes())
            .map_err(LogsExecutionError::Output)?;
        output.flush().map_err(LogsExecutionError::Output)?;
    }
    Ok(())
}

fn render_follow_event(
    output: &mut impl std::io::Write,
    event: ServiceLogsFollowEvent,
    pending_gap: &mut bool,
) -> Result<Option<ServiceLogsRefusal>, LogsExecutionError> {
    match event {
        ServiceLogsFollowEvent::Line { log } => {
            if *pending_gap {
                output
                    .write_all(GAP_LINE.as_bytes())
                    .map_err(LogsExecutionError::Output)?;
                *pending_gap = false;
            }
            render_line(output, &log)?;
            Ok(None)
        }
        ServiceLogsFollowEvent::Gap => {
            *pending_gap = false;
            output
                .write_all(GAP_LINE.as_bytes())
                .map_err(LogsExecutionError::Output)?;
            output.flush().map_err(LogsExecutionError::Output)?;
            Ok(None)
        }
        ServiceLogsFollowEvent::Terminal { refusal } => Ok(Some(refusal)),
    }
}

fn render_line(
    output: &mut impl std::io::Write,
    log: &ServiceLogLine,
) -> Result<(), LogsExecutionError> {
    match log.stream {
        ServiceLogStream::Stdout => writeln!(output, "{}", log.line),
        ServiceLogStream::Stderr => writeln!(output, "[stderr] {}", log.line),
    }
    .map_err(LogsExecutionError::Output)?;
    output.flush().map_err(LogsExecutionError::Output)
}

async fn wait_to_reconnect(
    started: &mut Option<Instant>,
    reconnect: &mut usize,
    last_error: String,
) -> Result<(), LogsExecutionError> {
    let started = *started.get_or_insert_with(Instant::now);
    let Some(delay) = RECONNECT_DELAYS.get(*reconnect).copied() else {
        return Err(LogsExecutionError::StreamDown { last_error });
    };
    if started.elapsed().saturating_add(delay) > RECONNECT_WINDOW {
        return Err(LogsExecutionError::StreamDown { last_error });
    }
    *reconnect += 1;
    tokio::time::sleep(delay).await;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum LogsExecutionError {
    #[error(transparent)]
    Remote(#[from] OperatorRemoteError),
    #[error("log stream event name was {found:?}, expected {expected}")]
    UnexpectedEventName {
        expected: &'static str,
        found: Option<String>,
    },
    #[error("log stream remained unavailable after bounded reconnects: {last_error}")]
    StreamDown { last_error: String },
    #[error("cannot write service logs: {0}")]
    Output(std::io::Error),
    #[error("service {namespace_name}/{service_name} was not found")]
    ServiceNotFound {
        namespace_name: String,
        service_name: String,
    },
    #[error("service {namespace_name}/{service_name} has no current container testimony")]
    ContainerNotFound {
        namespace_name: String,
        service_name: String,
    },
    #[error("service {namespace_name}/{service_name} has ambiguous local containers")]
    ContainerAmbiguous {
        namespace_name: String,
        service_name: String,
    },
    #[error(
        "this service runs containers on machines {machines}; pick one with `ployz logs <namespace> <service> --machine <machine>`"
    )]
    MachineSelectorRequired { machines: String },
    #[error(
        "replicas of this service stack on machine {machine}; per-container log selection is not yet supported"
    )]
    StackedReplicas { machine: String },
    #[error("service logs are owned by machine {machine}; point --target at that machine")]
    RemoteOwner { machine: String },
    #[error("service log runtime is unavailable on machine {machine_id}")]
    RuntimeUnavailable { machine_id: String },
}

impl From<ServiceLogsRefusal> for LogsExecutionError {
    fn from(refusal: ServiceLogsRefusal) -> Self {
        match refusal {
            ServiceLogsRefusal::ServiceNotFound {
                namespace_name,
                service_name,
            } => Self::ServiceNotFound {
                namespace_name: namespace_name.to_string(),
                service_name: service_name.to_string(),
            },
            ServiceLogsRefusal::ContainerNotFound {
                namespace_name,
                service_name,
            } => Self::ContainerNotFound {
                namespace_name: namespace_name.to_string(),
                service_name: service_name.to_string(),
            },
            ServiceLogsRefusal::ContainerAmbiguous {
                namespace_name,
                service_name,
            } => Self::ContainerAmbiguous {
                namespace_name: namespace_name.to_string(),
                service_name: service_name.to_string(),
            },
            ServiceLogsRefusal::MachineSelectorRequired { machines } => {
                let distinct = machines
                    .iter()
                    .map(|machine| machine.as_str())
                    .collect::<std::collections::BTreeSet<_>>();
                match distinct.iter().next() {
                    // Every entry names the same machine: replicas stack
                    // there, and no machine selector can split them.
                    Some(machine) if distinct.len() == 1 && machines.len() > 1 => {
                        Self::StackedReplicas {
                            machine: (*machine).to_owned(),
                        }
                    }
                    Some(_) | None => Self::MachineSelectorRequired {
                        machines: machines
                            .iter()
                            .map(|machine| machine.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                    },
                }
            }
            ServiceLogsRefusal::RemoteOwner {
                machine_id,
                machine_name,
            } => Self::RemoteOwner {
                machine: machine_name
                    .map_or_else(|| machine_id.to_string(), |name| name.as_str().to_owned()),
            },
            ServiceLogsRefusal::RuntimeUnavailable { machine_id } => Self::RuntimeUnavailable {
                machine_id: machine_id.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_gap_copy_is_explicit() {
        assert!(GAP_LINE.contains("gap"));
        assert!(GAP_LINE.contains("lost"));
    }

    #[test]
    fn reconnect_emits_exactly_one_gap_before_the_first_new_line() {
        let mut output = Vec::new();
        let mut pending_gap = true;
        for line in ["first", "second"] {
            render_follow_event(
                &mut output,
                ServiceLogsFollowEvent::Line {
                    log: ServiceLogLine {
                        stream: ServiceLogStream::Stdout,
                        line: line.to_owned(),
                    },
                },
                &mut pending_gap,
            )
            .expect("event renders");
        }
        let output = String::from_utf8(output).expect("utf8");
        assert_eq!(output.matches(GAP_LINE.trim()).count(), 1);
        assert!(output.ends_with("first\nsecond\n"));
    }

    #[test]
    fn a_bounded_tail_surfaces_server_truncation() {
        let mut output = Vec::new();
        render_tail_reply(
            &mut output,
            ServiceLogsTailReply {
                lines: vec![ServiceLogLine {
                    stream: ServiceLogStream::Stderr,
                    line: "partial evidence".to_owned(),
                }],
                truncated: true,
            },
        )
        .expect("tail renders");
        let output = String::from_utf8(output).expect("utf8");
        assert!(output.starts_with("[stderr] partial evidence\n"));
        assert!(output.ends_with(TAIL_TRUNCATION_LINE));
    }

    #[test]
    fn replica_refusals_name_their_resolving_flags() {
        let name = |value: &str| ployz_core::machine::MachineName::try_new(value).expect("name");
        let spread = LogsExecutionError::from(ServiceLogsRefusal::MachineSelectorRequired {
            machines: vec![name("edge-a"), name("edge-b")],
        });
        let copy = spread.to_string();
        assert!(copy.contains("edge-a, edge-b"), "{copy:?}");
        assert!(copy.contains("--machine <machine>"), "{copy:?}");

        let stacked = LogsExecutionError::from(ServiceLogsRefusal::MachineSelectorRequired {
            machines: vec![name("edge-a"), name("edge-a")],
        });
        let copy = stacked.to_string();
        assert!(copy.contains("stack on machine edge-a"), "{copy:?}");

        let machine_id = ployz_core::ids::MachineName::try_new("machine-a").expect("machine");
        let remote = LogsExecutionError::from(ServiceLogsRefusal::RemoteOwner {
            machine_id: machine_id.clone(),
            machine_name: Some(name("edge-b")),
        });
        let copy = remote.to_string();
        assert!(copy.contains("machine edge-b"), "{copy:?}");
        assert!(copy.contains("--target"), "{copy:?}");

        let unnamed = LogsExecutionError::from(ServiceLogsRefusal::RemoteOwner {
            machine_id: machine_id.clone(),
            machine_name: None,
        });
        assert!(unnamed.to_string().contains(&machine_id.to_string()));
    }

    #[test]
    fn follow_reconnects_keep_the_machine_selector_without_replaying() {
        let request = ServiceLogsRequest {
            tail_lines: None,
            machine: Some(
                ployz_core::machine::MachineName::try_new("edge-a").expect("machine name"),
            ),
        };
        assert_eq!(
            serde_json::to_value(&request).expect("request serializes"),
            serde_json::json!({ "machine": "edge-a" })
        );
    }
}
