use crate::model::OverlayIp;
use futures_util::stream::{FuturesUnordered, StreamExt};
use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr, TcpListener as StdTcpListener};
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

pub(crate) async fn probe_overlay_ips_parallel(
    overlay_ips: &[OverlayIp],
) -> HashMap<OverlayIp, TcpProbeResult> {
    let mut probes = FuturesUnordered::new();
    for overlay_ip in overlay_ips {
        let overlay_ip = *overlay_ip;
        probes.push(async move {
            let result = match probe_overlay_ip(overlay_ip, PROBE_TIMEOUT).await {
                Some(rtt) => TcpProbeResult::reachable(rtt),
                None => TcpProbeResult::unreachable(),
            };
            (overlay_ip, result)
        });
    }

    let mut results = HashMap::with_capacity(overlay_ips.len());
    while let Some((overlay_ip, result)) = probes.next().await {
        results.insert(overlay_ip, result);
    }
    results
}

async fn probe_endpoint(endpoint: &str, timeout: Duration) -> Option<Duration> {
    let addr = probe_socket_addr(endpoint)?;
    probe_addr(addr, timeout).await
}

async fn probe_overlay_ip(overlay_ip: OverlayIp, timeout: Duration) -> Option<Duration> {
    probe_addr(
        SocketAddr::new(IpAddr::from(overlay_ip.0), MESH_PROBE_PORT),
        timeout,
    )
    .await
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
    bind_v6_only_listener(port)
}

#[cfg(unix)]
fn bind_v6_only_listener(port: u16) -> io::Result<TcpListener> {
    use std::mem::{size_of, zeroed};
    use std::os::fd::FromRawFd;

    let fd = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }

    let enabled: libc::c_int = 1;
    let set_result = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_IPV6,
            libc::IPV6_V6ONLY,
            (&enabled as *const libc::c_int).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if set_result != 0 {
        let error = io::Error::last_os_error();
        let _ = unsafe { libc::close(fd) };
        return Err(error);
    }

    let reuse_result = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            (&enabled as *const libc::c_int).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if reuse_result != 0 {
        let error = io::Error::last_os_error();
        let _ = unsafe { libc::close(fd) };
        return Err(error);
    }

    let mut addr: libc::sockaddr_in6 = unsafe { zeroed() };
    addr.sin6_family = libc::AF_INET6 as libc::sa_family_t;
    addr.sin6_port = port.to_be();
    addr.sin6_addr = libc::in6_addr { s6_addr: [0; 16] };

    let bind_result = unsafe {
        libc::bind(
            fd,
            (&addr as *const libc::sockaddr_in6).cast(),
            size_of::<libc::sockaddr_in6>() as libc::socklen_t,
        )
    };
    if bind_result != 0 {
        let error = io::Error::last_os_error();
        let _ = unsafe { libc::close(fd) };
        return Err(error);
    }

    let listen_result = unsafe { libc::listen(fd, 1024) };
    if listen_result != 0 {
        let error = io::Error::last_os_error();
        let _ = unsafe { libc::close(fd) };
        return Err(error);
    }

    let std_listener = unsafe { StdTcpListener::from_raw_fd(fd) };
    std_listener.set_nonblocking(true)?;
    TcpListener::from_std(std_listener)
}

#[cfg(not(unix))]
async fn bind_v6_only_listener(port: u16) -> io::Result<TcpListener> {
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

    #[tokio::test]
    async fn v4_and_v6_probe_listeners_can_share_a_port() {
        let v4 = bind_v4(0).await.expect("bind IPv4 listener");
        let port = v4.local_addr().expect("v4 addr").port();
        let v6 = bind_v6(port).await.expect("bind IPv6 listener");

        assert_eq!(v6.local_addr().expect("v6 addr").port(), port);
    }
}
