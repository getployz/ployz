//! Bid fan-out for a deploy's placement: the driver asks every accepted
//! roster machine for live testimony and classifies every silence.
//!
//! A peer whose WireGuard handshake is already stale is expected-silent and
//! is skipped without burning an RPC timeout. A fresh peer that yields no bid
//! is anomalous-silent, with the concrete transport, timeout, or decline
//! reason recorded. The local machine always bids in-process through the same
//! collection the `/deploy/bid` responder runs.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hyper::StatusCode;
use ployz_core::corrosion::MachineTransport;
use ployz_core::ids::{ClusterId, MachineRowId};
use ployz_core::machine::{MachineLifecycle, MachineName};
use ployz_core::network::MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS;
use ployz_core::{
    AnomalousSilenceReason, HandshakeObservation, HandshakeObservationOutcome, PlacementBid,
    PlacementBidRequest, SilenceClassification, SilentMachine, V2Route,
};
use tokio::sync::watch;

use super::deploy_dispatch::machine_socket_addr;
use super::placement_http::{BID_COLLECTION_TIMEOUT, BidIdentity, collect_bid};
use super::store::read_accepted_roster;
use crate::corrosion::CorrosionClient;
use crate::roles::api::execution::docker::runner::DockerManagedContainerRunner;
use crate::roles::system_observation::SystemObservation;

/// Upper bound on one remote bid POST: the responder's own collection budget
/// plus transport headroom.
const BID_RPC_TIMEOUT: Duration = Duration::from_secs(BID_COLLECTION_TIMEOUT.as_secs() + 10);

/// One accepted roster machine the gather can address.
#[derive(Debug, Clone)]
pub(super) struct PlacementPeer {
    pub(super) id: MachineRowId,
    pub(super) name: MachineName,
    pub(super) lifecycle: MachineLifecycle,
    pub(super) transport: MachineTransport,
}

/// The gather's outcome: every live bid and every classified silence.
/// Silence is roster minus responders, never "no candidates".
#[derive(Debug, Clone)]
pub(super) struct GatheredBids {
    pub(super) bids: Vec<PlacementBid>,
    pub(super) silent: Vec<SilentMachine>,
}

/// The mesh surface a bid gather runs over. One real implementation reads
/// the accepted roster, asks the local Keeper for handshake ages, and POSTs
/// `/deploy/bid`; tests fake the seam.
#[async_trait]
pub(super) trait PlacementMesh: Send + Sync {
    /// Accepted roster machines, including the local machine.
    async fn roster(&self) -> Result<Vec<PlacementPeer>, String>;
    /// The live WireGuard handshake age for a peer, when observable.
    /// `None` means the age is unknown and the peer must be asked directly.
    async fn handshake_age_seconds(&self, transport: &MachineTransport) -> Option<u64>;
    async fn remote_bid(
        &self,
        transport: &MachineTransport,
        request: &PlacementBidRequest,
    ) -> Result<PlacementBid, AnomalousSilenceReason>;
    async fn local_bid(
        &self,
        peer: &PlacementPeer,
        request: &PlacementBidRequest,
    ) -> Result<PlacementBid, AnomalousSilenceReason>;
}

enum BidOutcome {
    Bid(Box<PlacementBid>),
    ExpectedSilent { handshake_age_seconds: u64 },
    AnomalousSilent { reason: AnomalousSilenceReason },
}

/// Fans the bid request out over the roster and classifies every machine as
/// bidder, expected-silent, or anomalous-silent.
pub(super) async fn gather_bids(
    mesh: &dyn PlacementMesh,
    peers: &[PlacementPeer],
    local_machine_id: &MachineRowId,
    request: &PlacementBidRequest,
) -> GatheredBids {
    let outcomes = futures_util::future::join_all(
        peers
            .iter()
            .map(|peer| collect_one(mesh, local_machine_id, peer, request)),
    )
    .await;
    let mut bids = Vec::new();
    let mut silent = Vec::new();
    for (peer, outcome) in peers.iter().zip(outcomes) {
        match outcome {
            BidOutcome::Bid(bid) => bids.push(*bid),
            BidOutcome::ExpectedSilent {
                handshake_age_seconds,
            } => silent.push(SilentMachine {
                machine_id: peer.id.clone(),
                classification: SilenceClassification::ExpectedSilent {
                    handshake_age_seconds,
                },
            }),
            BidOutcome::AnomalousSilent { reason } => silent.push(SilentMachine {
                machine_id: peer.id.clone(),
                classification: SilenceClassification::AnomalousSilent { reason },
            }),
        }
    }
    GatheredBids { bids, silent }
}

