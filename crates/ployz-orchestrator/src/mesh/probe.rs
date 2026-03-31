use crate::model::OverlayIp;
use futures_util::stream::{FuturesUnordered, StreamExt};
use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr, TcpListener as StdTcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
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
const PROBE_BIND_RETRY_INTERVAL: Duration = Duration::from_millis(250);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeListenerFamily {
    Ipv4,
    Ipv6,
}

impl ProbeListenerFamily {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ipv4 => "ipv4",
            Self::Ipv6 => "ipv6",
        }
    }

    #[must_use]
    pub(crate) fn for_overlay_ip(_overlay_ip: OverlayIp) -> Self {
        Self::Ipv6
    }
}

#[derive(Debug, Default)]
pub(crate) struct ProbeListenerReadiness {
    required_family: AtomicU8,
    ipv4_ready: AtomicBool,
    ipv6_ready: AtomicBool,
    required_ready_initialized: AtomicBool,
    required_ready_last: AtomicBool,
}

impl ProbeListenerReadiness {
    const REQUIRED_NONE: u8 = 0;
    const REQUIRED_IPV4: u8 = 1;
    const REQUIRED_IPV6: u8 = 2;

    #[must_use]
    pub(crate) fn new(required_family: Option<ProbeListenerFamily>) -> Self {
        let readiness = Self {
            required_family: AtomicU8::new(Self::encode_required_family(required_family)),
            ipv4_ready: AtomicBool::new(false),
            ipv6_ready: AtomicBool::new(false),
            required_ready_initialized: AtomicBool::new(false),
            required_ready_last: AtomicBool::new(false),
        };
        readiness.log_required_family_ready_change();
        readiness
    }

    pub(crate) fn set_required_family(&self, required_family: Option<ProbeListenerFamily>) {
        self.required_family.store(
            Self::encode_required_family(required_family),
            Ordering::SeqCst,
        );
        self.log_required_family_ready_change();
    }

    pub(crate) fn set_family_ready(&self, family: ProbeListenerFamily, ready: bool) {
        match family {
            ProbeListenerFamily::Ipv4 => self.ipv4_ready.store(ready, Ordering::SeqCst),
            ProbeListenerFamily::Ipv6 => self.ipv6_ready.store(ready, Ordering::SeqCst),
        }
        self.log_required_family_ready_change();
    }

    pub(crate) fn clear_bound_families(&self) {
        self.ipv4_ready.store(false, Ordering::SeqCst);
        self.ipv6_ready.store(false, Ordering::SeqCst);
        self.log_required_family_ready_change();
    }

    #[must_use]
    pub(crate) fn required_family(&self) -> Option<ProbeListenerFamily> {
        Self::decode_required_family(self.required_family.load(Ordering::SeqCst))
    }

    #[must_use]
    pub(crate) fn family_ready(&self, family: ProbeListenerFamily) -> bool {
        match family {
            ProbeListenerFamily::Ipv4 => self.ipv4_ready.load(Ordering::SeqCst),
            ProbeListenerFamily::Ipv6 => self.ipv6_ready.load(Ordering::SeqCst),
        }
    }

    #[must_use]
    pub(crate) fn local_probe_ready(&self) -> bool {
        match self.required_family() {
            Some(required_family) => self.family_ready(required_family),
            None => true,
        }
    }

    #[must_use]
    fn encode_required_family(required_family: Option<ProbeListenerFamily>) -> u8 {
        match required_family {
            Some(ProbeListenerFamily::Ipv4) => Self::REQUIRED_IPV4,
            Some(ProbeListenerFamily::Ipv6) => Self::REQUIRED_IPV6,
            None => Self::REQUIRED_NONE,
        }
    }

    #[must_use]
    fn decode_required_family(encoded: u8) -> Option<ProbeListenerFamily> {
        match encoded {
            Self::REQUIRED_IPV4 => Some(ProbeListenerFamily::Ipv4),
            Self::REQUIRED_IPV6 => Some(ProbeListenerFamily::Ipv6),
            Self::REQUIRED_NONE => None,
            _ => None,
        }
    }

    fn log_required_family_ready_change(&self) {
        let required_family = self.required_family();
        let ready = self.local_probe_ready();
        let initialized = self.required_ready_initialized.swap(true, Ordering::SeqCst);
        let previous_ready = self.required_ready_last.swap(ready, Ordering::SeqCst);
        if initialized && previous_ready == ready {
            return;
        }

        info!(
            required_family = required_family.map(ProbeListenerFamily::as_str),
            required_family_ready = ready,
            ipv4_ready = self.family_ready(ProbeListenerFamily::Ipv4),
            ipv6_ready = self.family_ready(ProbeListenerFamily::Ipv6),
            "mesh probe listener readiness changed"
        );
    }
}

