use crate::mesh::probe::{TcpProbeResult, TcpProbeStatus, probe_overlay_ip_with_timeout};
use crate::model::OverlayIp;
use futures_util::stream::{FuturesUnordered, StreamExt};
use std::collections::HashMap;
use std::time::Duration;

pub const DEFAULT_PROBE_ATTEMPTS: u8 = 3;
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReachabilityStatus {
    Reachable,
    Unreachable,
}

#[derive(Debug, Clone, Copy)]
pub struct ReachabilityResult {
    pub status: ReachabilityStatus,
    pub successful_attempt: Option<u8>,
}

pub async fn probe_overlay_ips(
    overlay_ips: &[OverlayIp],
) -> HashMap<OverlayIp, ReachabilityResult> {
    probe_overlay_ips_with_policy(overlay_ips, DEFAULT_PROBE_ATTEMPTS, DEFAULT_PROBE_TIMEOUT).await
}

pub async fn probe_overlay_ips_with_policy(
    overlay_ips: &[OverlayIp],
    attempts: u8,
    timeout: Duration,
) -> HashMap<OverlayIp, ReachabilityResult> {
    let mut probes = FuturesUnordered::new();
    for overlay_ip in overlay_ips {
        let overlay_ip = *overlay_ip;
        probes.push(async move {
            let result = probe_overlay_ip_with_retries(overlay_ip, attempts, timeout).await;
            (overlay_ip, result)
        });
    }

    let mut results = HashMap::with_capacity(overlay_ips.len());
    while let Some((overlay_ip, result)) = probes.next().await {
        results.insert(overlay_ip, result);
    }
    results
}

async fn probe_overlay_ip_with_retries(
    overlay_ip: OverlayIp,
    attempts: u8,
    timeout: Duration,
) -> ReachabilityResult {
    let attempts = attempts.max(1);
    for attempt in 1..=attempts {
        let TcpProbeResult { status } = probe_overlay_ip_with_timeout(overlay_ip, timeout).await;
        if status == TcpProbeStatus::Reachable {
            return ReachabilityResult {
                status: ReachabilityStatus::Reachable,
                successful_attempt: Some(attempt),
            };
        }
    }

    ReachabilityResult {
        status: ReachabilityStatus::Unreachable,
        successful_attempt: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::probe::MESH_PROBE_PORT;
    use std::io;
    use std::net::SocketAddr;
    use std::sync::OnceLock;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::{Mutex, MutexGuard};

    async fn run_probe_server() -> io::Result<(tokio::task::JoinHandle<()>, OverlayIp)> {
        let listener = TcpListener::bind(SocketAddr::from((
            std::net::Ipv6Addr::LOCALHOST,
            MESH_PROBE_PORT,
        )))
        .await?;
        let handle = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut request = [0_u8; 4];
                    if stream.read_exact(&mut request).await.is_ok() {
                        let _ = stream.write_all(b"OK!!").await;
                        let _ = stream.flush().await;
                    }
                });
            }
        });
        Ok((handle, OverlayIp(std::net::Ipv6Addr::LOCALHOST)))
    }

    #[tokio::test]
    async fn marks_unreachable_after_all_failures() {
        let _guard = lock_test_probe_port().await;
        let results = probe_overlay_ips_with_policy(
            &[OverlayIp(std::net::Ipv6Addr::LOCALHOST)],
            3,
            Duration::from_millis(1),
        )
        .await;
        let localhost = OverlayIp(std::net::Ipv6Addr::LOCALHOST);
        let result = results.get(&localhost).expect("localhost result present");
        assert_eq!(result.status, ReachabilityStatus::Unreachable);
    }

    #[tokio::test]
    async fn stops_after_first_success() {
        let _guard = lock_test_probe_port().await;
        let (server, overlay_ip) = run_probe_server().await.expect("start server");
        let results =
            probe_overlay_ips_with_policy(&[overlay_ip], 3, Duration::from_millis(50)).await;
        server.abort();
        let result = results.get(&overlay_ip).expect("overlay ip result present");
        assert_eq!(result.status, ReachabilityStatus::Reachable);
        assert_eq!(result.successful_attempt, Some(1));
    }

    async fn lock_test_probe_port() -> MutexGuard<'static, ()> {
        static PROBE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        PROBE_LOCK.get_or_init(|| Mutex::new(())).lock().await
    }
}