async fn collect_one(
    mesh: &dyn PlacementMesh,
    local_machine_id: &MachineRowId,
    peer: &PlacementPeer,
    request: &PlacementBidRequest,
) -> BidOutcome {
    if &peer.id == local_machine_id {
        return match mesh.local_bid(peer, request).await {
            Ok(bid) => BidOutcome::Bid(Box::new(bid)),
            Err(reason) => BidOutcome::AnomalousSilent { reason },
        };
    }
    if let Some(age) = mesh.handshake_age_seconds(&peer.transport).await
        && age > MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS
    {
        return BidOutcome::ExpectedSilent {
            handshake_age_seconds: age,
        };
    }
    match mesh.remote_bid(&peer.transport, request).await {
        Ok(bid) => BidOutcome::Bid(Box::new(bid)),
        Err(reason) => BidOutcome::AnomalousSilent { reason },
    }
}

/// The real mesh: accepted roster over the local Corrosion replica, Keeper
/// handshake observation, and bounded `/deploy/bid` POSTs to peers on the
/// shared API port.
pub(super) struct CorrosionPlacementMesh {
    corrosion: CorrosionClient,
    cluster_id: ClusterId,
    keeper_control_socket_path: PathBuf,
    api_port: u16,
    client: reqwest::Client,
    runner: Arc<DockerManagedContainerRunner>,
}

impl CorrosionPlacementMesh {
    #[must_use]
    pub(super) fn new(
        corrosion: CorrosionClient,
        cluster_id: ClusterId,
        keeper_control_socket_path: PathBuf,
        api_port: u16,
        client: reqwest::Client,
        runner: Arc<DockerManagedContainerRunner>,
    ) -> Self {
        Self {
            corrosion,
            cluster_id,
            keeper_control_socket_path,
            api_port,
            client,
            runner,
        }
    }
}

#[async_trait]
impl PlacementMesh for CorrosionPlacementMesh {
    async fn roster(&self) -> Result<Vec<PlacementPeer>, String> {
        let roster = read_accepted_roster(&self.corrosion, &self.cluster_id)
            .await
            .map_err(|error| format!("accepted roster unavailable: {error}"))?;
        Ok(roster
            .machines
            .into_iter()
            .map(|machine| PlacementPeer {
                id: machine.id,
                name: machine.document.name.clone(),
                lifecycle: machine.document.lifecycle,
                transport: machine.document.transport,
            })
            .collect())
    }

    async fn handshake_age_seconds(&self, transport: &MachineTransport) -> Option<u64> {
        let public_key = match transport {
            MachineTransport::Wireguard { pubkey, .. } => pubkey.clone(),
            MachineTransport::Tailscale { .. } => return None,
        };
        // The Keeper request is internally deadline-bounded; the channel only
        // exists to satisfy the shutdown-aware control protocol.
        let (_hold, shutdown) = watch::channel(false);
        match crate::roles::api::keeper_control::observe_handshake(
            &self.keeper_control_socket_path,
            public_key,
            shutdown,
        )
        .await
        {
            HandshakeObservationOutcome::Observed {
                observation: HandshakeObservation::Ago { age_seconds, .. },
            } => Some(age_seconds),
            HandshakeObservationOutcome::Observed {
                observation: HandshakeObservation::Never { .. },
            }
            | HandshakeObservationOutcome::Unavailable { .. } => None,
        }
    }

    async fn remote_bid(
        &self,
        transport: &MachineTransport,
        request: &PlacementBidRequest,
    ) -> Result<PlacementBid, AnomalousSilenceReason> {
        let target = machine_socket_addr(transport, self.api_port);
        let url = format!("http://{target}{}", V2Route::PlacementBid.path());
        let response = self
            .client
            .post(url)
            .timeout(BID_RPC_TIMEOUT)
            .json(request)
            .send()
            .await
            .map_err(|error| {
                tracing::warn!(%target, error = %error, "placement bid transport failed");
                if error.is_timeout() {
                    AnomalousSilenceReason::TimedOut
                } else {
                    AnomalousSilenceReason::TransportFailed
                }
            })?;
        let status = response.status();
        if status != StatusCode::OK.as_u16() {
            tracing::warn!(%target, %status, "placement bid declined");
            return Err(AnomalousSilenceReason::Declined {
                status: status.as_u16(),
            });
        }
        response.json::<PlacementBid>().await.map_err(|error| {
            tracing::warn!(%target, error = %error, "placement bid reply was invalid");
            AnomalousSilenceReason::TransportFailed
        })
    }

