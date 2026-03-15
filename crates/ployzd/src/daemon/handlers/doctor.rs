use crate::daemon::{ActiveMesh, DaemonState};
use crate::machine_liveness::{MachineLiveness, machine_liveness};
use crate::mesh::{DevicePeer, WireGuardDevice};
use crate::model::{MachineRecord, OverlayIp, PublicKey};
use crate::store::MachineStore;
use crate::time::now_unix_secs;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Instant;

const PARTICIPATION_HANDSHAKE_FRESHNESS_WINDOW: Duration = Duration::from_secs(30);
const PARTICIPATION_PROBE_TIMEOUT: Duration = Duration::from_millis(750);
const PARTICIPATION_PROBE_PORT: u16 = 51821;
const PROBE_REQUEST: &[u8; 4] = b"PLZ?";
const PROBE_RESPONSE: &[u8; 4] = b"OK!!";

impl DaemonState {
    pub(crate) async fn handle_doctor(&self) -> ployz_sdk::transport::DaemonResponse {
        let Some(active) = self.active.as_ref() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };

        let machines = match active.mesh.store.list_machines().await {
            Ok(machines) => machines,
            Err(err) => return self.err("LIST_FAILED", format!("failed to list machines: {err}")),
        };

        let now = now_unix_secs();
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
        let overlay_probe_by_ip =
            probe_overlay_health(machines.as_slice(), &self.identity.machine_id).await;

        self.ok(render_doctor_report(
            active,
            &machines,
            local_record,
            &device_peers,
            &overlay_probe_by_ip,
            now,
        ))
    }
}

fn render_doctor_report(
    active: &ActiveMesh,
    machines: &[MachineRecord],
    local_record: &MachineRecord,
    device_peers: &[DevicePeer],
    overlay_probe_by_ip: &HashMap<OverlayIp, ProbeState>,
    now: u64,
) -> String {
    let handshake_by_key = handshake_state_map(device_peers);
    let peer_rows = build_participation_rows(
        machines,
        &local_record.id,
        &handshake_by_key,
        overlay_probe_by_ip,
        now,
    );
    let blocking_peers: Vec<&ParticipationRow> = peer_rows
        .iter()
        .filter(|row| row.required)
        .filter(|row| !row.probe_reachable())
        .collect();
    let all_peers: Vec<&ParticipationRow> = peer_rows.iter().collect();

    let mut lines = Vec::new();
    lines.push(format!(
        "participation: {}",
        if blocking_peers.is_empty() {
            "healthy"
        } else {
            "blocked"
        }
    ));
    if !blocking_peers.is_empty() {
        lines.push(String::new());
        lines.push(String::from("blocking peers:"));
        append_peer_section(&mut lines, &blocking_peers, false);
    }
    if !all_peers.is_empty() {
        lines.push(String::new());
        lines.push(String::from("all peers:"));
        append_peer_section(&mut lines, &all_peers, true);
    }
    lines.push(String::new());
    lines.push(format!(
        "local: machine={} network={} store={}/{} heartbeat={}",
        local_record.id,
        active.config.name,
        local_record.participation,
        format_liveness(local_record, now),
        format_heartbeat(local_record.last_heartbeat, now)
    ));

    lines.join("\n")
}

fn append_peer_section(lines: &mut Vec<String>, rows: &[&ParticipationRow], include_cause: bool) {
    let w_id = rows
        .iter()
        .map(|row| row.id.len())
        .max()
        .unwrap_or(2)
        .max(2);
    let w_store = rows
        .iter()
        .map(|row| row.store_status().len())
        .max()
        .unwrap_or("store=enabled/fresh".len())
        .max("store=enabled/fresh".len());
    let w_wg = rows
        .iter()
        .map(|row| row.wg_status().len())
        .max()
        .unwrap_or("wg=fresh".len())
        .max("wg=fresh".len());
    let w_probe = rows
        .iter()
        .map(|row| row.probe_status().len())
        .max()
        .unwrap_or("probe=unreachable".len())
        .max("probe=unreachable".len());
    for row in rows {
        let base = format!(
            "  {:<w_id$}  {:<w_store$}  {:<w_wg$}  {:<w_probe$}",
            row.id,
            row.store_status(),
            row.wg_status(),
            row.probe_status(),
        );
        if include_cause {
            lines.push(base);
        } else {
            lines.push(format!("{base}  cause={}", row.cause()));
        }
    }
}

