//! On-demand Container log transport for SDK readers.
use crate::cluster::{Call, Deadline};
use ployz_core::error_chain;
use ployz_core::{
    ContainerId, HistoryContainer, HistoryGapReason, HistoryRow, HistoryStream, LiveLogTarget,
    LogBody, LogDirection, LogEntry, LogHistoryRequest, LogLevel, LogsOptions, MachineId,
    MachineTarget, OpaquePayload, RpcError, RpcErrorCode, TailLogsRequest, log_level, op,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::connect::{Client, ConnectError};
use crate::sdk::Session;
use ployz_core::{InspectContainerRequest, RetainedEnvRequest};
pub(in crate::sdk) const ROW_DEADLINE: Duration = Duration::from_secs(10);

/// One running Container's Docker output on one Machine.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerLogInput {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub tail: i32,
    pub follow: bool,
    pub since_unix_seconds: Option<i64>,
}

/// One page of one Machine's Log Store. Nanosecond bounds are decimal
/// strings so JavaScript keeps their precision; `since` is inclusive and
/// `until` exclusive.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogHistoryInput {
    pub machine_id: MachineId,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub deployment: Option<String>,
    #[serde(default)]
    pub container_id: Option<ContainerId>,
    #[serde(default)]
    pub since_nanos: Option<String>,
    #[serde(default)]
    pub until_nanos: Option<String>,
    #[serde(default)]
    pub direction: LogDirection,
    pub limit: u16,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// One row of a history page, in the order the store sent it. A page ends
/// with `end`, whose `next` resumes the read.
#[derive(Clone, Debug, Serialize, TS)]
#[serde(tag = "row", rename_all = "snake_case")]
pub enum LogHistoryRecord {
    Container(HistoryContainer),
    Line {
        container_id: ContainerId,
        timestamp_nanos: String,
        stream: HistoryStream,
        level: LogLevel,
        message: String,
    },
    Gap {
        container_id: ContainerId,
        from_nanos: String,
        to_nanos: String,
        reason: HistoryGapReason,
    },
    Exit {
        container_id: ContainerId,
        timestamp_nanos: String,
        exit_code: Option<i64>,
        oom_killed: bool,
    },
    /// A line the scrubbed reader could not check for Sealed values, without its text.
    Hidden {
        container_id: ContainerId,
        timestamp_nanos: String,
    },
    End {
        next: Option<String>,
    },
}

/// Output and source-local diagnostics share a stream without confusing their meaning.
#[derive(Clone, Debug, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LogChannel {
    Stdout,
    Stderr,
    Error,
}

