use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::future::join_all;
use ployz_api::{CoordOp, DaemonRequest, ResourceKey as ApiResourceKey};
use ployz_nats::coord::locks::{Lease, NatsLocks};
use ployz_nats::subjects;
use ployz_orchestrator::certificates::{
    AccountAcquisition, AcmeAccountCoordinator, HTTP01_CHALLENGE_VISIBILITY_TIMEOUT,
    Http01ChallengeReadiness, IssuanceAcquisition, IssuanceCoordinator, IssuanceHold,
};
use ployz_orchestrator::coordination::{
    PendingReservations, Reservation, ReservationId, ResourceKey, Vote,
};
use ployz_orchestrator::machine_policy::is_coordination_peer;
use ployz_store_api::{CertificateStore, MachineRegistry, RoutingSnapshotReader, StoreDriver};
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    MachineId, MachineLifecycle, MachineMembership, OverlayIp, RoutingState, ServiceReleaseRecord,
    ServiceRevisionRecord, ServiceRoutingPolicy,
};
use ployz_types::spec::{RouteSpec, ServiceSpec};
use ployz_types::time::now_unix_secs;
use tokio::time::{Instant, sleep, timeout};
use tracing::warn;

use crate::daemon::DaemonState;
use crate::daemon::handlers::peer_rpc;

/// Best-effort cluster-wide coordinator for ACME issuance and ACME account
/// creation. The peer inventory itself is required: if `list_machines` fails
/// we cannot know which active machines to ask, so the operation is deferred.
/// After inventory loads, known peer RPC failures are abstentions and explicit
/// `COORDINATION_DENIED` replies from reachable peers are vetoes.
pub struct OverlayIssuanceCoordinator {
    store: StoreDriver,
    reservations: Arc<PendingReservations>,
    self_id: MachineId,
    peer_rpc_port: u16,
    ttl: Duration,
}

const DEFAULT_ISSUANCE_TTL_SECS: u64 = 5 * 60;

#[derive(Clone)]
pub struct NatsIssuanceCoordinator {
    locks: NatsLocks,
    owner: MachineId,
    ttl: Duration,
}

impl NatsIssuanceCoordinator {
    #[must_use]
    pub fn new(locks: NatsLocks, owner: MachineId) -> Self {
        Self {
            locks,
            owner,
            ttl: Duration::from_secs(DEFAULT_ISSUANCE_TTL_SECS),
        }
    }

    async fn acquire_key(&self, key: String) -> std::result::Result<Lease, Error> {
        self.locks
            .acquire(
                &key,
                self.owner.0.clone(),
                ReservationId::random().0,
                self.ttl,
                now_unix_secs().saturating_add(self.ttl.as_secs()),
            )
            .await
    }

    fn hold_for(&self, lease: Lease) -> IssuanceHold {
        let locks = self.locks.clone();
        IssuanceHold::new(move || async move {
            if let Err(error) = locks.release(lease).await {
                warn!(%error, "failed to release NATS ACME coordination lock");
            }
        })
    }
}

#[async_trait]
impl IssuanceCoordinator for NatsIssuanceCoordinator {
    async fn try_acquire(&self, hostname: &str) -> IssuanceAcquisition {
        match self.acquire_key(subjects::cert_lock(hostname)).await {
            Ok(lease) => IssuanceAcquisition::Allowed(self.hold_for(lease)),
            Err(error) => IssuanceAcquisition::VetoedByPeer(error.to_string()),
        }
    }
}

#[async_trait]
impl AcmeAccountCoordinator for NatsIssuanceCoordinator {
    async fn try_acquire_account(&self, issuer_url: &str) -> AccountAcquisition {
        match self
            .acquire_key(subjects::acme_account_lock(issuer_url))
            .await
        {
            Ok(lease) => AccountAcquisition::Allowed(self.hold_for(lease)),
            Err(error) => AccountAcquisition::VetoedByPeer(error.to_string()),
        }
    }
}

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
    async fn release_local_reservation(&self, reservation: &Reservation) {
        let _released = self
            .reservations
            .release(&reservation.key, &reservation.nonce, now_unix_secs())
            .await;
    }

    async fn veto_with_local_release(
        &self,
        reservation: &Reservation,
        reason: String,
    ) -> ResourceAcquisition {
        self.release_local_reservation(reservation).await;
        ResourceAcquisition::VetoedByPeer(reason)
    }

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
            Ok(machines) => machines
                .iter()
                .filter(|machine| {
                    is_coordination_peer(&machine.placement_candidate(), &self.self_id)
                })
                .map(|machine| PeerAddress {
                    machine_id: machine.id.clone(),
                    overlay_ip: machine.overlay_ip,
                })
                .collect::<Vec<_>>(),
            Err(error) => {
                let _released = self
                    .reservations
                    .release(&reservation.key, &reservation.nonce, now_unix_secs())
                    .await;
                warn!(
                    %resource_kind,
                    resource = %resource_value,
                    ?error,
                    "could not list peers for coordination lock; deferring"
                );
                return self
                    .veto_with_local_release(
                        &reservation,
                        format!("peer inventory unavailable: {error}"),
                    )
                    .await;
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
                self.release_local_reservation(&reservation).await;
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
    release_peers_with(
        peers,
        reservation,
        peer_rpc_port,
        |peer, request, port| async move {
            let _response = peer_rpc::overlay_rpc(peer.overlay_ip, port, request).await?;
            Ok(())
        },
    )
    .await;
}

