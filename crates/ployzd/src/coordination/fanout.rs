use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use ployz_api::{
    CoordinationAbortRequest, CoordinationCommitRequest, CoordinationPreparePayload,
    CoordinationPrepareRequest,
};
use ployz_sdk::{DaemonClient, TcpTransport};
use ployz_types::model::{MachineId, OverlayIp};
use tokio::task::JoinSet;

/// A peer daemon to fan a coordination request out to.
#[derive(Clone)]
pub(crate) struct FanOutTarget {
    pub(crate) machine_id: MachineId,
    pub(crate) overlay_ip: OverlayIp,
}

/// Result of a fanned-out prepare operation.
pub(crate) struct FanOutPrepareResult {
    /// True when every responding peer accepted (no explicit denials).
    /// Offline peers (timeout / connection refused) are excluded from this check.
    /// Empty `targets` or all-offline is trivially true (single-machine cluster).
    pub(crate) all_online_accepted: bool,
    /// Targets that accepted, paired with their payload (carries the prepare token).
    pub(crate) accepted: Vec<(FanOutTarget, CoordinationPreparePayload)>,
    /// Targets that responded with `accepted: false` — real conflict.
    pub(crate) denied: Vec<MachineId>,
    /// Targets that timed out or refused the connection — not reachable right now.
    pub(crate) offline: Vec<MachineId>,
}

fn client(overlay_ip: OverlayIp, rpc_port: u16) -> DaemonClient<TcpTransport> {
    let addr = SocketAddr::new(IpAddr::V6(overlay_ip.0), rpc_port);
    DaemonClient::new(TcpTransport::new(addr))
}

/// Fan out a `CoordinationPrepare` to all `targets` in parallel.
///
/// Returns immediately once all tasks complete or time out.
/// Peers that are unreachable (timeout or connection refused) are recorded in
/// `offline` but do **not** block the operation — only explicit `accepted: false`
/// responses (in `denied`) cause `all_online_accepted` to be false.
pub(crate) async fn fanout_prepare(
    targets: &[FanOutTarget],
    rpc_port: u16,
    request: CoordinationPrepareRequest,
    deadline: Duration,
) -> FanOutPrepareResult {
    if targets.is_empty() {
        return FanOutPrepareResult {
            all_online_accepted: true,
            accepted: Vec::new(),
            denied: Vec::new(),
            offline: Vec::new(),
        };
    }

    // Each task returns (target, Ok(payload)) if the peer responded or (target, Err(is_offline))
    // where is_offline=true means unreachable, false means responded but denied.
    let mut set: JoinSet<(FanOutTarget, Result<CoordinationPreparePayload, bool>)> =
        JoinSet::new();
    for target in targets {
        let t = target.clone();
        let c = client(t.overlay_ip, rpc_port);
        let req = request.clone();
        set.spawn(async move {
            match tokio::time::timeout(deadline, c.coordination_prepare(req)).await {
                Ok(Ok(payload)) => (t, Ok(payload)),
                Ok(Err(_io_err)) => (t, Err(true)),  // connection refused / IO → offline
                Err(_timeout) => (t, Err(true)),     // deadline exceeded → offline
            }
        });
    }

    let mut accepted = Vec::new();
    let mut denied = Vec::new();
    let mut offline = Vec::new();
    while let Some(join_result) = set.join_next().await {
        match join_result {
            Ok((target, Ok(payload))) if payload.accepted => {
                accepted.push((target, payload));
            }
            Ok((target, Ok(_payload))) => {
                // Responded but accepted: false — real conflict.
                denied.push(target.machine_id);
            }
            Ok((target, Err(_is_offline))) => {
                offline.push(target.machine_id);
            }
            Err(_join_err) => {
                // Task panicked — machine_id is lost but it counts as offline.
            }
        }
    }

    let all_online_accepted = denied.is_empty();
    FanOutPrepareResult {
        all_online_accepted,
        accepted,
        denied,
        offline,
    }
}

