use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{RwLock, mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::error::Result as PloyzResult;
use crate::mesh::driver::WireguardDriver;
use crate::mesh::peer_state::PeerStateMap;
use crate::mesh::{DevicePeer, WireGuardDevice};
use crate::model::{MachineEvent, MachineId, MachineMembership, MachineObservation, PublicKey};

const ENDPOINT_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(1);
const ENDPOINT_CONNECTION_TIMEOUT: Duration = Duration::from_secs(15);
const PEER_DOWN_INTERVAL: Duration = Duration::from_secs(275);

pub(crate) type EndpointSelectionMap = Arc<RwLock<HashMap<MachineId, String>>>;

#[derive(Debug)]
pub(crate) enum EndpointMaintainerCommand {
    TickNow {
        force_rotate: bool,
        complete: oneshot::Sender<()>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimePeerStatus {
    Unknown,
    Up,
    Down,
}

#[derive(Debug, Clone)]
struct RuntimePeerState {
    machine_id: MachineId,
    public_key: PublicKey,
    all_endpoints: Vec<String>,
    current_endpoint: Option<String>,
    last_endpoint_change_at: Option<Instant>,
    last_handshake_at: Option<Instant>,
}

impl RuntimePeerState {
    fn new(observation: &MachineObservation, current_endpoint: Option<&str>, now: Instant) -> Self {
        let selected = select_current_endpoint(&observation.endpoints, current_endpoint);
        let last_endpoint_change_at = match (selected.is_some(), current_endpoint) {
            (true, Some(current))
                if observation
                    .endpoints
                    .iter()
                    .any(|endpoint| endpoint == current) =>
            {
                None
            }
            (true, _) => Some(now),
            (false, _) => None,
        };

        Self {
            machine_id: observation.identity.id.clone(),
            public_key: observation.identity.public_key.clone(),
            all_endpoints: observation.endpoints.clone(),
            current_endpoint: selected,
            last_endpoint_change_at,
            last_handshake_at: None,
        }
    }

    fn refresh_candidates(
        &mut self,
        observation: &MachineObservation,
        current_device_endpoint: Option<&str>,
        now: Instant,
    ) {
        self.public_key = observation.identity.public_key.clone();
        self.all_endpoints = observation.endpoints.clone();

        if let Some(endpoint) = current_device_endpoint
            && self
                .all_endpoints
                .iter()
                .any(|candidate| candidate == endpoint)
        {
            self.current_endpoint = Some(endpoint.to_string());
            self.last_endpoint_change_at = None;
            return;
        }

        if let Some(current_endpoint) = self.current_endpoint.as_deref()
            && self
                .all_endpoints
                .iter()
                .any(|candidate| candidate == current_endpoint)
        {
            return;
        }

        self.current_endpoint = self.all_endpoints.first().cloned();
        self.last_endpoint_change_at = self.current_endpoint.as_ref().map(|_| now);
    }

    fn update_from_device(&mut self, device_peer: Option<&DevicePeer>) {
        let Some(device_peer) = device_peer else {
            self.last_handshake_at = None;
            return;
        };

        if let Some(endpoint) = device_peer.endpoint.as_deref()
            && self
                .all_endpoints
                .iter()
                .any(|candidate| candidate == endpoint)
            && self.current_endpoint.as_deref() != Some(endpoint)
        {
            self.current_endpoint = Some(endpoint.to_string());
            self.last_endpoint_change_at = None;
        }

        self.last_handshake_at = device_peer.last_handshake;
    }

    fn status(&self, now: Instant) -> RuntimePeerStatus {
        if self.current_endpoint.is_none() {
            return RuntimePeerStatus::Unknown;
        }

        let since_endpoint_change = self
            .last_endpoint_change_at
            .map(|changed_at| now.duration_since(changed_at))
            .unwrap_or(PEER_DOWN_INTERVAL + Duration::from_secs(1));
        let since_last_handshake = self
            .last_handshake_at
            .map(|handshake_at| now.duration_since(handshake_at));
        let handshake_after_endpoint_change =
            match (self.last_handshake_at, self.last_endpoint_change_at) {
                (Some(_), None) => true,
                (Some(handshake_at), Some(changed_at)) => handshake_at > changed_at,
                (None, _) => false,
            };

        if since_endpoint_change > PEER_DOWN_INTERVAL {
            return match since_last_handshake {
                Some(duration) if duration < PEER_DOWN_INTERVAL => RuntimePeerStatus::Up,
                _ => RuntimePeerStatus::Down,
            };
        }

        if since_endpoint_change < ENDPOINT_CONNECTION_TIMEOUT {
            return if handshake_after_endpoint_change {
                RuntimePeerStatus::Up
            } else {
                RuntimePeerStatus::Unknown
            };
        }

        if handshake_after_endpoint_change {
            RuntimePeerStatus::Up
        } else {
            RuntimePeerStatus::Down
        }
    }

    fn next_endpoint(&self) -> Option<String> {
        if self.all_endpoints.is_empty() {
            return None;
        }
        let Some(current_endpoint) = self.current_endpoint.as_deref() else {
            return self.all_endpoints.first().cloned();
        };
        if let [only_endpoint] = self.all_endpoints.as_slice()
            && only_endpoint == current_endpoint
        {
            return None;
        }
        let current_idx = self
            .all_endpoints
            .iter()
            .position(|endpoint| endpoint == current_endpoint)
            .unwrap_or(0);
        let next_idx = (current_idx + 1) % self.all_endpoints.len();
        self.all_endpoints.get(next_idx).cloned()
    }
}

#[derive(Debug, Default)]
struct RuntimePeerMap {
    peers: HashMap<MachineId, RuntimePeerState>,
}

impl RuntimePeerMap {
    fn from_records(
        snapshot: &[MachineMembership],
        bootstrap_peers: &[MachineObservation],
        local_machine_id: &MachineId,
        device_peers: &[DevicePeer],
        now: Instant,
    ) -> Self {
        let mut peer_map = PeerStateMap::new();
        peer_map.init_from_snapshot(snapshot, now);
        for observation in bootstrap_peers {
            if observation.id() != local_machine_id {
                peer_map.upsert_transient(observation, now);
            }
        }

        let device_endpoint_by_key = device_endpoint_by_key(device_peers);
        let mut peers = HashMap::new();
        for peer in peer_map.effective_peers(local_machine_id) {
            if peer.endpoints.is_empty() {
                continue;
            }
            let observation = peer.to_observation();
            let current_endpoint =
                device_endpoint_by_key
                    .get(&peer.public_key)
                    .and_then(|endpoint| {
                        if peer.endpoints.iter().any(|candidate| candidate == endpoint) {
                            Some(endpoint.as_str())
                        } else {
                            None
                        }
                    });
            peers.insert(
                peer.id.clone(),
                RuntimePeerState::new(&observation, current_endpoint, now),
            );
        }
        Self { peers }
    }

    fn selection_map(&self) -> HashMap<MachineId, String> {
        self.peers
            .values()
            .filter_map(|peer| {
                peer.current_endpoint
                    .as_ref()
                    .map(|endpoint| (peer.machine_id.clone(), endpoint.clone()))
            })
            .collect()
    }

    fn refresh_candidates(
        &mut self,
        peer_map: &PeerStateMap,
        local_machine_id: &MachineId,
        device_peers: &[DevicePeer],
        now: Instant,
    ) {
        let device_endpoint_by_key = device_endpoint_by_key(device_peers);
        let mut seen = HashSet::new();

        for peer in peer_map.effective_peers(local_machine_id) {
            if peer.endpoints.is_empty() {
                self.peers.remove(&peer.id);
                continue;
            }
            seen.insert(peer.id.clone());
            let current_device_endpoint =
                device_endpoint_by_key
                    .get(&peer.public_key)
                    .and_then(|endpoint| {
                        if peer.endpoints.iter().any(|candidate| candidate == endpoint) {
                            Some(endpoint.as_str())
                        } else {
                            None
                        }
                    });
            let observation = peer.to_observation();
            self.peers
                .entry(peer.id.clone())
                .and_modify(|runtime_peer| {
                    runtime_peer.refresh_candidates(&observation, current_device_endpoint, now);
                })
                .or_insert_with(|| {
                    RuntimePeerState::new(&observation, current_device_endpoint, now)
                });
        }

        self.peers.retain(|machine_id, _| seen.contains(machine_id));
    }

    fn update_from_device(&mut self, device_peers: &[DevicePeer]) {
        let device_peer_by_key: HashMap<PublicKey, &DevicePeer> = device_peers
            .iter()
            .map(|peer| (peer.public_key.clone(), peer))
            .collect();
        for peer in self.peers.values_mut() {
            peer.update_from_device(device_peer_by_key.get(&peer.public_key).copied());
        }
    }

    async fn rotate_down_endpoints(
        &mut self,
        network: &WireguardDriver,
        now: Instant,
        force_rotate: bool,
    ) {
        for peer in self.peers.values_mut() {
            let status = peer.status(now);
            if !force_rotate && status != RuntimePeerStatus::Down && peer.current_endpoint.is_some()
            {
                continue;
            }

            let Some(next_endpoint) = peer.next_endpoint() else {
                continue;
            };
            if peer.current_endpoint.as_deref() == Some(next_endpoint.as_str()) {
                continue;
            }

            match network
                .set_peer_endpoint(&peer.public_key, &next_endpoint)
                .await
            {
                Ok(()) => {
                    debug!(
                        machine_id = %peer.machine_id,
                        endpoint = %next_endpoint,
                        "rotated wireguard peer endpoint"
                    );
                    peer.current_endpoint = Some(next_endpoint);
                    peer.last_endpoint_change_at = Some(now);
                }
                Err(error) => {
                    warn!(?error, machine_id = %peer.machine_id, "failed to rotate peer endpoint");
                }
            }
        }
    }
}

pub(crate) fn build_initial_endpoint_selections(
    snapshot: &[MachineMembership],
    bootstrap_peers: &[MachineObservation],
    local_machine_id: &MachineId,
    device_peers: &[DevicePeer],
    now: Instant,
) -> HashMap<MachineId, String> {
    RuntimePeerMap::from_records(
        snapshot,
        bootstrap_peers,
        local_machine_id,
        device_peers,
        now,
    )
    .selection_map()
}

pub(crate) struct EndpointMaintainerTask {
    pub(crate) snapshot: Vec<MachineMembership>,
    pub(crate) events: mpsc::Receiver<PloyzResult<MachineEvent>>,
    pub(crate) commands: mpsc::Receiver<EndpointMaintainerCommand>,
    pub(crate) bootstrap_peers: Vec<MachineObservation>,
    pub(crate) network: WireguardDriver,
    pub(crate) local_machine_id: MachineId,
    pub(crate) endpoint_selections: EndpointSelectionMap,
    pub(crate) initial_device_peers: Vec<DevicePeer>,
    pub(crate) cancel: CancellationToken,
}

// This task is intentionally periodic, but it is observing/maintaining
// transport state, not re-running control-plane reconciliation. The candidate
// endpoint list still comes from durable machine records; this loop only checks
// external WireGuard handshake/device state and chooses which already-declared
// endpoint to try right now.
pub(crate) async fn run_endpoint_maintainer_task(task: EndpointMaintainerTask) {
    let EndpointMaintainerTask {
        snapshot,
        mut events,
        mut commands,
        bootstrap_peers,
        network,
        local_machine_id,
        endpoint_selections,
        initial_device_peers,
        cancel,
    } = task;
    let mut peer_map = PeerStateMap::new();
    let now = Instant::now();
    peer_map.init_from_snapshot(&snapshot, now);
    for observation in &bootstrap_peers {
        if observation.id() != &local_machine_id {
            peer_map.upsert_transient(observation, now);
        }
    }
    let mut runtime = RuntimePeerMap::from_records(
        &snapshot,
        &bootstrap_peers,
        &local_machine_id,
        &initial_device_peers,
        now,
    );
    publish_endpoint_selections(&endpoint_selections, &runtime).await;
    let mut interval = tokio::time::interval(ENDPOINT_MAINTENANCE_INTERVAL);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("endpoint maintainer task cancelled");
                break;
            }
            _ = interval.tick() => {
                run_endpoint_maintenance_pass(
                    &mut runtime,
                    &network,
                    &endpoint_selections,
                    false,
                )
                .await;
            }
            event = events.recv() => {
                let Some(event) = event else {
                    warn!("endpoint maintainer machine subscription closed");
                    break;
                };
                let event = match event {
                    Ok(event) => event,
                    Err(error) => {
                        warn!(%error, "endpoint maintainer machine subscription failed");
                        break;
                    }
                };
                peer_map.apply_event(&event, Instant::now());
                let device_peers = network.read_peers().await.unwrap_or_default();
                runtime.refresh_candidates(&peer_map, &local_machine_id, &device_peers, Instant::now());
                publish_endpoint_selections(&endpoint_selections, &runtime).await;
            }
            Some(command) = commands.recv() => {
                match command {
                    EndpointMaintainerCommand::TickNow { force_rotate, complete } => {
                        run_endpoint_maintenance_pass(
                            &mut runtime,
                            &network,
                            &endpoint_selections,
                            force_rotate,
                        )
                        .await;
                        let _ = complete.send(());
                    }
                }
            }
        }
    }
}

