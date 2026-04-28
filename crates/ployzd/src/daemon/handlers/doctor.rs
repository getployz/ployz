use crate::daemon::{ActiveMesh, DaemonState};
use crate::endpoint_maintenance::local_endpoint_watch_supported;
use ployz_api::{DaemonPayload, DoctorLocal, DoctorOverall, DoctorPayload, DoctorPeer};
use ployz_orchestrator::machine_policy::{DiagnosticRole, diagnostic_role};
use ployz_orchestrator::mesh::wireguard::DEFAULT_LISTEN_PORT;
use ployz_orchestrator::mesh::{DevicePeer, WireGuardDevice};
use ployz_orchestrator::network::endpoints::detect_advertised_endpoints;
use ployz_store_api::{
    MachineRegistry, PeerMembershipObservation, PeerMembershipState, PeerMembershipStore,
    PeerRttObservation, PeerRttStore,
};
use ployz_types::model::{MachineId, MachineRecord, PublicKey};
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;
use tokio::time::Instant;

use super::machine::render::format_lifecycle;

const PARTICIPATION_HANDSHAKE_FRESHNESS_WINDOW: Duration = Duration::from_secs(30);
impl DaemonState {
    pub(crate) async fn handle_doctor(&self) -> ployz_api::DaemonResponse {
        let Some(active) = self.active.as_ref() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };

        let machines = match active.mesh.store.list_machines().await {
            Ok(machines) => machines,
            Err(err) => return self.err("LIST_FAILED", format!("failed to list machines: {err}")),
        };

        let local_record = match machines
            .iter()
            .find(|machine| machine.id == self.identity.machine_id)
        {
            Some(record) => record,
            None => {
                return self.err(
                    "SELF_RECORD_MISSING",
                    format!(
                        "local machine '{}' is not published in the store",
                        self.identity.machine_id
                    ),
                );
            }
        };

        let device_peers = match active.mesh.network.read_peers().await {
            Ok(peers) => peers,
            Err(err) => {
                return self.err(
                    "WIREGUARD_READ_FAILED",
                    format!("failed to read local wireguard peers: {err}"),
                );
            }
        };
        let membership_observations = match active.mesh.store.peer_membership_observations().await {
            Ok(observations) => observations,
            Err(err) => {
                return self.err(
                    "PEER_MEMBERSHIP_READ_FAILED",
                    format!("failed to read Corrosion peer membership: {err}"),
                );
            }
        };
        let rtt_observations = match active.mesh.store.peer_rtt_observations().await {
            Ok(observations) => observations,
            Err(err) => {
                return self.err(
                    "PEER_RTT_READ_FAILED",
                    format!("failed to read Corrosion peer RTTs: {err}"),
                );
            }
        };
        let detected_local_endpoints = detect_advertised_endpoints(DEFAULT_LISTEN_PORT).await;
        let payload = build_doctor_payload(
            active,
            &machines,
            local_record,
            &device_peers,
            &membership_observations,
            &rtt_observations,
            &detected_local_endpoints,
            local_endpoint_watch_supported(),
        );

        self.ok_with_payload(
            render_doctor_report(&payload),
            Some(DaemonPayload::Doctor(payload)),
        )
    }
}

fn build_doctor_payload(
    active: &ActiveMesh,
    machines: &[MachineRecord],
    local_record: &MachineRecord,
    device_peers: &[DevicePeer],
    membership_observations: &[PeerMembershipObservation],
    rtt_observations: &[PeerRttObservation],
    detected_local_endpoints: &[String],
    endpoint_watch_supported: bool,
) -> DoctorPayload {
    let handshake_by_key = handshake_state_map(device_peers);
    let membership_by_ip = membership_state_map(membership_observations);
    let rtt_by_ip = rtt_state_map(rtt_observations);
    let peers = build_participation_rows(
        machines,
        &local_record.id,
        &handshake_by_key,
        &membership_by_ip,
        &rtt_by_ip,
    );
    DoctorPayload {
        overall: DoctorOverall {
            lifecycle: if peers.iter().any(|row| row.blocking) {
                String::from("blocked")
            } else {
                String::from("healthy")
            },
        },
        local: DoctorLocal {
            machine_id: local_record.id.0.clone(),
            network: active.config.name.0.clone(),
            network_lifecycle: active.config.lifecycle.to_string(),
            machine_lifecycle: format_lifecycle(local_record).to_string(),
            config_subnet: active.config.subnet.map(|subnet| subnet.to_string()),
            record_subnet: local_record.subnet.map(|subnet| subnet.to_string()),
            runtime_running: true,
            published_endpoints: local_record.endpoints.clone(),
            detected_endpoints: detected_local_endpoints.to_vec(),
            endpoint_watch_supported,
        },
        peers,
    }
}