/// Fan out a `CoordinationCommit` to all `targets` in parallel.
///
/// Returns `true` when every target committed successfully.
pub(crate) async fn fanout_commit(
    targets: &[FanOutTarget],
    rpc_port: u16,
    request: CoordinationCommitRequest,
    deadline: Duration,
) -> bool {
    if targets.is_empty() {
        return true;
    }

    let mut set: JoinSet<bool> = JoinSet::new();
    for target in targets {
        let c = client(target.overlay_ip, rpc_port);
        let req = request.clone();
        set.spawn(async move {
            matches!(
                tokio::time::timeout(deadline, c.coordination_commit(req)).await,
                Ok(Ok(payload)) if payload.committed
            )
        });
    }

    let mut all_committed = true;
    while let Some(join_result) = set.join_next().await {
        match join_result {
            Ok(committed) => {
                if !committed {
                    all_committed = false;
                }
            }
            Err(_) => {
                all_committed = false;
            }
        }
    }
    all_committed
}

/// Fan out a `CoordinationAbort` to all `targets` in parallel, best-effort.
///
/// Errors and timeouts are silently ignored. The function drives all tasks to
/// completion before returning so the caller does not need to `.await` anything.
pub(crate) async fn fanout_abort(
    targets: &[FanOutTarget],
    rpc_port: u16,
    request: CoordinationAbortRequest,
) {
    const ABORT_DEADLINE: Duration = Duration::from_secs(5);

    if targets.is_empty() {
        return;
    }

    let mut set: JoinSet<()> = JoinSet::new();
    for target in targets {
        let c = client(target.overlay_ip, rpc_port);
        let req = request.clone();
        set.spawn(async move {
            let _ = tokio::time::timeout(ABORT_DEADLINE, c.coordination_abort(req)).await;
        });
    }

    while set.join_next().await.is_some() {}
}