#[derive(Debug, Clone)]
struct ParticipationRow {
    id: String,
    participation: String,
    liveness: String,
    required: bool,
    handshake: String,
    probe: ProbeState,
}

impl ParticipationRow {
    fn store_status(&self) -> String {
        format!("store={}/{}", self.participation, self.liveness)
    }

    fn wg_status(&self) -> String {
        format!("wg={}", self.handshake)
    }

    fn probe_status(&self) -> String {
        format!("probe={}", self.probe.as_str())
    }

    fn probe_reachable(&self) -> bool {
        self.probe == ProbeState::Reachable
    }

    fn cause(&self) -> &'static str {
        match (self.handshake.as_str(), self.probe) {
            (_, ProbeState::Reachable) => "healthy via overlay probe",
            ("absent", ProbeState::Unreachable) => {
                "no direct peer configured and overlay probe failed"
            }
            ("none", ProbeState::Unreachable) => {
                "direct peer exists but no handshake yet and overlay probe failed"
            }
            ("stale", ProbeState::Unreachable) => {
                "handshake older than 30s and overlay probe failed"
            }
            ("fresh", ProbeState::Unreachable) => "overlay probe failed despite recent handshake",
            _ => "unknown",
        }
    }
}

fn build_participation_rows(
    machines: &[MachineRecord],
    local_machine_id: &crate::model::MachineId,
    handshake_by_key: &HashMap<PublicKey, HandshakeState>,
    overlay_probe_by_ip: &HashMap<OverlayIp, ProbeState>,
    now: u64,
) -> Vec<ParticipationRow> {
    let mut rows: Vec<ParticipationRow> = machines
        .iter()
        .filter(|machine| machine.id != *local_machine_id)
        .map(|machine| {
            let required = machine_liveness(machine, now) == MachineLiveness::Fresh;
            let handshake_state = handshake_by_key
                .get(&machine.public_key)
                .cloned()
                .unwrap_or(HandshakeState::Absent);
            ParticipationRow {
                id: machine.id.0.clone(),
                participation: machine.participation.to_string(),
                liveness: format_liveness(machine, now).to_string(),
                required,
                handshake: String::from(handshake_state.as_str()),
                probe: overlay_probe_by_ip
                    .get(&machine.overlay_ip)
                    .copied()
                    .unwrap_or(ProbeState::Unreachable),
            }
        })
        .collect();

    rows.sort_by(|left, right| left.id.cmp(&right.id));
    rows
}

#[derive(Debug, Clone)]
enum HandshakeState {
    Fresh,
    Stale,
    None,
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeState {
    Reachable,
    Unreachable,
}

impl ProbeState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Reachable => "reachable",
            Self::Unreachable => "unreachable",
        }
    }
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

fn handshake_state_map(device_peers: &[DevicePeer]) -> HashMap<PublicKey, HandshakeState> {
    let now = Instant::now();
    device_peers
        .iter()
        .map(|peer| {
            let state = match peer.last_handshake {
                Some(last_handshake) => match now.checked_duration_since(last_handshake) {
                    Some(elapsed) if elapsed < PARTICIPATION_HANDSHAKE_FRESHNESS_WINDOW => {
                        HandshakeState::Fresh
                    }
                    Some(_) => HandshakeState::Stale,
                    None => HandshakeState::Fresh,
                },
                None => HandshakeState::None,
            };
            (peer.public_key.clone(), state)
        })
        .collect()
}

async fn probe_overlay_health(
    machines: &[MachineRecord],
    local_machine_id: &crate::model::MachineId,
) -> HashMap<OverlayIp, ProbeState> {
    let overlay_ips = machines
        .iter()
        .filter(|machine| machine.id != *local_machine_id)
        .map(|machine| machine.overlay_ip)
        .collect::<HashSet<_>>();
    let mut results = HashMap::with_capacity(overlay_ips.len());

    for overlay_ip in overlay_ips {
        let state = match probe_overlay_ip(overlay_ip).await {
            Some(()) => ProbeState::Reachable,
            None => ProbeState::Unreachable,
        };
        let _ = results.insert(overlay_ip, state);
    }

    results
}