fn render_doctor_report(report: &DoctorPayload) -> String {
    let blocking_peers: Vec<&DoctorPeer> = report.peers.iter().filter(|row| row.blocking).collect();
    let all_peers: Vec<&DoctorPeer> = report.peers.iter().collect();

    let mut lines = Vec::new();
    lines.push(format!("lifecycle: {}", report.overall.lifecycle));
    if !blocking_peers.is_empty() {
        lines.push(String::new());
        lines.push(String::from("blocking peers:"));
        append_peer_section(&mut lines, blocking_peers.as_slice(), false);
    }
    if !all_peers.is_empty() {
        lines.push(String::new());
        lines.push(String::from("all peers:"));
        append_peer_section(&mut lines, all_peers.as_slice(), true);
    }
    lines.push(String::new());
    lines.push(format!(
        "local: machine={} network={} network_lifecycle={} machine_lifecycle={} runtime_running={}",
        report.local.machine_id,
        report.local.network,
        report.local.network_lifecycle,
        report.local.machine_lifecycle,
        report.local.runtime_running,
    ));
    if report.local.config_subnet != report.local.record_subnet {
        lines.push(format!(
            "local subnet mismatch: config={:?} record={:?}",
            report.local.config_subnet, report.local.record_subnet
        ));
    }
    if report.local.published_endpoints != report.local.detected_endpoints {
        lines.push(format!(
            "local endpoint drift: published={:?} detected={:?}",
            report.local.published_endpoints, report.local.detected_endpoints
        ));
    }
    if !report.local.endpoint_watch_supported {
        lines.push(String::from(
            "local endpoint watch: unsupported, relying on periodic local endpoint audit",
        ));
    }

    lines.join("\n")
}

fn append_peer_section(lines: &mut Vec<String>, rows: &[&DoctorPeer], include_cause: bool) {
    let w_id = rows
        .iter()
        .map(|row| row.machine_id.len())
        .max()
        .unwrap_or(2)
        .max(2);
    let w_store = rows
        .iter()
        .map(|row| store_status_column(row).len())
        .max()
        .unwrap_or("store=active subnet=none".len())
        .max("store=active subnet=none".len());
    let w_wg = rows
        .iter()
        .map(|row| wg_status_column(row).len())
        .max()
        .unwrap_or("wg=fresh".len())
        .max("wg=fresh".len());
    let w_corrosion = rows
        .iter()
        .map(|row| corrosion_status_column(row).len())
        .max()
        .unwrap_or("corrosion=unknown".len())
        .max("corrosion=unknown".len());
    let w_rtt = rows
        .iter()
        .map(|row| rtt_status_column(row).len())
        .max()
        .unwrap_or("rtt=none".len())
        .max("rtt=none".len());
    for row in rows {
        let base = format!(
            "  {:<w_id$}  {:<w_store$}  {:<w_corrosion$}  {:<w_wg$}  {:<w_rtt$}",
            row.machine_id,
            store_status_column(row),
            corrosion_status_column(row),
            wg_status_column(row),
            rtt_status_column(row),
        );
        if include_cause {
            lines.push(base);
        } else {
            lines.push(format!("{base}  cause={}", row.cause_message));
        }
    }
}

fn store_status_column(row: &DoctorPeer) -> String {
    format!(
        "store={} subnet={}",
        row.store_lifecycle,
        row.subnet.as_deref().unwrap_or("none")
    )
}

fn wg_status_column(row: &DoctorPeer) -> String {
    format!("wg={}", row.wg_state)
}

fn corrosion_status_column(row: &DoctorPeer) -> String {
    format!("corrosion={}", row.corrosion_state)
}

fn rtt_status_column(row: &DoctorPeer) -> String {
    match (row.rtt_median_ms, row.rtt_stddev_ms) {
        (Some(median), Some(stddev)) => {
            format!(
                "rtt={}±{}",
                format_ms(median),
                format_ms_one_decimal(stddev)
            )
        }
        (Some(median), None) => format!("rtt={}", format_ms(median)),
        (None, Some(_)) | (None, None) => String::from("rtt=none"),
    }
}

