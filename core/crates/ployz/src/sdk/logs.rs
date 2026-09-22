//! On-demand Container log transport for SDK readers.
use ployz_core::{
    ContainerId, ContainerLogHistoryRequest, ContainerLogsRequest, LogBody, LogEntry, LogMetadata,
    LogsOptions, MachineId, MachineTarget, OpaquePayload, RpcError, op,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use super::{Session, invalid_argument};
use crate::connect::ConnectError;

/// One Machine-local log read. History reads include a complete timestamp boundary.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerLogInput {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub tail: i32,
    pub follow: bool,
    pub before_nanos: Option<String>,
    pub since_unix_seconds: Option<i64>,
}

/// Decoded output preserving nanosecond precision across JavaScript.
#[derive(Clone, Debug, Serialize, TS)]
pub struct ContainerLogRecord {
    pub source: LogMetadata,
    pub timestamp_nanos: String,
    pub channel: LogChannel,
    pub message: String,
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
        if !(-1..=1000).contains(&input.tail)
            || input.before_nanos.is_some() && (input.follow || input.tail <= 0)
        {
            return Err(invalid_argument(
                "tail must be -1..1000; history requires a positive tail and follow=false".into(),
            ));
        }
        let client = self.client()?;
        let target = MachineTarget::from(&input.machine_id);
        let stream = self
            .until_closed(async {
                let result = if let Some(before_nanos) = input.before_nanos {
                    let request =
                        op::ContainerLogHistory::into_request(ContainerLogHistoryRequest {
                            container_id: input.container_id,
                            limit: u16::try_from(input.tail)
                                .expect("validated positive history limit"),
                            before_nanos,
                        })
                        .encode()
                        .map_err(|error| invalid_argument(error.to_string()))?;
                    client.container_log_history_stream(&target, request).await
                } else {
                    let request = op::ContainerLogs::into_request(ContainerLogsRequest {
                        container_id: input.container_id,
                        options: LogsOptions {
                            tail: input.tail,
                            follow: input.follow,
                            since_unix_seconds: input.since_unix_seconds,
                            until_unix_seconds: None,
                        },
                    })
                    .encode()
                    .map_err(|error| invalid_argument(error.to_string()))?;
                    client.container_logs_stream(&target, request).await
                };
                result.map_err(|error| RpcError::from(ConnectError::Rpc(error)))
            })
            .await?;
        Ok(ContainerLogStream {
            cancel: self.inner.cancel.child_token(),
            stream: Mutex::new(stream),
        })
    }
}
impl ContainerLogStream {
    /// Read the next output record, skipping transport heartbeats.
    /// # Errors
    /// Returns malformed-frame and Machine transport failures.
    pub async fn next(&self) -> Result<Option<ContainerLogRecord>, RpcError> {
        let mut stream = self.stream.lock().await;
        loop {
            let payload = tokio::select! {
                () = self.cancel.cancelled() => return Ok(None),
                payload = stream.message() => payload.map_err(|error| RpcError::from(ConnectError::from(error)))?,
            };
            let Some(payload) = payload else {
                return Ok(None);
            };
            if let Some(record) = decode_record(&payload)? {
                return Ok(Some(record));
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

fn decode_record(payload: &OpaquePayload) -> Result<Option<ContainerLogRecord>, RpcError> {
    let entry = LogEntry::decode(payload).map_err(|error| invalid_argument(error.to_string()))?;
    let (channel, message) = match entry.body {
        LogBody::Stdout(bytes) => (
            LogChannel::Stdout,
            String::from_utf8_lossy(&bytes).into_owned(),
        ),
        LogBody::Stderr(bytes) => (
            LogChannel::Stderr,
            String::from_utf8_lossy(&bytes).into_owned(),
        ),
        LogBody::Error(message) => (LogChannel::Error, message),
        LogBody::Heartbeat => return Ok(None),
    };
    Ok(Some(ContainerLogRecord {
        source: entry.metadata,
        timestamp_nanos: entry.timestamp_unix_nanos.to_string(),
        channel,
        message,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::SessionInner;
    use std::sync::Arc;

    #[tokio::test]
    async fn invalid_log_options_fail_before_opening_a_session() {
        let session = Session {
            inner: Arc::new(SessionInner {
                client: std::sync::Mutex::new(None),
                cancel: CancellationToken::new(),
            }),
        };
        for (tail, follow, before) in [
            (1001, true, None),
            (0, false, Some("1")),
            (200, true, Some("1")),
        ] {
            let input = ContainerLogInput {
                machine_id: MachineId::random(),
                container_id: ContainerId::parse("a".repeat(64)).unwrap(),
                tail,
                follow,
                before_nanos: before.map(str::to_owned),
                since_unix_seconds: None,
            };
            let error = match session.container_logs(input).await {
                Err(error) => error,
                Ok(_) => panic!("invalid log options opened a stream"),
            };
            assert_eq!(error.code, ployz_core::RpcErrorCode::InvalidArgument);
        }
    }

    #[test]
    fn malformed_log_frame_is_an_error_not_empty_output() {
        let payload = OpaquePayload { json: vec![0xff] };
        assert!(decode_record(&payload).is_err());
    }
}
