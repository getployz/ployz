use std::{
    io,
    pin::Pin,
    process::{ExitStatus, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::{Stream, StreamExt};
use ployz_core::{LogBody, LogEntry, LogMetadata, LogsOptions, OpaquePayload};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::mpsc,
    time::MissedTickBehavior,
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;

const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("read journal logs: {0}")]
    Read(io::Error),
    #[error("journalctl exited with {0}")]
    Exit(ExitStatus),
    #[error("wait for journalctl: {0}")]
    Wait(io::Error),
    #[error("{0}")]
    Docker(String),
}

pub type LogSource = Pin<Box<dyn Stream<Item = Result<RawLogEntry, JournalError>> + Send>>;
pub type RpcStream = ReceiverStream<Result<OpaquePayload, Status>>;

/// Journal or Docker line stream: stdout or stderr, never a control frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogLineStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawLogEntry {
    pub stream: LogLineStream,
    pub timestamp_unix_nanos: i64,
    pub message: Vec<u8>,
}

#[must_use]
pub fn serve_logs(mut source: LogSource, metadata: LogMetadata, follow: bool) -> RpcStream {
    let (sender, receiver) = mpsc::channel(100);
    tokio::spawn(async move {
        let started = tokio::time::Instant::now();
        let mut last_sent = None;
        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        heartbeat.tick().await;
        loop {
            tokio::select! {
                entry = source.next() => match entry {
                    Some(Ok(entry)) => {
                        last_sent = Some(tokio::time::Instant::now());
                        let entry = LogEntry {
                            metadata: metadata.clone(),
                            timestamp_unix_nanos: entry.timestamp_unix_nanos,
                            body: match entry.stream {
                                LogLineStream::Stdout => LogBody::Stdout(entry.message),
                                LogLineStream::Stderr => LogBody::Stderr(entry.message),
                            },
                        };
                        if send_entry(&sender, entry).await.is_err() {
                            return;
                        }
                    }
                    Some(Err(error)) => {
                        let _ = send_entry(&sender, LogEntry::error(metadata.clone(), error.to_string())).await;
                        return;
                    }
                    None => return,
                },
                now = heartbeat.tick(), if follow => {
                    if last_sent.is_some_and(|last| now.duration_since(last) < HEARTBEAT_INTERVAL)
                        || last_sent.is_none() && started.elapsed() < HEARTBEAT_INTERVAL
                    {
                        continue;
                    }
                    let timestamp = system_time_nanos(SystemTime::now() - HEARTBEAT_INTERVAL);
                    if send_entry(&sender, LogEntry::heartbeat(metadata.clone(), timestamp)).await.is_err() {
                        return;
                    }
                    last_sent = Some(now);
                }
            }
        }
    });
    ReceiverStream::new(receiver)
}

async fn send_entry(
    sender: &mpsc::Sender<Result<OpaquePayload, Status>>,
    entry: LogEntry,
) -> Result<(), ()> {
    let payload = entry
        .encode()
        .map_err(|error| Status::internal(error.to_string()));
    sender.send(payload).await.map_err(|_| ())
}

pub async fn open_journal_logs(unit: &str, options: &LogsOptions) -> Result<LogSource, Status> {
    let mut command = Command::new("journalctl");
    command.args(["-u", unit, "--no-hostname", "-n"]);
    command.arg(if options.tail < 0 {
        "all".to_owned()
    } else {
        options.tail.to_string()
    });
    if options.follow {
        command.arg("-f");
    }
    command.args(["-o", "short-unix"]);
    if !options.since.is_empty() {
        command.args(["-S", &options.since]);
    }
    if !options.until.is_empty() {
        command.args(["-U", &options.until]);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| Status::internal(format!("start journalctl: {error}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Status::internal("journalctl stdout was not piped"))?;
    let (sender, receiver) = mpsc::channel(100);
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    if sender.send(Ok(parse_journal_entry(&line))).await.is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(JournalError::Read(error))).await;
                    return;
                }
            }
        }
        match child.wait().await {
            Ok(status) if status.success() => {}
            Ok(status) => {
                let _ = sender.send(Err(JournalError::Exit(status))).await;
            }
            Err(error) => {
                let _ = sender.send(Err(JournalError::Wait(error))).await;
            }
        }
    });
    Ok(Box::pin(ReceiverStream::new(receiver)))
}