/// Build fan-out targets from an accepted prepare result (for abort-on-failure paths).
pub(crate) fn accepted_targets(
    accepted: &[(FanOutTarget, CoordinationPreparePayload)],
) -> Vec<FanOutTarget> {
    accepted.iter().map(|(t, _)| t.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_api::{
        CoordinationCommitPayload, CoordinationLockKey, CoordinationOperation, DaemonPayload,
        DaemonResponse,
    };
    use std::net::Ipv6Addr;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    /// Spawn a mock TCP daemon that always replies with `response`.
    /// Returns the bound socket address.
    async fn spawn_mock(response: DaemonResponse) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock listener");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let response = response.clone();
                tokio::spawn(async move {
                    let (reader, mut writer) = stream.into_split();
                    let mut buf = BufReader::new(reader);
                    let mut line = String::new();
                    let _ = buf.read_line(&mut line).await;
                    let mut encoded =
                        serde_json::to_string(&response).expect("encode response");
                    encoded.push('\n');
                    let _ = writer.write_all(encoded.as_bytes()).await;
                });
            }
        });
        addr
    }

    fn accepted_prepare_response(token: &str) -> DaemonResponse {
        DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "coordination prepare accepted".into(),
            payload: Some(DaemonPayload::CoordinationPrepare(
                CoordinationPreparePayload {
                    accepted: true,
                    prepare_token: Some(token.into()),
                    reason: None,
                },
            )),
        }
    }

    fn denied_prepare_response(reason: &str) -> DaemonResponse {
        DaemonResponse {
            ok: false,
            code: "COORDINATION_DENIED".into(),
            message: reason.into(),
            payload: Some(DaemonPayload::CoordinationPrepare(
                CoordinationPreparePayload {
                    accepted: false,
                    prepare_token: None,
                    reason: Some(reason.into()),
                },
            )),
        }
    }

    fn committed_response() -> DaemonResponse {
        DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "coordination commit accepted".into(),
            payload: Some(DaemonPayload::CoordinationCommit(
                CoordinationCommitPayload {
                    committed: true,
                    reason: None,
                },
            )),
        }
    }

    fn sample_request() -> CoordinationPrepareRequest {
        CoordinationPrepareRequest {
            owner_id: "founder-a".into(),
            nonce: "n1".into(),
            lease_ttl_secs: 30,
            operation: CoordinationOperation::LockAcquire {
                key: CoordinationLockKey::SubnetClaim {
                    subnet: "10.210.1.0/24".into(),
                },
            },
        }
    }

    fn target_for(addr: SocketAddr) -> FanOutTarget {
        // Map the IPv4 address back to a loopback IPv6 address that the mock
        // is actually bound to. Since our mock uses IPv4 and our client uses
        // IPv6, we directly build an IPv4-mapped IPv6 address here so the
        // TcpTransport connects correctly. In production, overlay_ip is always
        // a real WireGuard IPv6 address.
        let ipv6 = match addr.ip() {
            IpAddr::V4(v4) => v4.to_ipv6_mapped(),
            IpAddr::V6(v6) => v6,
        };
        FanOutTarget {
            machine_id: MachineId(format!("m-{}", addr.port())),
            overlay_ip: OverlayIp(ipv6),
        }
    }

    #[tokio::test]
    async fn fanout_prepare_empty_targets_returns_all_online_accepted() {
        let result = fanout_prepare(
            &[],
            9999,
            sample_request(),
            Duration::from_secs(1),
        )
        .await;
        assert!(result.all_online_accepted);
        assert!(result.accepted.is_empty());
        assert!(result.denied.is_empty());
        assert!(result.offline.is_empty());
    }

    #[tokio::test]
    async fn fanout_prepare_single_target_accepted() {
        let addr = spawn_mock(accepted_prepare_response("tok-abc")).await;
        let targets = vec![target_for(addr)];

        let result = fanout_prepare(&targets, addr.port(), sample_request(), Duration::from_secs(2))
            .await;

        assert!(result.all_online_accepted);
        assert_eq!(result.accepted.len(), 1);
        assert!(result.denied.is_empty());
        assert!(result.offline.is_empty());
        let (_, payload) = &result.accepted[0];
        assert_eq!(payload.prepare_token.as_deref(), Some("tok-abc"));
    }

    #[tokio::test]
    async fn fanout_prepare_single_target_denied_blocks() {
        let addr = spawn_mock(denied_prepare_response("key already prepared")).await;
        let targets = vec![target_for(addr)];

        let result = fanout_prepare(&targets, addr.port(), sample_request(), Duration::from_secs(2))
            .await;

        assert!(!result.all_online_accepted);
        assert!(result.accepted.is_empty());
        assert_eq!(result.denied.len(), 1);
        assert!(result.offline.is_empty());
    }

    #[tokio::test]
    async fn fanout_prepare_timeout_counts_as_offline_not_denied() {
        // A mock that accepts connections but never writes back.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind silent listener");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            loop {
                let Ok((_stream, _)) = listener.accept().await else {
                    break;
                };
                // Hold stream open but write nothing.
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });

        let targets = vec![target_for(addr)];
        let result = fanout_prepare(
            &targets,
            addr.port(),
            sample_request(),
            Duration::from_millis(100),
        )
        .await;

        // Offline does NOT block — all_online_accepted is still true.
        assert!(result.all_online_accepted);
        assert!(result.accepted.is_empty());
        assert!(result.denied.is_empty());
        assert_eq!(result.offline.len(), 1);
    }

    #[tokio::test]
    async fn fanout_commit_empty_targets_returns_true() {
        let committed = fanout_commit(
            &[],
            9999,
            CoordinationCommitRequest {
                owner_id: "founder-a".into(),
                nonce: "n1".into(),
                prepare_tokens: vec!["tok".into()],
                operation: CoordinationOperation::LockAcquire {
                    key: CoordinationLockKey::SubnetClaim {
                        subnet: "10.210.1.0/24".into(),
                    },
                },
            },
            Duration::from_secs(1),
        )
        .await;
        assert!(committed);
    }

    #[tokio::test]
    async fn fanout_commit_single_target_committed() {
        let addr = spawn_mock(committed_response()).await;
        let targets = vec![target_for(addr)];

        let committed = fanout_commit(
            &targets,
            addr.port(),
            CoordinationCommitRequest {
                owner_id: "founder-a".into(),
                nonce: "n1".into(),
                prepare_tokens: vec!["tok".into()],
                operation: CoordinationOperation::LockAcquire {
                    key: CoordinationLockKey::SubnetClaim {
                        subnet: "10.210.1.0/24".into(),
                    },
                },
            },
            Duration::from_secs(2),
        )
        .await;

        assert!(committed);
    }

    #[tokio::test]
    async fn fanout_abort_completes_without_blocking() {
        let addr = spawn_mock(DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "coordination abort accepted".into(),
            payload: None,
        })
        .await;
        let targets = vec![target_for(addr)];

        fanout_abort(
            &targets,
            addr.port(),
            CoordinationAbortRequest {
                owner_id: "founder-a".into(),
                nonce: "n1".into(),
                operation: CoordinationOperation::LockAcquire {
                    key: CoordinationLockKey::SubnetClaim {
                        subnet: "10.210.1.0/24".into(),
                    },
                },
            },
        )
        .await;
        // If we reach here without hanging, the test passes.
    }
}