async fn run_endpoint_maintenance_pass(
    runtime: &mut RuntimePeerMap,
    network: &WireguardDriver,
    endpoint_selections: &EndpointSelectionMap,
    force_rotate: bool,
) {
    let Ok(device_peers) = network.read_peers().await else {
        return;
    };
    // This pass is polling external device state because the transport does
    // not emit the higher-level event we need. It does not recalculate
    // placement, participation, status, or any other cluster policy.
    runtime.update_from_device(&device_peers);
    runtime
        .rotate_down_endpoints(network, Instant::now(), force_rotate)
        .await;
    publish_endpoint_selections(endpoint_selections, runtime).await;
}

async fn publish_endpoint_selections(
    endpoint_selections: &EndpointSelectionMap,
    runtime: &RuntimePeerMap,
) {
    *endpoint_selections.write().await = runtime.selection_map();
}

fn device_endpoint_by_key(device_peers: &[DevicePeer]) -> HashMap<PublicKey, String> {
    device_peers
        .iter()
        .filter_map(|peer| {
            peer.endpoint
                .as_ref()
                .map(|endpoint| (peer.public_key.clone(), endpoint.clone()))
        })
        .collect()
}

fn select_current_endpoint(
    all_endpoints: &[String],
    current_endpoint: Option<&str>,
) -> Option<String> {
    if let Some(current_endpoint) = current_endpoint
        && all_endpoints
            .iter()
            .any(|endpoint| endpoint == current_endpoint)
    {
        return Some(current_endpoint.to_string());
    }

    all_endpoints.first().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MachineLifecycle, MachineTopology, OverlayIp};
    use std::collections::BTreeMap;
    use std::net::Ipv6Addr;

    fn test_record(id: &str, key: [u8; 32], endpoints: &[&str]) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.into()),
            public_key: PublicKey(key),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            topology: MachineTopology::local(),
            subnet: None,
            bridge_ip: None,
            endpoints: endpoints
                .iter()
                .map(|endpoint| endpoint.to_string())
                .collect(),
            lifecycle: MachineLifecycle::Standby,
            storage: true,
            storage_participation: crate::model::StorageParticipation::default_authority(),
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn initial_selection_prefers_device_endpoint_when_valid() {
        let snapshot = vec![test_record("peer", [2; 32], &["a:1", "b:2"])];
        let device_peers = vec![DevicePeer {
            public_key: PublicKey([2; 32]),
            endpoint: Some("b:2".into()),
            last_handshake: None,
        }];
        let selections = build_initial_endpoint_selections(
            &snapshot,
            &[],
            &MachineId("self".into()),
            &device_peers,
            Instant::now(),
        );
        assert_eq!(
            selections
                .get(&MachineId("peer".into()))
                .map(String::as_str),
            Some("b:2")
        );
    }

    #[test]
    fn initial_selection_falls_back_to_first_candidate() {
        let snapshot = vec![test_record("peer", [2; 32], &["a:1", "b:2"])];
        let selections = build_initial_endpoint_selections(
            &snapshot,
            &[],
            &MachineId("self".into()),
            &[],
            Instant::now(),
        );
        assert_eq!(
            selections
                .get(&MachineId("peer".into()))
                .map(String::as_str),
            Some("a:1")
        );
    }

    #[tokio::test]
    async fn exits_when_machine_subscription_closes() {
        let network = crate::mesh::driver::WireguardDriver::memory_with(std::sync::Arc::new(
            crate::mesh::wireguard::MemoryWireGuard::new(),
        ));
        let (event_tx, event_rx) = mpsc::channel(4);
        let (_command_tx, command_rx) = mpsc::channel::<EndpointMaintainerCommand>(4);

        drop(event_tx);

        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            run_endpoint_maintainer_task(EndpointMaintainerTask {
                snapshot: Vec::new(),
                events: event_rx,
                commands: command_rx,
                bootstrap_peers: Vec::new(),
                network,
                local_machine_id: MachineId("self".into()),
                endpoint_selections: std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::new())),
                initial_device_peers: Vec::new(),
                cancel: CancellationToken::new(),
            }),
        )
        .await
        .expect("endpoint maintainer should exit when machine subscription closes");
    }
}