fn diagnostic_role_name(role: DiagnosticRole) -> &'static str {
    match role {
        DiagnosticRole::Blocking => "blocking",
        DiagnosticRole::Informational => "informational",
    }
}

fn cause_parts(
    handshake: HandshakeState,
    corrosion: &CorrosionPeerState,
) -> (&'static str, &'static str) {
    match (corrosion.state, handshake) {
        (CorrosionState::Alive, _) => ("corrosion-alive", "corrosion reports peer alive"),
        (_, HandshakeState::Fresh) => (
            "fresh-wireguard-handshake",
            "wireguard has a recent peer handshake",
        ),
        (CorrosionState::Suspect, HandshakeState::Absent) => (
            "corrosion-suspect-no-direct-peer",
            "corrosion reports peer suspect and no direct peer is configured",
        ),
        (CorrosionState::Suspect, HandshakeState::None) => (
            "corrosion-suspect-no-handshake",
            "corrosion reports peer suspect and direct peer has no handshake yet",
        ),
        (CorrosionState::Suspect, HandshakeState::Stale) => (
            "corrosion-suspect-stale-handshake",
            "corrosion reports peer suspect and wireguard handshake is stale",
        ),
        (CorrosionState::Down, HandshakeState::Absent) => (
            "corrosion-down-no-direct-peer",
            "corrosion reports peer down and no direct peer is configured",
        ),
        (CorrosionState::Down, HandshakeState::None) => (
            "corrosion-down-no-handshake",
            "corrosion reports peer down and direct peer has no handshake yet",
        ),
        (CorrosionState::Down, HandshakeState::Stale) => (
            "corrosion-down-stale-handshake",
            "corrosion reports peer down and wireguard handshake is stale",
        ),
        (CorrosionState::Unknown, HandshakeState::Absent) => (
            "no-corrosion-membership-no-direct-peer",
            "no corrosion membership observation and no direct peer is configured",
        ),
        (CorrosionState::Unknown, HandshakeState::None) => (
            "no-corrosion-membership-no-handshake",
            "no corrosion membership observation and direct peer has no handshake yet",
        ),
        (CorrosionState::Unknown, HandshakeState::Stale) => (
            "no-corrosion-membership-stale-handshake",
            "no corrosion membership observation and wireguard handshake is stale",
        ),
    }
}

fn build_participation_rows(
    machines: &[MachineRecord],
    local_machine_id: &MachineId,
    handshake_by_key: &HashMap<PublicKey, HandshakeState>,
    membership_by_ip: &HashMap<IpAddr, CorrosionPeerState>,
    rtt_by_ip: &HashMap<IpAddr, RttState>,
) -> Vec<DoctorPeer> {
    let mut rows: Vec<DoctorPeer> = machines
        .iter()
        .filter_map(|machine| {
            let role = diagnostic_role(machine, local_machine_id)?;
            let handshake_state = handshake_by_key
                .get(&machine.public_key)
                .copied()
                .unwrap_or(HandshakeState::Absent);
            let corrosion = membership_by_ip
                .get(&IpAddr::V6(machine.overlay_ip.0))
                .cloned()
                .unwrap_or_default();
            let rtt = rtt_by_ip.get(&IpAddr::V6(machine.overlay_ip.0)).copied();
            let (cause_code, cause_message) = cause_parts(handshake_state, &corrosion);
            let healthy = corrosion.state == CorrosionState::Alive
                || handshake_state == HandshakeState::Fresh;
            Some(DoctorPeer {
                machine_id: machine.id.0.clone(),
                role: diagnostic_role_name(role).to_string(),
                blocking: role == DiagnosticRole::Blocking && !healthy,
                store_lifecycle: format_lifecycle(machine).to_string(),
                subnet: machine.subnet.map(|subnet| subnet.to_string()),
                wg_state: handshake_state.as_str().to_string(),
                probe_state: String::from("not-used"),
                corrosion_state: corrosion.state.as_str().to_string(),
                corrosion_actor_id: corrosion.actor_id,
                corrosion_timestamp: corrosion.timestamp,
                rtt_median_ms: rtt.map(|state| state.median_ms),
                rtt_stddev_ms: rtt.map(|state| state.stddev_ms),
                cause_code: cause_code.to_string(),
                cause_message: cause_message.to_string(),
            })
        })
        .collect();

    rows.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
    rows
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandshakeState {
    Fresh,
    Stale,
    None,
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CorrosionState {
    Alive,
    Suspect,
    Down,
    Unknown,
}

impl CorrosionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Alive => "alive",
            Self::Suspect => "suspect",
            Self::Down => "down",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct CorrosionPeerState {
    state: CorrosionState,
    actor_id: Option<String>,
    timestamp: Option<u64>,
}

impl Default for CorrosionState {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, Copy)]
struct RttState {
    median_ms: f64,
    stddev_ms: f64,
}

impl HandshakeState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::None => "none",
            Self::Absent => "absent",
        }
    }
}

