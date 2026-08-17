use std::{
    io,
    pin::Pin,
    process::{ExitStatus, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::{Stream, StreamExt};
use ployz_core::{LogBody, LogEntry, LogMetadata, LogStream, LogsOptions, OpaquePayload};
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawLogEntry {
    pub stream: LogStream,
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
                                LogStream::Stdout => LogBody::Stdout(entry.message),
                                LogStream::Stderr => LogBody::Stderr(entry.message),
                                LogStream::Heartbeat => LogBody::Heartbeat,
                                LogStream::Error => LogBody::Error(
                                    String::from_utf8_lossy(&entry.message).into_owned(),
                                ),
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
        stream: LogStream::Stdout,
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
    use super::*;

    #[test]
    fn journal_entries_preserve_raw_message_bytes() {
        assert_eq!(
            parse_journal_entry(b"1758193407.686964 systemd[1]: \xff\n"),
            RawLogEntry {
                stream: LogStream::Stdout,
                timestamp_unix_nanos: 1_758_193_407_686_964_000,
                message: b"systemd[1]: \xff\n".to_vec(),
            }
        );
    }
}