/// Dropping the reader releases this log stream, not the connected session.
pub struct ContainerLogStream {
    cancel: CancellationToken,
    stream: Mutex<tonic::Streaming<OpaquePayload>>,
}
impl Session {
    /// Open a finite history read or a Docker tail-and-follow stream.
    /// # Errors
    /// Returns invalid input, closed-session, or Machine transport errors.
    pub async fn container_logs(
        &self,
        input: ContainerLogInput,
    ) -> Result<ContainerLogStream, RpcError> {
        if !(-1..=1000).contains(&input.tail) {
            return Err(RpcError::invalid_argument("tail must be -1..1000"));
        }
        let client = self.client()?;
        let target = MachineTarget::from(&input.machine_id);
        let request = op::TailLogs::into_request(TailLogsRequest {
            target: LiveLogTarget::Container(input.container_id),
            options: LogsOptions {
                tail: input.tail,
                follow: input.follow,
                since_nanos: input
                    .since_unix_seconds
                    .map(|seconds| seconds.saturating_mul(1_000_000_000)),
                until_unix_seconds: None,
            },
        })
        .encode()
        .map_err(|error| error_chain::rpc_error(RpcErrorCode::InvalidArgument, &error))?;
        let stream = self
            .until_closed(async {
                client
                    .open_stream::<op::TailLogs>(Some(&target), tokio_stream::once(request))
                    .await
                    .map_err(|error| RpcError::from(ConnectError::Rpc(error)))
            })
            .await?;
        Ok(ContainerLogStream {
            cancel: self.inner.cancel.child_token(),
            stream: Mutex::new(stream),
        })
    }
    /// Read one page of one Machine's Log Store, removed Containers included.
    /// # Errors
    /// Returns invalid input, closed-session, or Machine transport errors.
    pub async fn log_history(&self, input: LogHistoryInput) -> Result<LogHistoryStream, RpcError> {
        let request = LogHistoryRequest {
            namespace: input.namespace,
            service: input.service,
            deployment: input.deployment,
            container_id: input.container_id,
            since_nanos: nanos(input.since_nanos.as_deref(), "since_nanos")?,
            until_nanos: nanos(input.until_nanos.as_deref(), "until_nanos")?,
            direction: input.direction,
            limit: input.limit,
            cursor: input.cursor,
        };
        request.validate().map_err(RpcError::invalid_argument)?;
        let payload = op::LogHistory::into_request(request)
            .encode()
            .map_err(|error| error_chain::rpc_error(RpcErrorCode::InvalidArgument, &error))?;
        let client = self.client()?;
        let target = MachineTarget::from(&input.machine_id);
        let stream = self
            .until_closed(async {
                client
                    .open_stream::<op::LogHistory>(Some(&target), tokio_stream::once(payload))
                    .await
                    .map_err(|error| RpcError::from(ConnectError::Rpc(error)))
            })
            .await?;
        Ok(LogHistoryStream {
            cancel: self.inner.cancel.child_token(),
            stream: Mutex::new(stream),
        })
    }
}

/// Dropping the reader stops the store's read, not the connected session.
pub struct LogHistoryStream {
    cancel: CancellationToken,
    stream: Mutex<tonic::Streaming<OpaquePayload>>,
}