fn handshake_state(now: Instant, last_handshake: Option<Instant>) -> HandshakeState {
    let Some(last_handshake) = last_handshake else {
        return HandshakeState::None;
    };
    match now.checked_duration_since(last_handshake) {
        Some(elapsed) if elapsed < PARTICIPATION_HANDSHAKE_FRESHNESS_WINDOW => {
            HandshakeState::Fresh
        }
        Some(_) => HandshakeState::Stale,
        None => HandshakeState::Fresh,
    }
}

fn handshake_state_map(device_peers: &[DevicePeer]) -> HashMap<PublicKey, HandshakeState> {
    let now = Instant::now();
    device_peers
        .iter()
        .map(|peer| {
            (
                peer.public_key.clone(),
                handshake_state(now, peer.last_handshake),
            )
        })
        .collect()
}

fn membership_state_map(
    observations: &[PeerMembershipObservation],
) -> HashMap<IpAddr, CorrosionPeerState> {
    observations
        .iter()
        .map(|observation| {
            (
                observation.addr.ip(),
                CorrosionPeerState {
                    state: match observation.state {
                        PeerMembershipState::Alive => CorrosionState::Alive,
                        PeerMembershipState::Suspect => CorrosionState::Suspect,
                        PeerMembershipState::Down => CorrosionState::Down,
                    },
                    actor_id: Some(observation.actor_id.clone()),
                    timestamp: Some(observation.timestamp),
                },
            )
        })
        .collect()
}

fn rtt_state_map(observations: &[PeerRttObservation]) -> HashMap<IpAddr, RttState> {
    observations
        .iter()
        .filter_map(|observation| {
            rtt_stats(observation.rtts_ms.as_slice()).map(|(median_ms, stddev_ms)| {
                (
                    observation.addr.ip(),
                    RttState {
                        median_ms,
                        stddev_ms,
                    },
                )
            })
        })
        .collect()
}

fn rtt_stats(samples: &[u64]) -> Option<(f64, f64)> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    let median = if sorted.len() % 2 == 0 {
        (sorted[mid - 1] as f64 + sorted[mid] as f64) / 2.0
    } else {
        sorted[mid] as f64
    };
    let mean = samples.iter().map(|sample| *sample as f64).sum::<f64>() / samples.len() as f64;
    let variance = samples
        .iter()
        .map(|sample| {
            let delta = *sample as f64 - mean;
            delta * delta
        })
        .sum::<f64>()
        / samples.len() as f64;
    Some((median, variance.sqrt()))
}

fn format_ms(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}ms")
    } else {
        format!("{value:.1}ms")
    }
}