    async fn local_bid(
        &self,
        peer: &PlacementPeer,
        request: &PlacementBidRequest,
    ) -> Result<PlacementBid, AnomalousSilenceReason> {
        let observation = SystemObservation::read().map_err(|error| {
            tracing::warn!(error = %error, "local bid observation failed");
            AnomalousSilenceReason::TransportFailed
        })?;
        let collected = tokio::time::timeout(
            BID_COLLECTION_TIMEOUT,
            collect_bid(
                self.runner.as_ref(),
                BidIdentity {
                    machine_id: peer.id.clone(),
                    machine_name: peer.name.clone(),
                    lifecycle: peer.lifecycle,
                },
                observation,
                request,
            ),
        )
        .await
        .map_err(|_| AnomalousSilenceReason::TimedOut)?;
        collected.map_err(|diagnostic| {
            tracing::warn!(diagnostic, "local bid collection failed");
            AnomalousSilenceReason::TransportFailed
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ployz_core::corrosion::{MachineLoadBand, MachineTransport};
    use ployz_core::ids::{MachineRowId, NamespaceRowId, ServiceRowId};
    use ployz_core::machine::{MachineLifecycle, MachineName};
    use ployz_core::network::MachineEndpointSubnet;
    use ployz_core::{
        AnomalousSilenceReason, PlacementBid, PlacementBidRequest, SilenceClassification,
    };

    use super::{GatheredBids, PlacementMesh, PlacementPeer, gather_bids};

    fn machine(id: &str) -> MachineRowId {
        MachineRowId::try_new(id).expect("machine id")
    }

    fn peer(id: &str, name: &str) -> PlacementPeer {
        PlacementPeer {
            id: machine(id),
            name: MachineName::try_new(name).expect("name"),
            lifecycle: MachineLifecycle::Active,
            transport: MachineTransport::Tailscale {
                ip: Ipv4Addr::new(100, 64, 0, 7),
                subnet_v4: MachineEndpointSubnet::try_new("10.210.20.0/24").expect("subnet"),
            },
        }
    }

    fn bid(id: &str, name: &str) -> PlacementBid {
        PlacementBid {
            machine_id: machine(id),
            machine_name: MachineName::try_new(name).expect("name"),
            architecture: "x86_64".to_owned(),
            lifecycle: MachineLifecycle::Active,
            free_disk_bytes: 20 * 1024 * 1024 * 1024,
            free_memory_bytes: 4 * 1024 * 1024 * 1024,
            load: MachineLoadBand::Idle,
            total_container_count: 0,
            service_containers: Vec::new(),
            volumes_held: BTreeSet::new(),
        }
    }

    fn request() -> PlacementBidRequest {
        PlacementBidRequest {
            namespace_id: NamespaceRowId::try_new("01J00000000000000000000013").expect("namespace"),
            service_id: ServiceRowId::try_new("01J00000000000000000000014").expect("service"),
            volumes: BTreeSet::new(),
            active_deploy: None,
        }
    }

    enum PeerScript {
        Bids,
        HandshakeStale { age: u64 },
        TransportFails,
        Declines,
    }

    struct FakeMesh {
        peers: Vec<(PlacementPeer, PeerScript)>,
        remote_calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl PlacementMesh for FakeMesh {
        async fn roster(&self) -> Result<Vec<PlacementPeer>, String> {
            Ok(self.peers.iter().map(|(peer, _)| peer.clone()).collect())
        }

        async fn handshake_age_seconds(&self, transport: &MachineTransport) -> Option<u64> {
            let MachineTransport::Tailscale { ip, .. } = transport else {
                return None;
            };
            self.peers
                .iter()
                .find(|(peer, _)| matches!(&peer.transport, MachineTransport::Tailscale { ip: peer_ip, .. } if peer_ip == ip))
                .map(|(_, script)| match script {
                    PeerScript::HandshakeStale { age } => *age,
                    PeerScript::Bids | PeerScript::TransportFails | PeerScript::Declines => 1,
                })
        }

        async fn remote_bid(
            &self,
            transport: &MachineTransport,
            _request: &PlacementBidRequest,
        ) -> Result<PlacementBid, AnomalousSilenceReason> {
            self.remote_calls.fetch_add(1, Ordering::SeqCst);
            let MachineTransport::Tailscale { ip, .. } = transport else {
                return Err(AnomalousSilenceReason::TransportFailed);
            };
            let Some((peer, script)) = self
                .peers
                .iter()
                .find(|(peer, _)| matches!(&peer.transport, MachineTransport::Tailscale { ip: peer_ip, .. } if peer_ip == ip))
            else {
                return Err(AnomalousSilenceReason::TransportFailed);
            };
            match script {
                PeerScript::Bids => Ok(bid(peer.id.as_str(), peer.name.as_str())),
                PeerScript::TransportFails => Err(AnomalousSilenceReason::TransportFailed),
                PeerScript::Declines => Err(AnomalousSilenceReason::Declined { status: 503 }),
                PeerScript::HandshakeStale { .. } => {
                    panic!("stale-handshake peers must never be dialled")
                }
            }
        }

        async fn local_bid(
            &self,
            peer: &PlacementPeer,
            _request: &PlacementBidRequest,
        ) -> Result<PlacementBid, AnomalousSilenceReason> {
            Ok(bid(peer.id.as_str(), peer.name.as_str()))
        }
    }

    fn tailscale_ip(index: u8) -> MachineTransport {
        MachineTransport::Tailscale {
            ip: Ipv4Addr::new(100, 64, 0, index),
            subnet_v4: MachineEndpointSubnet::try_new("10.210.20.0/24").expect("subnet"),
        }
    }

    fn scripted(peers: Vec<(PlacementPeer, PeerScript)>) -> FakeMesh {
        FakeMesh {
            peers,
            remote_calls: AtomicUsize::new(0),
        }
    }

    async fn run(mesh: &FakeMesh, local: &str) -> GatheredBids {
        let peers = mesh.roster().await.expect("roster");
        gather_bids(mesh, &peers, &machine(local), &request()).await
    }

    #[tokio::test]
    async fn stale_handshake_is_expected_silent_without_an_rpc() {
        let mut stale = peer("01J00000000000000000000002", "worker-1");
        stale.transport = tailscale_ip(2);
        let mesh = scripted(vec![
            (
                peer("01J00000000000000000000001", "driver"),
                PeerScript::Bids,
            ),
            (stale, PeerScript::HandshakeStale { age: 600 }),
        ]);
        let gathered = run(&mesh, "01J00000000000000000000001").await;
        assert_eq!(gathered.bids.len(), 1);
        let [silent] = gathered.silent.as_slice() else {
            panic!("one silent machine expected");
        };
        assert_eq!(silent.machine_id, machine("01J00000000000000000000002"));
        assert_eq!(
            silent.classification,
            SilenceClassification::ExpectedSilent {
                handshake_age_seconds: 600
            }
        );
        // The local bid is in-process, and the stale peer is skipped: no RPC.
        assert_eq!(mesh.remote_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn fresh_peer_failures_and_declines_are_anomalous_silent_with_reasons() {
        let mut failing = peer("01J00000000000000000000002", "worker-1");
        failing.transport = tailscale_ip(2);
        let mut declining = peer("01J00000000000000000000003", "worker-2");
        declining.transport = tailscale_ip(3);
        let mesh = scripted(vec![
            (
                peer("01J00000000000000000000001", "driver"),
                PeerScript::Bids,
            ),
            (failing, PeerScript::TransportFails),
            (declining, PeerScript::Declines),
        ]);
        let gathered = run(&mesh, "01J00000000000000000000001").await;
        assert_eq!(gathered.bids.len(), 1);
        let classifications: Vec<_> = gathered
            .silent
            .iter()
            .map(|silent| silent.classification.clone())
            .collect();
        assert_eq!(
            classifications,
            vec![
                SilenceClassification::AnomalousSilent {
                    reason: AnomalousSilenceReason::TransportFailed,
                },
                SilenceClassification::AnomalousSilent {
                    reason: AnomalousSilenceReason::Declined { status: 503 },
                },
            ]
        );
    }

    #[tokio::test]
    async fn the_local_machine_always_bids_in_process() {
        let mesh = scripted(vec![(
            peer("01J00000000000000000000001", "driver"),
            PeerScript::Bids,
        )]);
        let gathered = run(&mesh, "01J00000000000000000000001").await;
        assert_eq!(gathered.bids.len(), 1);
        assert!(gathered.silent.is_empty());
        assert_eq!(mesh.remote_calls.load(Ordering::SeqCst), 0);
    }
}
