//! On-demand Container log transport for SDK readers.
use crate::cluster::{Call, Deadline};
use ployz_core::error_chain;
use ployz_core::{
    ContainerDetails, ContainerId, InspectContainerRequest, LiveLogTarget, LogBody, LogEntry,
    LogsOptions, MachineId, MachineTarget, OpaquePayload, RpcError, RpcErrorCode, TailLogsRequest,
    op,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::connect::{Client, ConnectError};
use crate::sdk::Session;

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
    /// Open a Docker tail, or a tail-and-follow stream.
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

/// What Docker says about a Container now, `None` when its Machine does not answer or no
/// longer has it.
pub(crate) async fn inspect_container(
    client: &Client,
    machine: MachineId,
    container: ContainerId,
) -> Option<ContainerDetails> {
    let target = MachineTarget::from(&machine);
    client
        .call_shared::<op::InspectContainer>(
            InspectContainerRequest {
                container_id: container,
            },
            Call::once().to(&target).within(Deadline::Target(TAIL_READ)),
        )
        .await
        .ok()
}

/// The env that scrubs a Container's lines, as Docker has it. `None` when Docker does not
/// answer, which withholds the lines.
pub(crate) async fn container_env(
    client: &Client,
    machine: MachineId,
    container: ContainerId,
) -> Option<std::collections::BTreeMap<String, String>> {
    inspect_container(client, machine, container)
        .await?
        .environment
}

/// One line Docker holds for a Container: when it was written and its bytes.
pub(super) struct DockerLine {
    pub(super) timestamp_nanos: i64,
    pub(super) text: Vec<u8>,
}

/// The newest `tail` lines Docker holds for `container` since `since_nanos`, oldest first. The
/// Machine masks the Sealed values it knows before sending them.
pub(super) async fn docker_lines(
    client: &Client,
    machine: MachineId,
    container: ContainerId,
    tail: i32,
    since_nanos: Option<i64>,
) -> Result<Vec<DockerLine>, RpcError> {
    let target = MachineTarget::from(&machine);
    let request = op::TailLogs::into_request(TailLogsRequest {
        target: LiveLogTarget::Container(container),
        options: LogsOptions {
            tail,
            follow: false,
            since_nanos,
            until_unix_seconds: None,
        },
    })
    .encode()
    .map_err(|error| error_chain::rpc_error(RpcErrorCode::InvalidArgument, &error))?;
    let mut stream = client
        .open_stream::<op::TailLogs>(Some(&target), tokio_stream::once(request))
        .await
        .map_err(|error| RpcError::from(ConnectError::Rpc(error)))?;
    let mut lines = Vec::new();
    while let Some(payload) = stream
        .message()
        .await
        .map_err(|error| RpcError::from(ConnectError::from(error)))?
    {
        let entry = LogEntry::decode(&payload)
            .map_err(|error| error_chain::rpc_error(RpcErrorCode::InvalidArgument, &error))?;
        match entry.body {
            LogBody::Stdout(text) | LogBody::Stderr(text) => lines.push(DockerLine {
                timestamp_nanos: entry.timestamp_unix_nanos,
                text,
            }),
            LogBody::Error(message) => return Err(RpcError::unavailable(message)),
            LogBody::Heartbeat => {}
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connect::test_support::rpc_stream_client;
    use crate::sdk::SessionInner;
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

    #[tokio::test(start_paused = true)]
    async fn silent_live_reader_waits_for_its_next_line() {
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
    }

    #[test]
    fn malformed_log_frame_is_an_error_not_empty_output() {
        let payload = OpaquePayload { json: vec![0xff] };
        assert!(LogEntry::decode(&payload).is_err());
    }
}
