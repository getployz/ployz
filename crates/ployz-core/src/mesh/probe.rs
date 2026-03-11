use futures_util::stream::{FuturesUnordered, StreamExt};
use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

pub(crate) const MESH_PROBE_PORT: u16 = 51821;
const PROBE_REQUEST: &[u8; 4] = b"PLZ?";
const PROBE_RESPONSE: &[u8; 4] = b"OK!!";
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TcpProbeStatus {
    Reachable,
    Unreachable,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TcpProbeResult {
    pub(crate) status: TcpProbeStatus,
    pub(crate) rtt: Option<Duration>,
}

impl TcpProbeResult {
    fn reachable(rtt: Duration) -> Self {
        Self {
            status: TcpProbeStatus::Reachable,
            rtt: Some(rtt),
        }
    }

    fn unreachable() -> Self {
        Self {
            status: TcpProbeStatus::Unreachable,
            rtt: None,
        }
    }
}

pub(crate) async fn run_probe_listener_task(cancel: CancellationToken) {
    let mut listeners = Vec::new();

    match bind_v4(MESH_PROBE_PORT).await {
        Ok(listener) => listeners.push(listener),
        Err(error) => warn!(
            ?error,
            port = MESH_PROBE_PORT,
            "failed to bind IPv4 mesh probe listener"
        ),
    }
    match bind_v6(MESH_PROBE_PORT).await {
        Ok(listener) => listeners.push(listener),
        Err(error) => warn!(
            ?error,
            port = MESH_PROBE_PORT,
            "failed to bind IPv6 mesh probe listener"
        ),
    }

    if listeners.is_empty() {
        warn!(
            port = MESH_PROBE_PORT,
            "mesh probe listener disabled because no sockets could bind"
        );
        return;
    }

    info!(port = MESH_PROBE_PORT, "mesh probe listener started");
    let mut tasks = Vec::with_capacity(listeners.len());
    for listener in listeners {
        let listener_cancel = cancel.clone();
        tasks.push(tokio::spawn(async move {
            serve_listener(listener, listener_cancel).await;
        }));
    }

    cancel.cancelled().await;
    for task in tasks {
        let _ = task.await;
    }
    info!(port = MESH_PROBE_PORT, "mesh probe listener stopped");
}

pub(crate) async fn probe_endpoints_parallel(
    endpoints: &[String],
) -> HashMap<String, TcpProbeResult> {
    let mut probes = FuturesUnordered::new();
    for endpoint in endpoints {
        let endpoint = endpoint.clone();
        probes.push(async move {
            let result = match probe_endpoint(&endpoint, PROBE_TIMEOUT).await {
                Some(rtt) => TcpProbeResult::reachable(rtt),
                None => TcpProbeResult::unreachable(),
            };
            (endpoint, result)
        });
    }

    let mut results = HashMap::with_capacity(endpoints.len());
    while let Some((endpoint, result)) = probes.next().await {
        results.insert(endpoint, result);
    }
    results
}

async fn probe_endpoint(endpoint: &str, timeout: Duration) -> Option<Duration> {
    let addr = probe_socket_addr(endpoint)?;
    probe_addr(addr, timeout).await
}

async fn probe_addr(addr: SocketAddr, timeout: Duration) -> Option<Duration> {
    let start = Instant::now();
    let mut stream = tokio::time::timeout(timeout, TcpStream::connect(addr))
        .await
        .ok()?
        .ok()?;

    tokio::time::timeout(timeout, async {
        stream.write_all(PROBE_REQUEST).await?;
        let mut response = [0_u8; PROBE_RESPONSE.len()];
        stream.read_exact(&mut response).await?;
        if &response == PROBE_RESPONSE {
            Ok::<(), io::Error>(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid mesh probe response",
            ))
        }
    })
    .await
    .ok()?
    .ok()?;

    Some(start.elapsed())
}

fn probe_socket_addr(endpoint: &str) -> Option<SocketAddr> {
    let mut addr: SocketAddr = endpoint.parse().ok()?;
    addr.set_port(MESH_PROBE_PORT);
    Some(addr)
}

async fn serve_listener(listener: TcpListener, cancel: CancellationToken) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                match accepted {
                    Ok((mut stream, remote_addr)) => {
                        tokio::spawn(async move {
                            if let Err(error) = handle_probe_connection(&mut stream).await {
                                debug!(?error, %remote_addr, "mesh probe connection failed");
                            }
                        });
                    }
                    Err(error) => {
                        debug!(?error, "mesh probe accept failed");
                    }
                }
            }
        }
    }
}

async fn handle_probe_connection(stream: &mut TcpStream) -> io::Result<()> {
    let mut request = [0_u8; PROBE_REQUEST.len()];
    stream.read_exact(&mut request).await?;
    if &request != PROBE_REQUEST {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid mesh probe request",
        ));
    }
    stream.write_all(PROBE_RESPONSE).await?;
    stream.flush().await
}

async fn bind_v4(port: u16) -> io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::new(IpAddr::from([0, 0, 0, 0]), port)).await
}

async fn bind_v6(port: u16) -> io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::new(
        IpAddr::from(std::net::Ipv6Addr::UNSPECIFIED),
        port,
    ))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probe_roundtrip_reports_rtt() {
        let listener = bind_v4(0).await.expect("bind IPv4 test listener");
        let addr = listener.local_addr().expect("local addr");
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            serve_listener(listener, task_cancel).await;
        });

        let rtt = probe_addr(addr, Duration::from_secs(1))
            .await
            .expect("probe should succeed");
        assert!(rtt >= Duration::ZERO);

        cancel.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn probe_times_out_when_unreachable() {
        let endpoint = "127.0.0.1:9".to_string();
        assert!(
            probe_endpoint(&endpoint, Duration::from_millis(50))
                .await
                .is_none()
        );
    }
}