async fn probe_overlay_ip(overlay_ip: OverlayIp) -> Option<()> {
    let mut stream = tokio::time::timeout(
        PARTICIPATION_PROBE_TIMEOUT,
        TcpStream::connect(SocketAddr::new(
            IpAddr::from(overlay_ip.0),
            PARTICIPATION_PROBE_PORT,
        )),
    )
    .await
    .ok()?
    .ok()?;

    tokio::time::timeout(PARTICIPATION_PROBE_TIMEOUT, async {
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

    Some(())
}

fn format_liveness(machine: &MachineRecord, now: u64) -> &'static str {
    match machine_liveness(machine, now) {
        MachineLiveness::Fresh => "fresh",
        MachineLiveness::Stale => "stale",
        MachineLiveness::Down => "down",
    }
}

fn format_heartbeat(ts: u64, now: u64) -> String {
    if ts == 0 {
        return String::from("never");
    }
    let ago = now.saturating_sub(ts);
    if ago < 60 {
        format!("{ago}s ago")
    } else if ago < 3600 {
        format!("{}m ago", ago / 60)
    } else if ago < 86_400 {
        format!("{}h ago", ago / 3600)
    } else {
        format!("{}d ago", ago / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::ActiveMesh;
    use crate::deploy::remote::RemoteControlHandle;
    use crate::mesh::DevicePeer;
    use crate::mesh::driver::WireguardDriver;
    use crate::mesh::wireguard::MemoryWireGuard;
    use crate::model::{MachineId, MachineStatus, OverlayIp, PublicKey};
    use crate::node::identity::Identity;
    use crate::store::backends::memory::{MemoryService, MemoryStore};
    use crate::store::driver::StoreDriver;
    use crate::store::network::NetworkConfig;
    use ployz_sdk::Participation;
    use std::net::Ipv6Addr;
    use std::path::PathBuf;
    use std::sync::{Arc, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::{Mutex, MutexGuard};
    use tokio::task::JoinHandle;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn doctor_reports_missing_required_peer_handshake() {
        let _probe_guard = lock_test_probe_port().await;
        let (state, store, network) = make_state().await;
        let now = now_unix_secs();
        let peer_key = PublicKey([2; 32]);
        let stale_key = PublicKey([3; 32]);

        store
            .upsert_self_machine(&test_machine_record(
                "peer",
                Participation::Enabled,
                now,
                peer_key.clone(),
            ))
            .await
            .expect("upsert peer");
        store
            .upsert_self_machine(&test_machine_record(
                "stale-peer",
                Participation::Enabled,
                now.saturating_sub(31),
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
        assert!(response.message.contains("participation: blocked"));
        assert!(response.message.contains("blocking peers:"));
        assert!(response.message.lines().any(|line| {
            line.contains("peer")
                && line.contains("store=enabled/fresh")
                && line.contains("wg=absent")
                && line.contains("probe=unreachable")
                && line.contains("cause=no direct peer configured and overlay probe failed")
        }));
        assert!(response.message.contains("all peers:"));
        assert!(response.message.lines().any(|line| {
            line.contains("stale-peer")
                && line.contains("store=enabled/stale")
                && line.contains("wg=stale")
                && line.contains("probe=unreachable")
        }));
    }

    #[tokio::test]
    async fn doctor_reports_healthy_when_overlay_probe_is_reachable() {
        let (_probe_guard, probe_cancel, probe_task) = start_test_probe_listener().await;
        let (state, store, network) = make_state().await;
        let now = now_unix_secs();
        let peer_key = PublicKey([2; 32]);

        store
            .upsert_self_machine(&test_machine_record(
                "peer",
                Participation::Enabled,
                now,
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
        assert!(response.message.contains("participation: healthy"));
        assert!(!response.message.contains("blocking peers:"));
        assert!(response.message.contains("all peers:"));
        assert!(response.message.lines().any(|line| {
            line.contains("peer")
                && line.contains("store=enabled/fresh")
                && line.contains("wg=fresh")
                && line.contains("probe=reachable")
        }));
        stop_test_probe_listener(probe_cancel, probe_task).await;
    }

    #[tokio::test]
    async fn doctor_treats_overlay_probe_as_second_health_signal() {
        let now = now_unix_secs();
        let local_record =
            test_machine_record("joiner5", Participation::Disabled, now, PublicKey([1; 32]));
        let peer_record =
            test_machine_record("peer", Participation::Enabled, now, PublicKey([2; 32]));
        let machines = vec![local_record.clone(), peer_record.clone()];
        let overlay_probe_by_ip = HashMap::from([(peer_record.overlay_ip, ProbeState::Reachable)]);
        let report = render_doctor_report(
            &test_active_mesh(),
            machines.as_slice(),
            &local_record,
            &[],
            &overlay_probe_by_ip,
            now,
        );

        assert!(report.contains("participation: healthy"));
        assert!(report.contains("wg=absent"));
        assert!(report.contains("probe=reachable"));
    }

    async fn make_state() -> (DaemonState, Arc<MemoryStore>, Arc<MemoryWireGuard>) {
        let identity = Identity::generate(MachineId(String::from("joiner5")), [1; 32]);
        let config = NetworkConfig::new(
            crate::model::NetworkName(String::from("alpha")),
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
                Participation::Disabled,
                now_unix_secs(),
                identity.public_key.clone(),
            ))
            .await
            .expect("upsert self");

        let mesh = crate::Mesh::new(
            WireguardDriver::Memory(network.clone()),
            StoreDriver::Memory {
                store: store.clone(),
                service,
            },
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
            1,
        );
        state.active = Some(ActiveMesh {
            config,
            mesh,
            remote_control: RemoteControlHandle::noop(),
            gateway: crate::services::gateway::GatewayHandle::noop(),
            dns: crate::services::dns::DnsHandle::noop(),
        });

        (state, store, network)
    }

    fn test_machine_record(
        id: &str,
        participation: Participation,
        last_heartbeat: u64,
        public_key: PublicKey,
    ) -> MachineRecord {
        MachineRecord {
            id: MachineId(String::from(id)),
            public_key,
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            subnet: Some("10.210.0.0/24".parse().expect("valid subnet")),
            bridge_ip: None,
            endpoints: vec![String::from("127.0.0.1:51820")],
            status: MachineStatus::Up,
            participation,
            last_heartbeat,
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        }
    }

    fn test_active_mesh() -> ActiveMesh {
        let identity = Identity::generate(MachineId(String::from("joiner5")), [1; 32]);
        let config = NetworkConfig::new(
            crate::model::NetworkName(String::from("alpha")),
            &identity.public_key,
            "10.210.0.0/16",
            "10.210.3.0/24".parse().expect("valid subnet"),
        );
        let store = Arc::new(MemoryStore::new());
        let service = Arc::new(MemoryService::new());
        let network = Arc::new(MemoryWireGuard::new());
        let mesh = crate::Mesh::new(
            WireguardDriver::Memory(network),
            StoreDriver::Memory { store, service },
            None,
            identity.machine_id,
            51820,
        );

        ActiveMesh {
            config,
            mesh,
            remote_control: RemoteControlHandle::noop(),
            gateway: crate::services::gateway::GatewayHandle::noop(),
            dns: crate::services::dns::DnsHandle::noop(),
        }
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }

    async fn start_test_probe_listener()
    -> (MutexGuard<'static, ()>, CancellationToken, JoinHandle<()>) {
        let probe_guard = lock_test_probe_port().await;
        let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, PARTICIPATION_PROBE_PORT))
            .await
            .expect("bind probe listener");
        let probe_cancel = CancellationToken::new();
        let task_cancel = probe_cancel.clone();
        let probe_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else {
                            continue;
                        };
                        tokio::spawn(async move {
                            let mut request = [0_u8; PROBE_REQUEST.len()];
                            if stream.read_exact(&mut request).await.is_err() {
                                return;
                            }
                            if request != *PROBE_REQUEST {
                                return;
                            }
                            let _ = stream.write_all(PROBE_RESPONSE).await;
                            let _ = stream.flush().await;
                        });
                    }
                }
            }
        });
        (probe_guard, probe_cancel, probe_task)
    }

    async fn lock_test_probe_port() -> MutexGuard<'static, ()> {
        static PROBE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        PROBE_LOCK.get_or_init(|| Mutex::new(())).lock().await
    }

    async fn stop_test_probe_listener(probe_cancel: CancellationToken, probe_task: JoinHandle<()>) {
        probe_cancel.cancel();
        let _ = probe_task.await;
    }
}