pub(crate) async fn run_probe_listener_task(
    cancel: CancellationToken,
    readiness: Arc<ProbeListenerReadiness>,
) {
    readiness.clear_bound_families();
    info!(
        port = MESH_PROBE_PORT,
        required_family = readiness.required_family().map(ProbeListenerFamily::as_str),
        "mesh probe listener supervisor started"
    );

    let mut tasks = Vec::with_capacity(2);
    for family in [ProbeListenerFamily::Ipv4, ProbeListenerFamily::Ipv6] {
        let family_cancel = cancel.clone();
        let family_readiness = Arc::clone(&readiness);
        tasks.push(tokio::spawn(async move {
            run_family_listener_task(family, family_cancel, family_readiness).await;
        }));
    }

    cancel.cancelled().await;
    for task in tasks {
        let _ = task.await;
    }
    readiness.clear_bound_families();
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

async fn run_family_listener_task(
    family: ProbeListenerFamily,
    cancel: CancellationToken,
    readiness: Arc<ProbeListenerReadiness>,
) {
    let mut bind_failures = 0_u32;

    loop {
        let required_family = readiness.required_family();
        match bind_listener(family, MESH_PROBE_PORT).await {
            Ok(listener) => {
                readiness.set_family_ready(family, true);
                if bind_failures == 0 {
                    info!(
                        family = family.as_str(),
                        port = MESH_PROBE_PORT,
                        "mesh probe listener bound"
                    );
                } else {
                    info!(
                        family = family.as_str(),
                        port = MESH_PROBE_PORT,
                        bind_failures,
                        "mesh probe listener bound after retry"
                    );
                }
                serve_listener(listener, cancel.clone()).await;
                readiness.set_family_ready(family, false);
                info!(
                    family = family.as_str(),
                    port = MESH_PROBE_PORT,
                    "mesh probe listener unbound"
                );
                break;
            }
            Err(error) => {
                readiness.set_family_ready(family, false);
                bind_failures = bind_failures.saturating_add(1);
                let is_required = required_family == Some(family);
                if is_required || bind_failures == 1 {
                    warn!(
                        ?error,
                        family = family.as_str(),
                        port = MESH_PROBE_PORT,
                        bind_failures,
                        required_family = required_family.map(ProbeListenerFamily::as_str),
                        "failed to bind mesh probe listener"
                    );
                } else {
                    debug!(
                        ?error,
                        family = family.as_str(),
                        port = MESH_PROBE_PORT,
                        bind_failures,
                        required_family = required_family.map(ProbeListenerFamily::as_str),
                        "mesh probe listener bind still failing"
                    );
                }
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(PROBE_BIND_RETRY_INTERVAL) => {}
                }
            }
        }
    }
}

async fn bind_listener(family: ProbeListenerFamily, port: u16) -> io::Result<TcpListener> {
    match family {
        ProbeListenerFamily::Ipv4 => bind_v4(port).await,
        ProbeListenerFamily::Ipv6 => bind_v6(port).await,
    }
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
pub(crate) fn probe_port_test_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};

    static PROBE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    PROBE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("lock probe port")
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
        // Bind to a random port, then drop the listener so nothing is listening.
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        drop(listener);

        assert!(probe_addr(addr, Duration::from_millis(50)).await.is_none());
    }

    #[tokio::test]
    async fn v4_and_v6_probe_listeners_can_share_a_port() {
        let v4 = bind_v4(0).await.expect("bind IPv4 listener");
        let port = v4.local_addr().expect("v4 addr").port();
        let v6 = bind_v6(port).await.expect("bind IPv6 listener");

        assert_eq!(v6.local_addr().expect("v6 addr").port(), port);
    }

    #[tokio::test]
    async fn required_ipv6_probe_readiness_retries_until_ipv6_bind_succeeds() {
        let _port_guard = probe_port_test_lock();
        let blocking_listener = bind_v6_only_listener(MESH_PROBE_PORT).expect("block IPv6 bind");
        let readiness = Arc::new(ProbeListenerReadiness::new(Some(ProbeListenerFamily::Ipv6)));
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task_readiness = Arc::clone(&readiness);
        let task = tokio::spawn(async move {
            run_probe_listener_task(task_cancel, task_readiness).await;
        });

        wait_until(Duration::from_secs(2), || {
            readiness.family_ready(ProbeListenerFamily::Ipv4)
        })
        .await;
        assert!(!readiness.local_probe_ready());
        assert!(!readiness.family_ready(ProbeListenerFamily::Ipv6));

        drop(blocking_listener);

        wait_until(Duration::from_secs(3), || readiness.local_probe_ready()).await;
        assert!(readiness.family_ready(ProbeListenerFamily::Ipv6));

        cancel.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn non_required_probe_family_recovers_after_becoming_required() {
        let _port_guard = probe_port_test_lock();
        let blocking_listener = bind_v6_only_listener(MESH_PROBE_PORT).expect("block IPv6 bind");
        let readiness = Arc::new(ProbeListenerReadiness::new(Some(ProbeListenerFamily::Ipv4)));
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task_readiness = Arc::clone(&readiness);
        let task = tokio::spawn(async move {
            run_probe_listener_task(task_cancel, task_readiness).await;
        });

        wait_until(Duration::from_secs(2), || {
            readiness.family_ready(ProbeListenerFamily::Ipv4)
        })
        .await;
        assert!(readiness.local_probe_ready());
        assert!(!readiness.family_ready(ProbeListenerFamily::Ipv6));

        readiness.set_required_family(Some(ProbeListenerFamily::Ipv6));
        wait_until(Duration::from_secs(1), || !readiness.local_probe_ready()).await;

        drop(blocking_listener);

        wait_until(Duration::from_secs(3), || readiness.local_probe_ready()).await;
        assert!(readiness.family_ready(ProbeListenerFamily::Ipv6));

        cancel.cancel();
        let _ = task.await;
    }

    async fn wait_until(timeout: Duration, mut check: impl FnMut() -> bool) {
        let started = Instant::now();
        while started.elapsed() < timeout {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("condition did not become true within {timeout:?}");
    }
}
