use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::future::join_all;
use ployz_api::{CoordOp, DaemonRequest, ResourceKey as ApiResourceKey};
use ployz_orchestrator::certificates::{
    AccountAcquisition, AcmeAccountCoordinator, HTTP01_CHALLENGE_VISIBILITY_TIMEOUT,
    Http01ChallengeReadiness, IssuanceAcquisition, IssuanceCoordinator, IssuanceHold,
};
use ployz_orchestrator::coordination::{
    PendingReservations, Reservation, ReservationId, ResourceKey, Vote,
};
use ployz_orchestrator::machine_policy::coordination_peers;
use ployz_store_api::{CertificateStore, MachineStore, StoreDriver};
use ployz_types::error::{Error, Result};
use ployz_types::model::{MachineId, OverlayIp};
use ployz_types::time::now_unix_secs;
use tokio::time::timeout;
use tracing::warn;

use crate::daemon::DaemonState;
use crate::daemon::handlers::peer_rpc;

/// Best-effort cluster-wide coordinator for ACME issuance. Fans out
/// `CoordOp::Prepare` to every active peer; explicit `COORDINATION_DENIED`
/// from any reachable peer is a veto; RPC failures are abstentions.
pub struct OverlayIssuanceCoordinator {
    store: StoreDriver,
    reservations: Arc<PendingReservations>,
    self_id: MachineId,
    peer_rpc_port: u16,
    ttl: Duration,
}

const DEFAULT_ISSUANCE_TTL_SECS: u64 = 5 * 60;

impl OverlayIssuanceCoordinator {
    #[must_use]
    pub fn new(
        store: StoreDriver,
        reservations: Arc<PendingReservations>,
        self_id: MachineId,
        peer_rpc_port: u16,
    ) -> Self {
        Self {
            store,
            reservations,
            self_id,
            peer_rpc_port,
            ttl: Duration::from_secs(DEFAULT_ISSUANCE_TTL_SECS),
        }
    }
}

#[derive(Clone)]
struct PeerAddress {
    machine_id: MachineId,
    overlay_ip: OverlayIp,
}

#[async_trait]
impl IssuanceCoordinator for OverlayIssuanceCoordinator {
    async fn try_acquire(&self, hostname: &str) -> IssuanceAcquisition {
        match self
            .try_acquire_resource(
                ResourceKey::CertIssuance(hostname.to_string()),
                "cert issuance",
                hostname,
            )
            .await
        {
            ResourceAcquisition::Allowed(hold) => IssuanceAcquisition::Allowed(hold),
            ResourceAcquisition::VetoedByPeer(reason) => IssuanceAcquisition::VetoedByPeer(reason),
        }
    }
}

#[async_trait]
impl AcmeAccountCoordinator for OverlayIssuanceCoordinator {
    async fn try_acquire_account(&self, issuer_url: &str) -> AccountAcquisition {
        match self
            .try_acquire_resource(
                ResourceKey::AcmeAccount(issuer_url.to_string()),
                "ACME account",
                issuer_url,
            )
            .await
        {
            ResourceAcquisition::Allowed(hold) => AccountAcquisition::Allowed(hold),
            ResourceAcquisition::VetoedByPeer(reason) => AccountAcquisition::VetoedByPeer(reason),
        }
    }
}

enum ResourceAcquisition {
    Allowed(IssuanceHold),
    VetoedByPeer(String),
}