fn parse_journal_entry(line: &[u8]) -> RawLogEntry {
    let (timestamp, message) = split_at_space(line)
        .and_then(|(timestamp, message)| parse_short_unix(timestamp).map(|time| (time, message)))
        .unwrap_or((0, line));
    RawLogEntry {
        stream: LogLineStream::Stdout,
        timestamp_unix_nanos: timestamp,
        message: message.to_vec(),
    }
}

pub(crate) fn split_at_space(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let at = bytes.iter().position(|byte| *byte == b' ')?;
    let (left, right) = bytes.split_at(at);
    Some((left, right.get(1..).unwrap_or_default()))
}

fn parse_short_unix(value: &[u8]) -> Option<i64> {
    let value = std::str::from_utf8(value).ok()?;
    let (seconds, micros) = value.split_once('.')?;
    let seconds = seconds.parse::<i64>().ok()?;
    let micros = micros.parse::<i64>().ok()?;
    Some(seconds.saturating_mul(1_000_000_000) + micros.saturating_mul(1_000))
}

fn system_time_nanos(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use ployz_core::{LogOrigin, MachineId, MachineLogService, MachineName};

    use super::*;

    #[test]
    fn journal_entries_preserve_raw_message_bytes() {
        assert_eq!(
            parse_journal_entry(b"1758193407.686964 systemd[1]: \xff\n"),
            RawLogEntry {
                stream: LogLineStream::Stdout,
                timestamp_unix_nanos: 1_758_193_407_686_964_000,
                message: b"systemd[1]: \xff\n".to_vec(),
            }
        );
    }

    #[tokio::test]
    async fn serve_logs_maps_line_streams_and_source_errors_to_log_bodies() {
        let (sender, receiver) = mpsc::channel(4);
        let mut stream = serve_logs(
            Box::pin(ReceiverStream::new(receiver)),
            test_metadata(),
            false,
        );
        sender
            .send(Ok(RawLogEntry {
                stream: LogLineStream::Stdout,
                timestamp_unix_nanos: 1,
                message: b"out\n".to_vec(),
            }))
            .await
            .unwrap();
        sender
            .send(Ok(RawLogEntry {
                stream: LogLineStream::Stderr,
                timestamp_unix_nanos: 2,
                message: b"err\n".to_vec(),
            }))
            .await
            .unwrap();
        sender
            .send(Err(JournalError::Docker("docker died".into())))
            .await
            .unwrap();

        assert_eq!(
            decode_next(&mut stream).await.body,
            LogBody::Stdout(b"out\n".to_vec())
        );
        assert_eq!(
            decode_next(&mut stream).await.body,
            LogBody::Stderr(b"err\n".to_vec())
        );
        assert_eq!(
            decode_next(&mut stream).await.body,
            LogBody::Error("docker died".into())
        );
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn serve_logs_emits_heartbeat_bodies_while_following() {
        let (_sender, receiver) = mpsc::channel(1);
        let mut stream = serve_logs(
            Box::pin(ReceiverStream::new(receiver)),
            test_metadata(),
            true,
        );
        let payload = tokio::time::timeout(HEARTBEAT_INTERVAL * 4, stream.next())
            .await
            .expect("heartbeat within a few intervals")
            .unwrap()
            .unwrap();
        assert_eq!(LogEntry::decode(&payload).unwrap().body, LogBody::Heartbeat);
    }

    async fn decode_next(stream: &mut RpcStream) -> LogEntry {
        let payload = stream.next().await.unwrap().unwrap();
        LogEntry::decode(&payload).unwrap()
    }

    fn test_metadata() -> LogMetadata {
        LogMetadata {
            origin: LogOrigin::Machine {
                service: MachineLogService::Ployz,
            },
            machine_id: MachineId::parse("a".repeat(32)).unwrap(),
            machine_name: MachineName::parse("machine").unwrap(),
        }
    }
}