async fn release_peers_with<F, Fut>(
    peers: &[PeerAddress],
    reservation: &Reservation,
    peer_rpc_port: u16,
    release: F,
) where
    F: Fn(PeerAddress, DaemonRequest, u16) -> Fut,
    Fut: std::future::Future<Output = std::result::Result<(), String>>,
{
    join_all(peers.iter().cloned().map(|peer| {
        let request = release_request(reservation);
        let release = &release;
        async move {
            let machine_id = peer.machine_id.clone();
            if let Err(error) = release(peer, request, peer_rpc_port).await {
                warn!(peer = %machine_id, %error, "cert issuance release rpc failed");
            }
        }
    }))
    .await;
}

fn release_request(reservation: &Reservation) -> DaemonRequest {
    DaemonRequest::Coord {
        op: CoordOp::Release {
            id: reservation.id.0.clone(),
            key: to_api_resource_key(&reservation.key),
            nonce: reservation.nonce.clone(),
        },
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

#[allow(dead_code)]
pub struct OverlayChallengeReadiness {
    store: StoreDriver,
    self_id: MachineId,
    peer_rpc_port: u16,
}

pub struct NatsChallengeReadiness {
    store: StoreDriver,
}

impl NatsChallengeReadiness {
    #[must_use]
    pub fn new(store: StoreDriver) -> Self {
        Self { store }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChallengeEligibility {
    eligible: BTreeSet<MachineId>,
    excluded: Vec<ChallengeReadinessExclusion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChallengeReadinessExclusion {
    machine_id: MachineId,
    reason: &'static str,
}

#[allow(dead_code)]
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
impl Http01ChallengeReadiness for NatsChallengeReadiness {
    async fn wait_ready(&self, store: &StoreDriver, hostname: &str, token: &str) -> Result<()> {
        wait_for_local_challenge(store, hostname, token).await?;

        let routing = self.store.load_routing_state().await.map_err(|error| {
            Error::operation(
                "acme_challenge_visibility",
                format!("eligibility_unknown: could not load routing state: {error}"),
            )
        })?;
        let eligibility = challenge_eligibility(&routing, hostname)?;
        if eligibility.eligible.is_empty() {
            return Err(Error::operation(
                "acme_challenge_visibility",
                format!(
                    "eligibility_unknown: no active advertised gateway is eligible for HTTP-01 challenge {hostname}; excluded={}",
                    format_exclusions(&eligibility.excluded)
                ),
            ));
        }

        let deadline = Instant::now() + HTTP01_CHALLENGE_VISIBILITY_TIMEOUT;
        loop {
            let records = self
                .store
                .list_acme_challenge_readiness(hostname, token)
                .await
                .map_err(|error| {
                    Error::operation(
                        "acme_challenge_visibility",
                        format!(
                            "eligibility_unknown: could not list readiness observations: {error}"
                        ),
                    )
                })?;
            let observed = records
                .into_iter()
                .map(|record| record.machine_id)
                .collect::<BTreeSet<_>>();
            let missing = eligibility
                .eligible
                .difference(&observed)
                .cloned()
                .collect::<Vec<_>>();
            if missing.is_empty() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::operation(
                    "acme_challenge_visibility",
                    format!(
                        "HTTP-01 challenge for {hostname} token {token} is missing readiness observations from advertised eligible gateways: {}; reason=missing_ack; excluded={}",
                        format_machine_ids(&missing),
                        format_exclusions(&eligibility.excluded)
                    ),
                ));
            }
            sleep(Duration::from_millis(100)).await;
        }
    }
}

#[async_trait]
impl Http01ChallengeReadiness for OverlayChallengeReadiness {
    async fn wait_ready(&self, store: &StoreDriver, hostname: &str, token: &str) -> Result<()> {
        wait_for_local_challenge(store, hostname, token).await?;

        // HTTP-01 readiness is not a mutation lock. It is a projection check:
        // missing peer inventory falls back to local readiness, known
        // unreachable peers abstain, and reachable peers that have not yet
        // replicated the challenge keep the order in Issuing for a later
        // finalization pass.
        let peers =
            challenge_readiness_peers(self.store.list_machines().await, &self.self_id, hostname);

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

#[allow(dead_code)]
fn challenge_readiness_peers(
    machines: Result<Vec<ployz_types::model::MachineMembership>>,
    self_id: &MachineId,
    hostname: &str,
) -> Vec<PeerAddress> {
    match machines {
        Ok(machines) => machines
            .iter()
            .filter(|machine| is_coordination_peer(&machine.placement_candidate(), self_id))
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
    }
}

#[allow(dead_code)]
enum ChallengeReadyOutcome {
    Ready,
    NotReady(String),
    Unreachable,
}

#[allow(dead_code)]
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

fn challenge_eligibility(routing: &RoutingState, hostname: &str) -> Result<ChallengeEligibility> {
    if !hostname_is_advertised(routing, hostname)? {
        return Err(Error::operation(
            "acme_challenge_visibility",
            format!(
                "eligibility_unknown: hostname {hostname} is not in the current advertised routing state"
            ),
        ));
    }

    let mut eligible = BTreeSet::new();
    let mut excluded = Vec::new();
    for machine in &routing.machines {
        match readiness_exclusion(machine) {
            Some(reason) => excluded.push(ChallengeReadinessExclusion {
                machine_id: machine.id.clone(),
                reason,
            }),
            None => {
                eligible.insert(machine.id.clone());
            }
        }
    }

    Ok(ChallengeEligibility { eligible, excluded })
}

fn readiness_exclusion(machine: &MachineMembership) -> Option<&'static str> {
    match machine.lifecycle {
        MachineLifecycle::Active => {}
        MachineLifecycle::Standby => return Some("excluded_by_lifecycle"),
        MachineLifecycle::Draining => return Some("excluded_by_lifecycle"),
    }
    if machine.subnet.is_none() {
        return Some("no_subnet");
    }
    None
}

fn hostname_is_advertised(routing: &RoutingState, hostname: &str) -> Result<bool> {
    let normalized = normalize_hostname(hostname);
    for release in &routing.releases {
        for revision in active_release_revisions(release) {
            for record in matching_revisions(routing, release, revision) {
                let spec: ServiceSpec =
                    serde_json::from_str(&record.spec_json).map_err(|error| {
                        Error::operation(
                            "acme_challenge_visibility",
                            format!(
                                "eligibility_unknown: invalid service revision {}/{}@{}: {error}",
                                record.namespace, record.service, record.revision_hash
                            ),
                        )
                    })?;
                if spec.routes.iter().any(|route| match route {
                    RouteSpec::Http(route) => route
                        .hostnames
                        .iter()
                        .any(|candidate| normalize_hostname(candidate) == normalized),
                    RouteSpec::Tcp(_) => false,
                }) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn active_release_revisions(release: &ServiceReleaseRecord) -> Vec<&str> {
    match &release.release.routing {
        ServiceRoutingPolicy::Direct { revision_hash } => vec![revision_hash.as_str()],
        ServiceRoutingPolicy::Split { allocations } => allocations
            .iter()
            .filter(|allocation| allocation.percent > 0)
            .map(|allocation| allocation.revision_hash.as_str())
            .collect(),
    }
}

fn matching_revisions<'a>(
    routing: &'a RoutingState,
    release: &ServiceReleaseRecord,
    revision_hash: &str,
) -> impl Iterator<Item = &'a ServiceRevisionRecord> {
    routing.revisions.iter().filter(move |record| {
        record.namespace == release.namespace
            && record.service == release.service
            && record.revision_hash == revision_hash
    })
}

fn normalize_hostname(hostname: &str) -> String {
    hostname.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn format_machine_ids(machine_ids: &[MachineId]) -> String {
    machine_ids
        .iter()
        .map(|machine_id| machine_id.0.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

fn format_exclusions(exclusions: &[ChallengeReadinessExclusion]) -> String {
    if exclusions.is_empty() {
        return "none".into();
    }
    exclusions
        .iter()
        .map(|excluded| format!("{}:{}", excluded.machine_id, excluded.reason))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{DeployId, MachineRole, MachineTopology, PublicKey, ServiceRelease};
    use ployz_types::spec::Namespace;
    use std::net::Ipv6Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    #[tokio::test]
    async fn release_peers_starts_peer_releases_concurrently() {
        let peers = vec![
            PeerAddress {
                machine_id: MachineId("peer-a".into()),
                overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            },
            PeerAddress {
                machine_id: MachineId("peer-b".into()),
                overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            },
        ];
        let reservation = Reservation {
            id: ReservationId("reservation".into()),
            key: ResourceKey::CertIssuance("example.com".into()),
            owner: MachineId("self".into()),
            nonce: "nonce".into(),
            expires_at: now_unix_secs() + 60,
        };
        let started = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(Notify::new());

        let timeout_result = tokio::time::timeout(
            Duration::from_millis(500),
            release_peers_with(&peers, &reservation, 4318, {
                let started = Arc::clone(&started);
                let max_in_flight = Arc::clone(&max_in_flight);
                let notify = Arc::clone(&notify);
                move |_peer, _request, _port| {
                    let started = Arc::clone(&started);
                    let max_in_flight = Arc::clone(&max_in_flight);
                    let notify = Arc::clone(&notify);
                    async move {
                        let in_flight = started.fetch_add(1, Ordering::SeqCst) + 1;
                        max_in_flight.fetch_max(in_flight, Ordering::SeqCst);
                        if in_flight == 2 {
                            notify.notify_waiters();
                        }
                        while started.load(Ordering::SeqCst) < 2 {
                            notify.notified().await;
                        }
                        Ok(())
                    }
                }
            }),
        )
        .await;
        assert!(
            timeout_result.is_ok(),
            "release fanout should not serialize peers; timed out waiting for both peer releases"
        );

        assert_eq!(max_in_flight.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn challenge_readiness_peer_inventory_failure_falls_back_to_local_only() {
        let peers = challenge_readiness_peers(
            Err(Error::operation("list_machines", "inventory unavailable")),
            &MachineId("self".into()),
            "example.com",
        );

        assert!(peers.is_empty());
    }

    #[test]
    fn challenge_eligibility_excludes_standby_and_no_subnet_machines() {
        let routing = RoutingState {
            machines: vec![
                test_machine("active", MachineLifecycle::Active, true),
                test_machine("standby", MachineLifecycle::Standby, true),
                test_machine("no-subnet", MachineLifecycle::Active, false),
            ],
            revisions: vec![test_revision("api", "rev-a", "example.com")],
            releases: vec![test_release("api", "rev-a")],
            instances: Vec::new(),
        };

        let eligibility =
            challenge_eligibility(&routing, "example.com").expect("hostname should be advertised");

        assert!(eligibility.eligible.contains(&MachineId("active".into())));
        assert!(!eligibility.eligible.contains(&MachineId("standby".into())));
        assert!(
            !eligibility
                .eligible
                .contains(&MachineId("no-subnet".into()))
        );
        assert!(eligibility.excluded.iter().any(|excluded| {
            excluded.machine_id == MachineId("standby".into())
                && excluded.reason == "excluded_by_lifecycle"
        }));
        assert!(eligibility.excluded.iter().any(|excluded| {
            excluded.machine_id == MachineId("no-subnet".into()) && excluded.reason == "no_subnet"
        }));
    }

    #[test]
    fn challenge_eligibility_fails_loudly_when_hostname_is_unknown() {
        let routing = RoutingState {
            machines: vec![test_machine("active", MachineLifecycle::Active, true)],
            revisions: vec![test_revision("api", "rev-a", "example.com")],
            releases: vec![test_release("api", "rev-a")],
            instances: Vec::new(),
        };

        let error =
            challenge_eligibility(&routing, "missing.example.com").expect_err("must fail loudly");

        assert!(error.to_string().contains("eligibility_unknown"));
    }

    fn test_machine(id: &str, lifecycle: MachineLifecycle, has_subnet: bool) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.into()),
            public_key: PublicKey([0; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            topology: MachineTopology::local(),
            control_target: None,
            subnet: has_subnet.then(|| "10.0.0.0/24".parse().expect("valid cidr")),
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle,
            role: MachineRole::StorageCandidate,
            created_at: 1,
            updated_at: 1,
            labels: Default::default(),
        }
    }

    fn test_revision(service: &str, revision_hash: &str, hostname: &str) -> ServiceRevisionRecord {
        ServiceRevisionRecord {
            namespace: Namespace("prod".into()),
            service: service.into(),
            revision_hash: revision_hash.into(),
            spec_json: serde_json::json!({
                "name": service,
                "placement": "global",
                "template": { "image": "example/app:latest" },
                "network": "overlay",
                "service_ports": [{ "name": "http", "container_port": 8080 }],
                "routes": [{
                    "http": {
                        "service_port": "http",
                        "hostnames": [hostname],
                        "path_prefix": "/"
                    }
                }]
            })
            .to_string(),
            created_by: MachineId("active".into()),
            created_at: 1,
        }
    }

    fn test_release(service: &str, revision_hash: &str) -> ServiceReleaseRecord {
        ServiceReleaseRecord {
            namespace: Namespace("prod".into()),
            service: service.into(),
            release: ServiceRelease {
                primary_revision_hash: revision_hash.into(),
                referenced_revision_hashes: vec![revision_hash.into()],
                routing: ServiceRoutingPolicy::Direct {
                    revision_hash: revision_hash.into(),
                },
                slots: Vec::new(),
                updated_by_deploy_id: DeployId("deploy-1".into()),
                updated_at: 1,
            },
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