impl OverlayIssuanceCoordinator {
    async fn try_acquire_resource(
        &self,
        key: ResourceKey,
        resource_kind: &'static str,
        resource_value: &str,
    ) -> ResourceAcquisition {
        let now = now_unix_secs();
        let reservation = Reservation {
            id: ReservationId::random(),
            key,
            owner: self_id_value(&self.self_id),
            nonce: ReservationId::random().0,
            expires_at: now.saturating_add(self.ttl.as_secs()),
        };

        // Local prepare first: cheap veto without any network traffic.
        match self
            .reservations
            .prepare(reservation.clone(), false, now)
            .await
        {
            Vote::Allow => {}
            Vote::Deny(conflict) => {
                return ResourceAcquisition::VetoedByPeer(format!("local: {conflict:?}"));
            }
        }

        let peers = match self.store.list_machines().await {
            Ok(machines) => coordination_peers(&machines, &self.self_id)
                .into_iter()
                .map(|machine| PeerAddress {
                    machine_id: machine.id.clone(),
                    overlay_ip: machine.overlay_ip,
                })
                .collect::<Vec<_>>(),
            Err(error) => {
                warn!(
                    %resource_kind,
                    resource = %resource_value,
                    ?error,
                    "could not list peers for coordination lock; deferring"
                );
                return ResourceAcquisition::VetoedByPeer(format!(
                    "peer inventory unavailable: {error}"
                ));
            }
        };

        let prepare_results: Vec<PeerPrepareOutcome> = join_all(peers.iter().map(|peer| {
            let reservation = reservation.clone();
            let port = self.peer_rpc_port;
            let peer = peer.clone();
            async move { prepare_peer(&peer, &reservation, port).await }
        }))
        .await;

        for (peer, outcome) in peers.iter().zip(prepare_results.iter()) {
            if matches!(outcome, PeerPrepareOutcome::ExplicitDeny) {
                // Veto by any reachable peer — release everywhere and abort.
                let _ = self
                    .reservations
                    .release(&reservation.key, &reservation.nonce, now_unix_secs())
                    .await;
                let allowed_peers = peers
                    .iter()
                    .zip(prepare_results.iter())
                    .filter_map(|(peer, outcome)| match outcome {
                        PeerPrepareOutcome::Allow => Some(peer.clone()),
                        PeerPrepareOutcome::ExplicitDeny | PeerPrepareOutcome::Unreachable => None,
                    })
                    .collect::<Vec<_>>();
                release_peers(&allowed_peers, &reservation, self.peer_rpc_port).await;
                return ResourceAcquisition::VetoedByPeer(format!("peer {}", peer.machine_id));
            }
        }

        // Allow: build a hold that releases everything on drop.
        let reservations = Arc::clone(&self.reservations);
        let reservation_for_release = reservation.clone();
        let allowed_peers = peers
            .into_iter()
            .zip(prepare_results.into_iter())
            .filter_map(|(peer, outcome)| match outcome {
                PeerPrepareOutcome::Allow => Some(peer),
                PeerPrepareOutcome::ExplicitDeny | PeerPrepareOutcome::Unreachable => None,
            })
            .collect::<Vec<_>>();
        let peer_rpc_port = self.peer_rpc_port;
        ResourceAcquisition::Allowed(IssuanceHold::new(move || async move {
            let _ = reservations
                .release(
                    &reservation_for_release.key,
                    &reservation_for_release.nonce,
                    now_unix_secs(),
                )
                .await;
            release_peers(&allowed_peers, &reservation_for_release, peer_rpc_port).await;
        }))
    }
}

enum PeerPrepareOutcome {
    Allow,
    ExplicitDeny,
    Unreachable,
}

async fn prepare_peer(
    peer: &PeerAddress,
    reservation: &Reservation,
    peer_rpc_port: u16,
) -> PeerPrepareOutcome {
    let request = DaemonRequest::Coord {
        op: CoordOp::Prepare {
            id: reservation.id.0.clone(),
            key: to_api_resource_key(&reservation.key),
            owner: reservation.owner.clone(),
            nonce: reservation.nonce.clone(),
            ttl_secs: reservation.expires_at.saturating_sub(now_unix_secs()),
        },
    };
    match timeout(
        peer_rpc::PEER_RPC_TIMEOUT,
        peer_rpc::overlay_rpc(peer.overlay_ip, peer_rpc_port, request),
    )
    .await
    {
        Ok(Ok(response)) if response.ok => PeerPrepareOutcome::Allow,
        Ok(Ok(response)) if response.code == "COORDINATION_DENIED" => {
            warn!(peer = %peer.machine_id, message = %response.message, "cert issuance vetoed by peer");
            PeerPrepareOutcome::ExplicitDeny
        }
        Ok(Ok(response)) => {
            warn!(
                peer = %peer.machine_id,
                code = %response.code,
                message = %response.message,
                "cert issuance prepare returned non-ok; abstaining"
            );
            PeerPrepareOutcome::Unreachable
        }
        Ok(Err(error)) => {
            warn!(peer = %peer.machine_id, %error, "cert issuance prepare rpc failed; abstaining");
            PeerPrepareOutcome::Unreachable
        }
        Err(_) => {
            warn!(peer = %peer.machine_id, "cert issuance prepare rpc timed out; abstaining");
            PeerPrepareOutcome::Unreachable
        }
    }
}

async fn release_peers(peers: &[PeerAddress], reservation: &Reservation, peer_rpc_port: u16) {
    for peer in peers {
        let request = DaemonRequest::Coord {
            op: CoordOp::Release {
                id: reservation.id.0.clone(),
                key: to_api_resource_key(&reservation.key),
                nonce: reservation.nonce.clone(),
            },
        };
        if let Err(error) = peer_rpc::overlay_rpc(peer.overlay_ip, peer_rpc_port, request).await {
            warn!(peer = %peer.machine_id, %error, "cert issuance release rpc failed");
        }
    }
}

fn to_api_resource_key(key: &ResourceKey) -> ApiResourceKey {
    match key {
        ResourceKey::Subnet(subnet) => ApiResourceKey::Subnet(*subnet),
        ResourceKey::DeployNamespace(namespace) => {
            ApiResourceKey::DeployNamespace(namespace.clone())
        }
        ResourceKey::CertIssuance(hostname) => ApiResourceKey::CertIssuance(hostname.clone()),
        ResourceKey::AcmeAccount(issuer_url) => ApiResourceKey::AcmeAccount(issuer_url.clone()),
    }
}