fn format_ms_one_decimal(value: f64) -> String {
    format!("{value:.1}ms")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::ActiveMesh;
    use crate::mesh_state::network::NetworkConfig;
    use ployz_orchestrator::Mesh;
    use ployz_orchestrator::mesh::DevicePeer;
    use ployz_orchestrator::mesh::driver::WireguardDriver;
    use ployz_orchestrator::mesh::wireguard::MemoryWireGuard;
    use ployz_runtime_api::Identity;
    use ployz_store_api::StoreDriver;
    use ployz_store_api::memory::{MemoryService, MemoryStore};
    use ployz_store_api::{PeerMembershipObservation, PeerMembershipState, PeerRttObservation};
    use ployz_types::model::{
        MachineId, MachineLifecycle, MachineTopology, NetworkLifecycle, OverlayIp, PublicKey,
    };
    use std::net::{IpAddr, Ipv6Addr, SocketAddr};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn doctor_reports_missing_required_peer_handshake() {
        let (state, store, network) = make_state().await;
        let peer_key = PublicKey([2; 32]);
        let stale_key = PublicKey([3; 32]);

        store
            .upsert_self_machine(&test_machine_record(
                "peer",
                MachineLifecycle::Active,
                peer_key.clone(),
            ))
            .await
            .expect("upsert peer");
        store
            .upsert_self_machine(&test_machine_record(
                "stale-peer",
                MachineLifecycle::Active,
                stale_key.clone(),
            ))
            .await
            .expect("upsert stale peer");

        network.set_device_peers(vec![DevicePeer {
            public_key: stale_key,
            endpoint: Some(String::from("127.0.0.1:51820")),
            last_handshake: Some(Instant::now() - Duration::from_secs(31)),
        }]);

        let response = state.handle_doctor().await;
        assert!(response.ok, "{}", response.message);
        let Some(DaemonPayload::Doctor(payload)) = response.payload.as_ref() else {
            panic!("expected doctor payload");
        };
        assert_eq!(payload.overall.lifecycle, "blocked");
        assert!(payload.peers.iter().any(|peer| {
            peer.machine_id == "peer"
                && peer.blocking
                && peer.role == "blocking"
                && peer.store_lifecycle == "active"
                && peer.wg_state == "absent"
                && peer.corrosion_state == "unknown"
                && peer.cause_code == "no-corrosion-membership-no-direct-peer"
        }));
        assert!(payload.peers.iter().any(|peer| {
            peer.machine_id == "stale-peer"
                && peer.wg_state == "stale"
                && peer.corrosion_state == "unknown"
        }));
        assert!(response.message.contains("lifecycle: blocked"));
        assert!(response.message.contains("blocking peers:"));
        assert!(response.message.lines().any(|line| {
            line.contains("peer")
                && line.contains("store=active")
                && line.contains("corrosion=unknown")
                && line.contains("wg=absent")
                && line.contains("rtt=none")
                && line.contains(
                    "cause=no corrosion membership observation and no direct peer is configured",
                )
        }));
        assert!(response.message.contains("all peers:"));
        assert!(response.message.lines().any(|line| {
            line.contains("stale-peer")
                && line.contains("store=active")
                && line.contains("corrosion=unknown")
                && line.contains("wg=stale")
        }));
        assert!(!response.message.contains("probe="));
    }

    #[tokio::test]
    async fn doctor_reports_healthy_when_wireguard_handshake_is_fresh() {
        let (state, store, network) = make_state().await;
        let peer_key = PublicKey([2; 32]);

        store
            .upsert_self_machine(&test_machine_record(
                "peer",
                MachineLifecycle::Active,
                peer_key.clone(),
            ))
            .await
            .expect("upsert peer");

        network.set_device_peers(vec![DevicePeer {
            public_key: peer_key,
            endpoint: Some(String::from("127.0.0.1:51820")),
            last_handshake: Some(Instant::now()),
        }]);

        let response = state.handle_doctor().await;
        assert!(response.ok, "{}", response.message);
        let Some(DaemonPayload::Doctor(payload)) = response.payload.as_ref() else {
            panic!("expected doctor payload");
        };
        assert_eq!(payload.overall.lifecycle, "healthy");
        assert!(payload.peers.iter().any(|peer| {
            peer.machine_id == "peer"
                && !peer.blocking
                && peer.wg_state == "fresh"
                && peer.corrosion_state == "unknown"
                && peer.cause_code == "fresh-wireguard-handshake"
        }));
        assert!(response.message.contains("lifecycle: healthy"));
        assert!(!response.message.contains("blocking peers:"));
        assert!(response.message.contains("all peers:"));
        assert!(response.message.lines().any(|line| {
            line.contains("peer")
                && line.contains("store=active")
                && line.contains("corrosion=unknown")
                && line.contains("wg=fresh")
                && line.contains("rtt=none")
        }));
        assert!(!response.message.contains("probe="));
    }

    #[tokio::test]
    async fn doctor_projects_corrosion_membership_rtt_and_wireguard_state() {
        let local_record =
            test_machine_record("joiner5", MachineLifecycle::Standby, PublicKey([1; 32]));
        let mut peer_record =
            test_machine_record("peer", MachineLifecycle::Active, PublicKey([2; 32]));
        peer_record.overlay_ip = OverlayIp("fd00::2".parse().expect("valid overlay"));
        let machines = vec![local_record.clone(), peer_record.clone()];
        let peer_addr = SocketAddr::new(IpAddr::V6(peer_record.overlay_ip.0), 51001);
        let memberships = vec![PeerMembershipObservation {
            addr: peer_addr,
            actor_id: String::from("actor-2"),
            state: PeerMembershipState::Alive,
            timestamp: 123,
        }];
        let rtts = vec![PeerRttObservation {
            addr: peer_addr,
            rtts_ms: vec![120, 140, 160],
        }];
        let payload = build_doctor_payload(
            &test_active_mesh(),
            machines.as_slice(),
            &local_record,
            &[],
            memberships.as_slice(),
            rtts.as_slice(),
            &local_record.endpoints,
            true,
        );
        let report = render_doctor_report(&payload);

        assert_eq!(payload.overall.lifecycle, "healthy");
        assert!(payload.peers.iter().any(|peer| {
            peer.machine_id == "peer"
                && peer.corrosion_state == "alive"
                && peer.corrosion_actor_id.as_deref() == Some("actor-2")
                && peer.corrosion_timestamp == Some(123)
                && peer.rtt_median_ms == Some(140.0)
                && peer.cause_code == "corrosion-alive"
        }));
        assert!(report.contains("lifecycle: healthy"));
        assert!(report.contains("corrosion=alive"));
        assert!(report.contains("wg=absent"));
        assert!(report.contains("rtt=140ms±16.3ms"));
        assert!(!report.contains("probe="));
    }

    async fn make_state() -> (DaemonState, Arc<MemoryStore>, Arc<MemoryWireGuard>) {
        let identity = Identity::generate(MachineId(String::from("joiner5")), [1; 32]);
        let config = NetworkConfig::new(
            ployz_types::model::NetworkName(String::from("alpha")),
            &identity.public_key,
            "10.210.0.0/16",
            "10.210.3.0/24".parse().expect("valid subnet"),
        );
        let store = Arc::new(MemoryStore::new());
        let service = Arc::new(MemoryService::new());
        let network = Arc::new(MemoryWireGuard::new());

        store
            .upsert_self_machine(&test_machine_record(
                "joiner5",
                MachineLifecycle::Standby,
                identity.public_key.clone(),
            ))
            .await
            .expect("upsert self");

        let mesh = Mesh::new(
            WireguardDriver::memory_with(network.clone()),
            StoreDriver::memory_with(store.clone(), service),
            None,
            identity.machine_id.clone(),
            51820,
        );

        let mut state = DaemonState::new_for_tests(
            &unique_temp_dir("ployz-doctor-state"),
            identity,
            String::from("10.210.0.0/16"),
            24,
            4317,
            String::from("127.0.0.1:0"),
            None,
            1,
        );
        let cached_subnet = config.subnet;
        state.active = Some(ActiveMesh {
            config,
            cached_subnet,
            mesh,
            remote_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            peer_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            zfs_transfer: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            certificate_renewal: None,
        });

        (state, store, network)
    }

    fn test_machine_record(
        id: &str,
        lifecycle: MachineLifecycle,
        public_key: PublicKey,
    ) -> MachineRecord {
        MachineRecord {
            id: MachineId(String::from(id)),
            public_key,
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            topology: MachineTopology::local(),
            control_target: None,
            subnet: Some("10.210.0.0/24".parse().expect("valid subnet")),
            bridge_ip: None,
            endpoints: vec![String::from("127.0.0.1:51820")],
            lifecycle,
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        }
    }

    fn test_active_mesh() -> ActiveMesh {
        let identity = Identity::generate(MachineId(String::from("joiner5")), [1; 32]);
        let mut config = NetworkConfig::new(
            ployz_types::model::NetworkName(String::from("alpha")),
            &identity.public_key,
            "10.210.0.0/16",
            "10.210.3.0/24".parse().expect("valid subnet"),
        );
        config.lifecycle = NetworkLifecycle::Running;
        let store = Arc::new(MemoryStore::new());
        let service = Arc::new(MemoryService::new());
        let network = Arc::new(MemoryWireGuard::new());
        let mesh = Mesh::new(
            WireguardDriver::memory_with(network),
            StoreDriver::memory_with(store, service),
            None,
            identity.machine_id,
            51820,
        );
        let cached_subnet = config.subnet;

        ActiveMesh {
            config,
            cached_subnet,
            mesh,
            remote_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            peer_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            zfs_transfer: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            certificate_renewal: None,
        }
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }
}
