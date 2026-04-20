//! TCP transport for daemon RPC.
//!
//! Timeouts are split by phase:
//! - **connect** — "is this peer reachable?" Short and uniform (~500ms on LAN).
//!   Under silent packet loss (iptables DROP, blackholed routes, dead peer) a
//!   naked `TcpStream::connect` burns the kernel's SYN-retry window. The
//!   explicit connect deadline is what makes unreachable peers fail in
//!   milliseconds instead of seconds.
//! - **request** — "is this operation making progress?" Per-call (~2s for
//!   transactional RPCs). Long-poll RPCs opt into a larger budget via
//!   [`TcpTransport::with_timeouts`].
//!
//! Callers must not wrap transport calls in their own `tokio::time::timeout`.
//! Two deadlines for the same concern is either dead code or a bug-hider — the
//! transport owns timeout truth. If a caller has a different latency
//! expectation, set it on the transport via `with_timeouts`.
//!
//! See `AGENTS.md` → "RPC Timeouts and Fail-Fast" for the project-wide rules.

use std::net::SocketAddr;
use std::time::Duration;

use ployz_api::{DaemonRequest, DaemonResponse};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

pub struct TcpTransport {
    addr: SocketAddr,
    connect_timeout: Duration,
    request_timeout: Duration,
}

impl TcpTransport {
    #[must_use]
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_timeouts(mut self, connect_timeout: Duration, request_timeout: Duration) -> Self {
        self.connect_timeout = connect_timeout;
        self.request_timeout = request_timeout;
        self
    }
}

impl super::Transport for TcpTransport {
    async fn request(&self, request: DaemonRequest) -> std::io::Result<DaemonResponse> {
        let stream = timeout(self.connect_timeout, TcpStream::connect(self.addr))
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "tcp connect timeout")
            })??;

        timeout(self.request_timeout, async move {
            let (reader, mut writer) = stream.into_split();

            let mut line = serde_json::to_string(&request)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            line.push('\n');
            writer.write_all(line.as_bytes()).await?;
            writer.shutdown().await?;

            let mut response = String::new();
            let mut reader = BufReader::new(reader);
            reader.read_line(&mut response).await?;

            serde_json::from_str(&response)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "tcp request timeout"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Transport;
    use ployz_api::{DaemonPayload, StatusPayload};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn request_round_trips_json_over_tcp_socket() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind tcp listener");
        let addr = listener.local_addr().expect("local addr");
        let response = DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "status".into(),
            payload: Some(DaemonPayload::Status(StatusPayload {
                protocol_version: 1,
                daemon_version: "0.2.2".into(),
                machine_id: "alpha".into(),
                active_network_name: Some("mesh-a".into()),
                phase: "running".into(),
                capabilities: vec!["status-payload-v1".into()],
            })),
        };

        let server = tokio::spawn({
            let response = response.clone();
            async move {
                let (stream, _) = listener.accept().await.expect("accept client");
                let (reader, mut writer) = stream.into_split();
                let mut request_line = String::new();
                let mut reader = BufReader::new(reader);
                reader
                    .read_line(&mut request_line)
                    .await
                    .expect("read request");
                let request: DaemonRequest =
                    serde_json::from_str(&request_line).expect("decode request");
                let DaemonRequest::Status = request else {
                    panic!("unexpected request: {request:?}");
                };

                let mut encoded = serde_json::to_string(&response).expect("encode response");
                encoded.push('\n');
                writer
                    .write_all(encoded.as_bytes())
                    .await
                    .expect("write response");
            }
        });

        let transport = TcpTransport::new(addr);
        let received = transport
            .request(DaemonRequest::Status)
            .await
            .expect("request succeeds");
        assert!(received.ok);
        assert_eq!(received.message, "status");

        server.await.expect("server task exits");
    }

    #[tokio::test]
    async fn request_reports_invalid_json_response() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind tcp listener");
        let addr = listener.local_addr().expect("local addr");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept client");
            let (_, mut writer) = stream.into_split();
            writer
                .write_all(b"not-json\n")
                .await
                .expect("write invalid response");
        });

        let transport = TcpTransport::new(addr);
        let error = transport
            .request(DaemonRequest::Status)
            .await
            .expect_err("invalid response should fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        server.await.expect("server task exits");
    }

    #[tokio::test]
    async fn request_times_out_when_response_stalls() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind tcp listener");
        let addr = listener.local_addr().expect("local addr");

        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.expect("accept client");
            tokio::time::sleep(Duration::from_millis(200)).await;
        });

        let transport = TcpTransport::new(addr)
            .with_timeouts(Duration::from_millis(50), Duration::from_millis(50));
        let error = transport
            .request(DaemonRequest::Status)
            .await
            .expect_err("stalled response should time out");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);

        server.await.expect("server task exits");
    }
}