fn self_id_value(self_id: &MachineId) -> MachineId {
    self_id.clone()
}

pub struct OverlayChallengeReadiness {
    store: StoreDriver,
    self_id: MachineId,
    peer_rpc_port: u16,
}

impl OverlayChallengeReadiness {
    #[must_use]
    pub fn new(store: StoreDriver, self_id: MachineId, peer_rpc_port: u16) -> Self {
        Self {
            store,
            self_id,
            peer_rpc_port,
        }
    }
}

#[async_trait]
impl Http01ChallengeReadiness for OverlayChallengeReadiness {
    async fn wait_ready(&self, store: &StoreDriver, hostname: &str, token: &str) -> Result<()> {
        wait_for_local_challenge(store, hostname, token).await?;

        let peers = match self.store.list_machines().await {
            Ok(machines) => coordination_peers(&machines, &self.self_id)
                .into_iter()
                .map(|machine| PeerAddress {
                    machine_id: machine.id.clone(),
                    overlay_ip: machine.overlay_ip,
                })
                .collect::<Vec<_>>(),
            Err(error) => {
                warn!(
                    hostname = %hostname,
                    ?error,
                    "could not list peers for ACME challenge readiness; proceeding with local confirmation"
                );
                Vec::new()
            }
        };

        let outcomes = join_all(peers.iter().map(|peer| {
            let hostname = hostname.to_string();
            let token = token.to_string();
            let peer = peer.clone();
            let port = self.peer_rpc_port;
            async move { challenge_ready_peer(&peer, &hostname, &token, port).await }
        }))
        .await;

        for (peer, outcome) in peers.iter().zip(outcomes.iter()) {
            match outcome {
                ChallengeReadyOutcome::Ready | ChallengeReadyOutcome::Unreachable => {}
                ChallengeReadyOutcome::NotReady(message) => {
                    return Err(Error::operation(
                        "acme_challenge_visibility",
                        format!(
                            "HTTP-01 challenge for {hostname} was not visible on peer {} within {:?}: {message}",
                            peer.machine_id, HTTP01_CHALLENGE_VISIBILITY_TIMEOUT
                        ),
                    ));
                }
            }
        }

        Ok(())
    }
}

enum ChallengeReadyOutcome {
    Ready,
    NotReady(String),
    Unreachable,
}

async fn challenge_ready_peer(
    peer: &PeerAddress,
    hostname: &str,
    token: &str,
    peer_rpc_port: u16,
) -> ChallengeReadyOutcome {
    let request = DaemonRequest::AcmeChallengeReady {
        hostname: hostname.to_string(),
        token: token.to_string(),
    };
    let read_timeout = HTTP01_CHALLENGE_VISIBILITY_TIMEOUT + peer_rpc::PEER_RPC_TIMEOUT;
    match peer_rpc::overlay_rpc_expect_ok_with_read_timeout(
        peer.overlay_ip,
        peer_rpc_port,
        request,
        read_timeout,
    )
    .await
    {
        Ok(()) => ChallengeReadyOutcome::Ready,
        Err(error) if error.contains("remote daemon error [ACME_CHALLENGE_NOT_READY]") => {
            ChallengeReadyOutcome::NotReady(error)
        }
        Err(error) => {
            warn!(peer = %peer.machine_id, %error, "ACME challenge readiness rpc failed; abstaining");
            ChallengeReadyOutcome::Unreachable
        }
    }
}

impl DaemonState {
    pub(crate) async fn handle_acme_challenge_ready(
        &self,
        hostname: &str,
        token: &str,
    ) -> ployz_api::DaemonResponse {
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        match wait_for_local_challenge(&active.mesh.store, hostname, token).await {
            Ok(()) => self.ok("acme challenge ready"),
            Err(error) => self.err("ACME_CHALLENGE_NOT_READY", error.to_string()),
        }
    }
}

async fn wait_for_local_challenge(store: &StoreDriver, hostname: &str, token: &str) -> Result<()> {
    let start = tokio::time::Instant::now();
    loop {
        let visible = store
            .list_acme_challenges()
            .await?
            .iter()
            .any(|challenge| challenge.hostname == hostname && challenge.token == token);
        if visible {
            return Ok(());
        }
        if start.elapsed() >= HTTP01_CHALLENGE_VISIBILITY_TIMEOUT {
            return Err(Error::operation(
                "acme_challenge_visibility",
                format!(
                    "HTTP-01 challenge for {hostname} was not visible in local store within {:?}",
                    HTTP01_CHALLENGE_VISIBILITY_TIMEOUT
                ),
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