impl LogHistoryStream {
    /// The next row as the store sent it, skipping heartbeats. `None` after `end`.
    /// # Errors
    /// Returns the store's error row, malformed frames, transport failures,
    /// and a timeout when the store sends no frame for ten seconds.
    pub(in crate::sdk) async fn row(&self) -> Result<Option<HistoryRow>, RpcError> {
        let mut stream = self.stream.lock().await;
        loop {
            let payload = tokio::select! {
                () = self.cancel.cancelled() => return Ok(None),
                payload = tokio::time::timeout(ROW_DEADLINE, stream.message()) => payload
                    .map_err(|_| RpcError::unavailable("the Server's Log Store stopped answering"))?
                    .map_err(|error| RpcError::from(ConnectError::from(error)))?,
            };
            let Some(payload) = payload else {
                return Ok(None);
            };
            if let Some(row) = history_row(&payload)? {
                return Ok(Some(row));
            }
        }
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub(in crate::sdk) fn cancelled(&self) -> &CancellationToken {
        &self.cancel
    }
}

impl Drop for LogHistoryStream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn nanos(value: Option<&str>, field: &str) -> Result<Option<i64>, RpcError> {
    value
        .map(|value| {
            value.parse().map_err(|_| {
                RpcError::invalid_argument(format!("{field} must be a decimal integer"))
            })
        })
        .transpose()
}

fn history_row(payload: &OpaquePayload) -> Result<Option<HistoryRow>, RpcError> {
    match HistoryRow::decode(payload)
        .map_err(|error| error_chain::rpc_error(RpcErrorCode::InvalidArgument, &error))?
    {
        HistoryRow::Heartbeat => Ok(None),
        HistoryRow::Error(message) => Err(RpcError::unavailable(message)),
        row @ (HistoryRow::Container(_)
        | HistoryRow::Line { .. }
        | HistoryRow::Gap { .. }
        | HistoryRow::Exit { .. }
        | HistoryRow::End { .. }) => Ok(Some(row)),
    }
}

/// `row` as a page row. Heartbeats and errors never get here: [`history_row`] took them.
pub(in crate::sdk) fn history_record(row: HistoryRow) -> LogHistoryRecord {
    match row {
        HistoryRow::Container(container) => LogHistoryRecord::Container(container),
        HistoryRow::Line {
            container_id,
            ts,
            stream,
            text,
        } => LogHistoryRecord::Line {
            container_id,
            timestamp_nanos: ts.to_string(),
            stream,
            level: log_level(&text),
            message: String::from_utf8_lossy(&text).into_owned(),
        },
        HistoryRow::Gap {
            container_id,
            from,
            to,
            reason,
        } => LogHistoryRecord::Gap {
            container_id,
            from_nanos: from.to_string(),
            to_nanos: to.to_string(),
            reason,
        },
        HistoryRow::Exit {
            container_id,
            ts,
            exit_code,
            oom_killed,
        } => LogHistoryRecord::Exit {
            container_id,
            timestamp_nanos: ts.to_string(),
            exit_code,
            oom_killed,
        },
        HistoryRow::End { next } => LogHistoryRecord::End { next },
        HistoryRow::Heartbeat | HistoryRow::Error(_) => {
            unreachable!("history_row takes heartbeats and errors")
        }
    }
}
impl ContainerLogStream {
    /// The next frame as the Machine sent it, skipping heartbeats.
    pub(in crate::sdk) async fn entry(&self) -> Result<Option<LogEntry>, RpcError> {
        let mut stream = self.stream.lock().await;
        loop {
            let payload = tokio::select! {
                () = self.cancel.cancelled() => return Ok(None),
                payload = stream.message() => payload.map_err(|error| RpcError::from(ConnectError::from(error)))?,
            };
            let Some(payload) = payload else {
                return Ok(None);
            };
            let entry = LogEntry::decode(&payload)
                .map_err(|error| error_chain::rpc_error(RpcErrorCode::InvalidArgument, &error))?;
            if !matches!(entry.body, LogBody::Heartbeat) {
                return Ok(Some(entry));
            }
        }
    }
    /// Stop this reader only.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}
impl Drop for ContainerLogStream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// How long reading a failed Container's tail may take.
pub(super) const TAIL_READ: Duration = Duration::from_secs(5);

/// A stored exit, how many of the tail's newest lines the next run printed after it,
/// and, when the exit before it was read, how many lines came after that one.
#[derive(Clone, Copy)]
pub(super) struct Exit {
    pub(super) code: Option<i64>,
    pub(super) lines_after: usize,
    pub(super) run_from: Option<usize>,
}

/// The env that scrubs a Container's lines: Docker's, or the one its Machine's
/// Log Store kept once the Machine answers that the Container is gone. `None`
/// when neither answers, which withholds the lines.
pub(crate) async fn container_env(
    client: &Client,
    machine: MachineId,
    container: ContainerId,
) -> Option<BTreeMap<String, String>> {
    let target = MachineTarget::from(&machine);
    let inspected = client
        .call_shared::<op::InspectContainer>(
            InspectContainerRequest {
                container_id: container,
            },
            Call::once().to(&target).within(Deadline::Target(TAIL_READ)),
        )
        .await
        .map_err(crate::connect::rpc_error);
    match inspected {
        Ok(details) => return details.environment,
        Err(error) if error.code == RpcErrorCode::NotFound => {}
        Err(_) => return None,
    }
    client
        .call_shared::<op::RetainedEnv>(
            RetainedEnvRequest {
                container_id: container,
            },
            Call::once().to(&target).within(Deadline::Target(TAIL_READ)),
        )
        .await
        .map_err(crate::connect::rpc_error)
        .ok()
        .and_then(|reply| reply.environment)
}

/// The newest `fetch` lines `selector` picks, newest first, and the code of the newest
/// exit read with them. The error says why the Log Store could not be read.
pub(super) async fn newest_lines(
    client: &Client,
    target: &MachineTarget,
    selector: LogHistoryRequest,
    fetch: u16,
) -> Result<(Vec<(ContainerId, Vec<u8>)>, Option<Exit>), RpcError> {
    let mut newest = Vec::new();
    let mut exit = None;
    let mut cursor = None;
    loop {
        let request = op::LogHistory::into_request(LogHistoryRequest {
            direction: LogDirection::Backward,
            limit: fetch,
            cursor: cursor.take(),
            ..selector.clone()
        })
        .encode()
        .map_err(|error| error_chain::rpc_error(RpcErrorCode::InvalidArgument, &error))?;
        let mut stream = client
            .open_stream::<op::LogHistory>(Some(target), tokio_stream::once(request))
            .await
            .map_err(|error| RpcError::from(ConnectError::Rpc(error)))?;
        while let Some(payload) = stream
            .message()
            .await
            .map_err(|error| RpcError::from(ConnectError::from(error)))?
        {
            let row = HistoryRow::decode(&payload)
                .map_err(|error| error_chain::rpc_error(RpcErrorCode::InvalidArgument, &error))?;
            match row {
                HistoryRow::Line {
                    container_id, text, ..
                } => newest.push((container_id, text)),
                HistoryRow::End { next } => cursor = next,
                HistoryRow::Error(message) => {
                    return Err(RpcError::unavailable(message));
                }
                HistoryRow::Exit { exit_code, .. } => match &mut exit {
                    None => {
                        exit = Some(Exit {
                            code: exit_code,
                            lines_after: newest.len(),
                            run_from: None,
                        });
                    }
                    Some(newer) => {
                        newer.run_from.get_or_insert(newest.len());
                    }
                },
                HistoryRow::Container(_) | HistoryRow::Gap { .. } | HistoryRow::Heartbeat => {}
            }
        }
        if cursor.is_none() || newest.len() >= usize::from(fetch) {
            break;
        }
    }
    newest.truncate(usize::from(fetch));
    Ok((newest, exit))
}

/// Newest-first lines as frames in the order they were written.
pub(super) fn frames(newest: impl DoubleEndedIterator<Item = Vec<u8>>) -> Vec<String> {
    newest
        .rev()
        .map(|text| format!("{}\n", String::from_utf8_lossy(&text)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connect::test_support::rpc_stream_client;
    use crate::sdk::SessionInner;
    use crate::sdk::logs::agent::service_tail;
    use ployz_store::tools::LineCap;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;

    async fn streaming_session() -> (
        Session,
        mpsc::Sender<Result<OpaquePayload, tonic::Status>>,
        tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    ) {
        let (sender, receiver) = mpsc::channel(4);
        let receiver = Arc::new(std::sync::Mutex::new(Some(receiver)));
        let (client, server) = rpc_stream_client(move |_| {
            let receiver = receiver.lock().unwrap().take().unwrap();
            async move { Ok(tonic::Response::new(ReceiverStream::new(receiver))) }
        })
        .await;
        let session = Session {
            inner: Arc::new(SessionInner {
                client: std::sync::Mutex::new(Some(client)),
                cancel: CancellationToken::new(),
            }),
        };
        (session, sender, server)
    }

    async fn history_reader(session: &Session) -> LogHistoryStream {
        session
            .log_history(LogHistoryInput {
                machine_id: MachineId::random(),
                namespace: Some("app".into()),
                service: None,
                deployment: None,
                container_id: None,
                since_nanos: None,
                until_nanos: None,
                direction: LogDirection::Backward,
                limit: 200,
                cursor: None,
            })
            .await
            .unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn silent_history_reader_fails_after_ten_seconds() {
        let (session, _sender, server) = streaming_session().await;
        let reader = history_reader(&session).await;
        let started = tokio::time::Instant::now();
        let error = tokio::time::timeout(Duration::from_secs(11), reader.row())
            .await
            .expect("silent history reader did not time out")
            .unwrap_err();
        assert_eq!(started.elapsed(), Duration::from_secs(10));
        assert_eq!(error.code, RpcErrorCode::Unavailable);
        assert_eq!(error.message, "the Server's Log Store stopped answering");
        server.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn history_heartbeats_renew_the_row_deadline() {
        let (session, sender, server) = streaming_session().await;
        let reader = history_reader(&session).await;
        let writer = tokio::spawn(async move {
            for row in [
                HistoryRow::Heartbeat,
                HistoryRow::Heartbeat,
                HistoryRow::End { next: None },
            ] {
                tokio::time::sleep(Duration::from_secs(9)).await;
                sender.send(Ok(row.encode().unwrap())).await.unwrap();
            }
        });
        let started = tokio::time::Instant::now();
        let row = tokio::time::timeout(Duration::from_secs(30), reader.row())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(row, Some(HistoryRow::End { next: None })));
        assert_eq!(started.elapsed(), Duration::from_secs(27));
        writer.await.unwrap();
        server.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_history_reader_stops_a_pending_read_only() {
        let (session, _sender, server) = streaming_session().await;
        let reader = history_reader(&session).await;
        let next = reader.row();
        tokio::pin!(next);
        tokio::select! {
            biased;
            result = &mut next => panic!("silent history read completed before cancellation: {result:?}"),
            () = tokio::task::yield_now() => {},
        }
        reader.cancel();
        assert!(next.await.unwrap().is_none());
        assert!(session.client().is_ok());
        server.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn silent_live_reader_can_wait_past_history_deadline() {
        let (session, sender, server) = streaming_session().await;
        let reader = session
            .container_logs(ContainerLogInput {
                machine_id: MachineId::random(),
                container_id: ContainerId::parse("a".repeat(64)).unwrap(),
                tail: 0,
                follow: true,
                since_unix_seconds: None,
            })
            .await
            .unwrap();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(11)).await;
            drop(sender);
        });
        let started = tokio::time::Instant::now();
        assert!(reader.entry().await.unwrap().is_none());
        assert_eq!(started.elapsed(), Duration::from_secs(11));
        writer.await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn a_server_with_no_lines_has_no_tail() {
        let (session, sender, server) = streaming_session().await;
        sender
            .send(Ok(HistoryRow::End { next: None }.encode().unwrap()))
            .await
            .unwrap();
        drop(sender);
        let selector = LogHistoryRequest {
            namespace: Some("app".into()),
            service: Some("web".into()),
            ..LogHistoryRequest::default()
        };
        let cap = LineCap::new(50).unwrap();
        let client = session.client().unwrap();
        let tail = service_tail(&client, MachineId::random(), selector, cap, None)
            .await
            .unwrap();
        assert!(tail.is_none(), "{tail:?}");
        server.abort();
    }

    #[tokio::test]
    async fn invalid_log_options_fail_before_opening_a_session() {
        let session = Session {
            inner: Arc::new(SessionInner {
                client: std::sync::Mutex::new(None),
                cancel: CancellationToken::new(),
            }),
        };
        let input = ContainerLogInput {
            machine_id: MachineId::random(),
            container_id: ContainerId::parse("a".repeat(64)).unwrap(),
            tail: 1001,
            follow: true,
            since_unix_seconds: None,
        };
        let Err(error) = session.container_logs(input).await else {
            panic!("invalid log options opened a stream");
        };
        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
        for (limit, since) in [(0, None), (5001, None), (200, Some("soon"))] {
            let input = LogHistoryInput {
                machine_id: MachineId::random(),
                namespace: Some("shop".into()),
                service: None,
                deployment: None,
                container_id: None,
                since_nanos: since.map(str::to_owned),
                until_nanos: None,
                direction: LogDirection::Backward,
                limit,
                cursor: None,
            };
            let Err(error) = session.log_history(input).await else {
                panic!("invalid history options opened a stream");
            };
            assert_eq!(error.code, RpcErrorCode::InvalidArgument);
        }
    }

    #[test]
    fn malformed_log_frame_is_an_error_not_empty_output() {
        let payload = OpaquePayload { json: vec![0xff] };
        assert!(LogEntry::decode(&payload).is_err());
        assert!(history_row(&payload).is_err());
    }
}
