use super::network::NetworkConfig;
use ployz_orchestrator::network::endpoints::detect_advertised_endpoints;
use ployz_runtime_api::Identity;
use ployz_store_api::{MachineRegistry, StoreDriver};
use ployz_types::model::{
    MachineEvent, MachineId, MachineMembership, MachineRole, MachineTopology, OverlayIp, PublicKey,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use base64::Engine as _;

const BOOTSTRAP_PEERS_FILE: &str = "bootstrap-peers.json";
const SEED_CACHE_DEBOUNCE: Duration = Duration::from_millis(250);

static SEED_CACHE_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapPeerRecord {
    pub machine_id: MachineId,
    pub public_key: PublicKey,
    pub overlay_ip: OverlayIp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnet: Option<ipnet::Ipv4Net>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_ip: Option<OverlayIp>,
    pub role: MachineRole,
    pub endpoints: Vec<String>,
}

impl BootstrapPeerRecord {
    #[must_use]
    pub fn into_machine_record(self) -> MachineMembership {
        let mut record = MachineMembership::seed(
            self.machine_id,
            self.public_key,
            self.overlay_ip,
            self.subnet,
            self.endpoints,
        );
        record.bridge_ip = self.bridge_ip;
        record.role = self.role;
        record
    }

    #[must_use]
    pub fn from_machine_record(record: &MachineMembership) -> Self {
        Self {
            machine_id: record.id.clone(),
            public_key: record.public_key.clone(),
            overlay_ip: record.overlay_ip,
            subnet: record.subnet,
            bridge_ip: record.bridge_ip,
            role: record.role,
            endpoints: record.endpoints.clone(),
        }
    }

    #[must_use]
    #[cfg(test)]
    pub fn from_invite(invite: &super::invite::InviteToken) -> Option<Self> {
        let overlay_str = invite.issuer_overlay_ip.as_deref()?;
        let public_key_b64 = invite.issuer_wg_public_key.as_deref()?;
        if invite.issuer_endpoints.is_empty() {
            return None;
        }

        let key_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(public_key_b64)
            .ok()?;
        let public_key: [u8; 32] = key_bytes.as_slice().try_into().ok()?;
        let overlay_ip = overlay_str.parse().ok()?;

        Some(Self {
            machine_id: invite.issuer_machine_id.clone(),
            public_key: PublicKey(public_key),
            overlay_ip: OverlayIp(overlay_ip),
            subnet: None,
            bridge_ip: None,
            role: MachineRole::StorageCandidate,
            endpoints: invite.issuer_endpoints.clone(),
        })
    }
}

#[must_use = "dropping the handle leaves the seed cache task running detached; call shutdown to cancel and observe join errors"]
pub struct BootstrapSeedCacheTask {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

impl BootstrapSeedCacheTask {
    pub fn spawn(
        network_dir: std::path::PathBuf,
        store: StoreDriver,
        local_machine_id: MachineId,
    ) -> Self {
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(run_bootstrap_seed_cache_task(
            network_dir,
            store,
            local_machine_id,
            cancel.clone(),
        ));
        Self { cancel, handle }
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        if let Err(error) = self.handle.await {
            tracing::warn!(?error, "bootstrap seed cache task join failed");
        }
    }
}

#[must_use]
pub fn bootstrap_peers_path(network_dir: &Path) -> std::path::PathBuf {
    network_dir.join(BOOTSTRAP_PEERS_FILE)
}

pub fn load_bootstrap_peer_records(network_dir: &Path) -> Result<Vec<BootstrapPeerRecord>, String> {
    let path = bootstrap_peers_path(network_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let data = std::fs::read_to_string(&path)
        .map_err(|error| format!("read bootstrap peers '{}': {error}", path.display()))?;
    serde_json::from_str(&data)
        .map_err(|error| format!("parse bootstrap peers '{}': {error}", path.display()))
}

pub fn write_bootstrap_peer_records(
    network_dir: &Path,
    peers: &[BootstrapPeerRecord],
) -> Result<(), String> {
    let path = bootstrap_peers_path(network_dir);
    std::fs::create_dir_all(network_dir).map_err(|error| {
        format!(
            "create bootstrap peer dir '{}': {error}",
            network_dir.display()
        )
    })?;

    let mut peers = peers.to_vec();
    peers.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
    peers.dedup_by(|left, right| left.machine_id == right.machine_id);

    let body = serde_json::to_string_pretty(&peers)
        .map_err(|error| format!("encode bootstrap peers '{}': {error}", path.display()))?;
    let seq = SEED_CACHE_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_path = path.with_file_name(format!(
        "{BOOTSTRAP_PEERS_FILE}.tmp.{}.{seq}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, body)
        .map_err(|error| format!("write bootstrap peers '{}': {error}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &path).map_err(|error| {
        let _ = std::fs::remove_file(&tmp_path);
        format!(
            "replace bootstrap peers '{}' with '{}': {error}",
            tmp_path.display(),
            path.display()
        )
    })
}

pub async fn refresh_bootstrap_peer_records_from_store(
    network_dir: &Path,
    store: &StoreDriver,
    local_machine_id: &MachineId,
) -> Result<(), String> {
    let machines = store
        .list_machines()
        .await
        .map_err(|error| format!("list machines for bootstrap seed cache: {error}"))?;
    let peers = remote_peer_records(machines.iter(), local_machine_id);
    write_bootstrap_peer_records(network_dir, &peers)
}

pub fn resolve_bootstrap_addrs(
    peers: &[BootstrapPeerRecord],
    local_machine_id: &MachineId,
    bootstrap_gossip_port: u16,
) -> Vec<String> {
    peers
        .iter()
        .filter(|peer| peer.machine_id != *local_machine_id)
        .filter(|peer| peer.role == MachineRole::StorageCandidate)
        .map(|peer| format!("[{}]:{}", peer.overlay_ip.0, bootstrap_gossip_port))
        .collect()
}

pub async fn build_seed_records(
    identity: &Identity,
    net_config: &NetworkConfig,
    self_control_target: Option<String>,
    listen_port: u16,
    bootstrap_peers: &[BootstrapPeerRecord],
    configured_topology: Option<&MachineTopology>,
) -> Vec<MachineMembership> {
    let mut seed_records: Vec<MachineMembership> = bootstrap_peers
        .iter()
        .filter(|peer| peer.machine_id != identity.machine_id)
        .cloned()
        .map(BootstrapPeerRecord::into_machine_record)
        .collect();

    let endpoints = detect_advertised_endpoints(listen_port).await;
    let mut self_record = MachineMembership::seed(
        identity.machine_id.clone(),
        identity.public_key.clone(),
        net_config.overlay_ip,
        net_config.subnet,
        endpoints,
    );
    self_record.role = net_config.machine_role;
    if let Some(topology) = configured_topology {
        self_record.topology = topology.clone();
    }
    if self_control_target.is_some() {
        self_record.control_target = self_control_target;
    }
    seed_records.push(self_record);

    seed_records
}

async fn run_bootstrap_seed_cache_task(
    network_dir: std::path::PathBuf,
    store: StoreDriver,
    local_machine_id: MachineId,
    cancel: CancellationToken,
) {
    let retry_delay = Duration::from_secs(5);
    loop {
        let subscription = tokio::select! {
            _ = cancel.cancelled() => return,
            subscription = store.subscribe_machines() => subscription,
        };

        let (snapshot, mut events) = match subscription {
            Ok(subscription) => subscription,
            Err(error) => {
                tracing::warn!(%error, "bootstrap seed cache machine subscription failed");
                if sleep_or_cancel(retry_delay, &cancel).await {
                    return;
                }
                continue;
            }
        };

        let mut machines: HashMap<MachineId, MachineMembership> = snapshot
            .into_iter()
            .map(|machine| (machine.id.clone(), machine))
            .collect();
        let mut last_written = load_bootstrap_peer_records(&network_dir).ok();
        sync_seed_cache(
            &network_dir,
            &machines,
            &local_machine_id,
            &mut last_written,
        );

        loop {
            let event = tokio::select! {
                _ = cancel.cancelled() => return,
                event = events.recv() => event,
            };
            let Some(event) = event else {
                tracing::warn!("bootstrap seed cache machine subscription ended");
                break;
            };
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    tracing::warn!(%error, "bootstrap seed cache machine subscription failed");
                    break;
                }
            };

            apply_machine_event(&mut machines, event);
            let mut subscription_ended = false;
            // The debounce timer is shared across the burst — a steady stream of
            // events still allows it to elapse, bounding write latency under churn.
            let debounce = tokio::time::sleep(SEED_CACHE_DEBOUNCE);
            tokio::pin!(debounce);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    event = events.recv() => {
                        let Some(event) = event else {
                            tracing::warn!("bootstrap seed cache machine subscription ended");
                            subscription_ended = true;
                            break;
                        };
                        let event = match event {
                            Ok(event) => event,
                            Err(error) => {
                                tracing::warn!(%error, "bootstrap seed cache machine subscription failed");
                                subscription_ended = true;
                                break;
                            }
                        };
                        apply_machine_event(&mut machines, event);
                    }
                    _ = &mut debounce => break,
                }
            }
            sync_seed_cache(
                &network_dir,
                &machines,
                &local_machine_id,
                &mut last_written,
            );
            if subscription_ended {
                break;
            }
        }

        if sleep_or_cancel(retry_delay, &cancel).await {
            return;
        }
    }
}

fn apply_machine_event(machines: &mut HashMap<MachineId, MachineMembership>, event: MachineEvent) {
    match event {
        MachineEvent::Added(machine) | MachineEvent::Updated(machine) => {
            machines.insert(machine.id.clone(), machine);
        }
        MachineEvent::Removed(machine) => {
            machines.remove(&machine.id);
        }
    }
}

async fn sleep_or_cancel(delay: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => true,
        _ = tokio::time::sleep(delay) => false,
    }
}

fn sync_seed_cache(
    network_dir: &Path,
    machines: &HashMap<MachineId, MachineMembership>,
    local_machine_id: &MachineId,
    last_written: &mut Option<Vec<BootstrapPeerRecord>>,
) {
    let peers = remote_peer_records(machines.values(), local_machine_id);
    if last_written.as_ref() == Some(&peers) {
        return;
    }
    match write_bootstrap_peer_records(network_dir, &peers) {
        Ok(()) => *last_written = Some(peers),
        Err(error) => tracing::warn!(%error, "failed to write bootstrap seed cache"),
    }
}

fn remote_peer_records<'a>(
    machines: impl Iterator<Item = &'a MachineMembership>,
    local_machine_id: &MachineId,
) -> Vec<BootstrapPeerRecord> {
    let mut peers = machines
        .filter(|machine| machine.id != *local_machine_id)
        .map(BootstrapPeerRecord::from_machine_record)
        .collect::<Vec<_>>();
    peers.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
    peers
}

#[cfg(test)]
mod tests {
    use super::super::invite::InviteToken;
    use super::super::network::{DEFAULT_CLUSTER_CIDR, NetworkConfig};
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use ployz_runtime_api::Identity;
    use ployz_store_api::{MachineRegistry, StoreDriver};
    use ployz_types::model::{MachineTopology, NetworkId, NetworkName};
    use std::time::{Duration, Instant};

    fn temp_network_dir(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ployz-bootstrap-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).expect("create temp bootstrap dir");
        root
    }

    fn machine_record(id: &str, overlay_ip: &str, endpoints: Vec<&str>) -> MachineMembership {
        let mut record = MachineMembership::seed(
            MachineId(id.into()),
            PublicKey([id.as_bytes().first().copied().unwrap_or(1); 32]),
            OverlayIp(overlay_ip.parse().expect("valid overlay")),
            Some("10.210.9.0/24".parse().expect("valid subnet")),
            endpoints.into_iter().map(String::from).collect(),
        );
        record.bridge_ip = Some(OverlayIp("fd00::99".parse().expect("valid bridge")));
        record
    }

    async fn wait_until_async(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if predicate() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn sample_invite() -> InviteToken {
        InviteToken {
            invite_id: "invite".into(),
            network_id: NetworkId("network".into()),
            network_name: "alpha".into(),
            issuer_machine_id: MachineId("founder".into()),
            issuer_verify_key: "verify".into(),
            expires_at: 1,
            issuer_endpoints: vec!["10.0.0.1:51820".into()],
            issuer_overlay_ip: Some("fd00::1".into()),
            issuer_wg_public_key: Some(URL_SAFE_NO_PAD.encode([7u8; 32])),
            bootstrap_peers: Vec::new(),
        }
    }

    #[test]
    fn bootstrap_peer_roundtrip_from_invite() {
        let network_dir = temp_network_dir("roundtrip");
        let invite = sample_invite();
        let peer = BootstrapPeerRecord::from_invite(&invite).expect("bootstrap peer");

        write_bootstrap_peer_records(&network_dir, std::slice::from_ref(&peer))
            .expect("persist bootstrap peer");
        let loaded = load_bootstrap_peer_records(&network_dir).expect("load bootstrap peers");

        assert_eq!(loaded, vec![peer]);
        let _ = std::fs::remove_dir_all(&network_dir);
    }

    #[test]
    fn missing_bootstrap_peer_file_loads_empty() {
        let network_dir = temp_network_dir("missing");

        let loaded = load_bootstrap_peer_records(&network_dir).expect("load missing cache");

        assert!(loaded.is_empty());
        let _ = std::fs::remove_dir_all(&network_dir);
    }

    #[test]
    fn bootstrap_peer_records_roundtrip_subnet_and_bridge_ip() {
        let network_dir = temp_network_dir("full-roundtrip");
        let record = BootstrapPeerRecord {
            machine_id: MachineId("peer".into()),
            public_key: PublicKey([5; 32]),
            overlay_ip: OverlayIp("fd00::5".parse().expect("valid overlay")),
            subnet: Some("10.210.5.0/24".parse().expect("valid subnet")),
            bridge_ip: Some(OverlayIp("fd00::55".parse().expect("valid bridge"))),
            role: MachineRole::Mirror,
            endpoints: vec!["peer:51820".into()],
        };

        write_bootstrap_peer_records(&network_dir, std::slice::from_ref(&record))
            .expect("write seed cache");
        let loaded = load_bootstrap_peer_records(&network_dir).expect("load seed cache");

        assert_eq!(loaded, vec![record]);
        let _ = std::fs::remove_dir_all(&network_dir);
    }

    #[test]
    fn resolve_bootstrap_addrs_filters_local_self() {
        let local = MachineId("local".into());
        let local_peer = BootstrapPeerRecord {
            machine_id: local.clone(),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp("fd00::1".parse().expect("valid overlay")),
            subnet: None,
            bridge_ip: None,
            role: MachineRole::StorageCandidate,
            endpoints: Vec::new(),
        };
        let remote_peer = BootstrapPeerRecord {
            machine_id: MachineId("remote".into()),
            public_key: PublicKey([2; 32]),
            overlay_ip: OverlayIp("fd00::2".parse().expect("valid overlay")),
            subnet: None,
            bridge_ip: None,
            role: MachineRole::StorageCandidate,
            endpoints: Vec::new(),
        };

        let addrs = resolve_bootstrap_addrs(&[local_peer, remote_peer], &local, 51001);

        assert_eq!(addrs, vec!["[fd00::2]:51001"]);
    }

    #[tokio::test]
    async fn build_seed_records_uses_bootstrap_peers_and_rebuilds_self() {
        let identity = Identity::generate(MachineId("joiner".into()), [3; 32]);
        let net_config = NetworkConfig::new(
            NetworkName("alpha".into()),
            &identity.public_key,
            DEFAULT_CLUSTER_CIDR,
            "10.210.1.0/24".parse().expect("valid subnet"),
        );
        let founder = BootstrapPeerRecord {
            machine_id: MachineId("founder".into()),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp("fd00::1".parse().expect("valid overlay")),
            subnet: Some("10.210.2.0/24".parse().expect("valid subnet")),
            bridge_ip: Some(OverlayIp("fd00::22".parse().expect("valid bridge"))),
            role: MachineRole::StorageCandidate,
            endpoints: vec!["bootstrap:51820".into()],
        };
        let seed_records = build_seed_records(
            &identity,
            &net_config,
            None,
            51820,
            std::slice::from_ref(&founder),
            None,
        )
        .await;

        assert!(
            seed_records
                .iter()
                .any(|machine| machine.id == founder.machine_id
                    && machine.public_key == founder.public_key
                    && machine.subnet == founder.subnet
                    && machine.bridge_ip == founder.bridge_ip)
        );
        assert!(
            seed_records
                .iter()
                .any(|machine| machine.id == identity.machine_id
                    && machine.public_key == identity.public_key
                    && machine.overlay_ip == net_config.overlay_ip
                    && machine.subnet == net_config.subnet)
        );
    }

    #[tokio::test]
    async fn build_seed_records_applies_self_control_target() {
        let identity = Identity::generate(MachineId("joiner".into()), [4; 32]);
        let net_config = NetworkConfig::new(
            NetworkName("alpha".into()),
            &identity.public_key,
            DEFAULT_CLUSTER_CIDR,
            "10.210.1.0/24".parse().expect("valid subnet"),
        );

        let seed_records = build_seed_records(
            &identity,
            &net_config,
            Some("self-target".into()),
            51820,
            &[],
            None,
        )
        .await;

        let self_record = seed_records
            .into_iter()
            .find(|machine| machine.id == identity.machine_id)
            .expect("self record");
        assert_eq!(self_record.control_target, Some("self-target".into()));
    }

    #[tokio::test]
    async fn build_seed_records_applies_configured_self_topology() {
        let identity = Identity::generate(MachineId("joiner".into()), [4; 32]);
        let net_config = NetworkConfig::new(
            NetworkName("alpha".into()),
            &identity.public_key,
            DEFAULT_CLUSTER_CIDR,
            "10.210.1.0/24".parse().expect("valid subnet"),
        );
        let configured_topology =
            MachineTopology::new("eu-primary", Some("hel1-a")).expect("valid topology");

        let seed_records = build_seed_records(
            &identity,
            &net_config,
            None,
            51820,
            &[],
            Some(&configured_topology),
        )
        .await;

        let self_record = seed_records
            .into_iter()
            .find(|machine| machine.id == identity.machine_id)
            .expect("self record");
        assert_eq!(self_record.topology, configured_topology);
    }

    #[tokio::test]
    async fn seed_cache_task_writes_initial_snapshot() {
        let network_dir = temp_network_dir("task-initial");
        let store = StoreDriver::memory();
        let local = machine_record("local", "fd00::1", vec!["local:51820"]);
        let peer = machine_record("peer", "fd00::2", vec!["peer:51820"]);
        store
            .upsert_self_machine(&local)
            .await
            .expect("insert local");
        store.upsert_self_machine(&peer).await.expect("insert peer");

        let task = BootstrapSeedCacheTask::spawn(network_dir.clone(), store, local.id.clone());
        let wrote_peer = wait_until_async(Duration::from_secs(2), || {
            load_bootstrap_peer_records(&network_dir)
                .map(|records| records.iter().any(|record| record.machine_id == peer.id))
                .unwrap_or(false)
        })
        .await;

        task.shutdown().await;
        assert!(wrote_peer);
        let _ = std::fs::remove_dir_all(&network_dir);
    }

    #[tokio::test]
    async fn seed_cache_task_removes_deleted_peer() {
        let network_dir = temp_network_dir("task-remove");
        let store = StoreDriver::memory();
        let local = machine_record("local", "fd00::1", vec!["local:51820"]);
        let peer = machine_record("peer", "fd00::2", vec!["peer:51820"]);
        store
            .upsert_self_machine(&local)
            .await
            .expect("insert local");
        store.upsert_self_machine(&peer).await.expect("insert peer");
        let task =
            BootstrapSeedCacheTask::spawn(network_dir.clone(), store.clone(), local.id.clone());
        let wrote_peer = wait_until_async(Duration::from_secs(2), || {
            load_bootstrap_peer_records(&network_dir)
                .map(|records| records.iter().any(|record| record.machine_id == peer.id))
                .unwrap_or(false)
        })
        .await;
        assert!(wrote_peer);

        store.delete_machine(&peer.id).await.expect("delete peer");
        let removed_peer = wait_until_async(Duration::from_secs(2), || {
            load_bootstrap_peer_records(&network_dir)
                .map(|records| records.iter().all(|record| record.machine_id != peer.id))
                .unwrap_or(false)
        })
        .await;

        task.shutdown().await;
        assert!(removed_peer);
        let _ = std::fs::remove_dir_all(&network_dir);
    }

    #[tokio::test]
    async fn refresh_bootstrap_peer_records_from_store_rewrites_remote_peers() {
        let network_dir = temp_network_dir("refresh");
        let store = StoreDriver::memory();
        let local = machine_record("local", "fd00::1", vec!["local:51820"]);
        let peer = machine_record("peer", "fd00::2", vec!["peer:51820"]);
        store
            .upsert_self_machine(&local)
            .await
            .expect("insert local");
        store.upsert_self_machine(&peer).await.expect("insert peer");

        refresh_bootstrap_peer_records_from_store(&network_dir, &store, &local.id)
            .await
            .expect("refresh cache");
        let records = load_bootstrap_peer_records(&network_dir).expect("load cache");
        assert_eq!(
            records
                .iter()
                .map(|record| record.machine_id.clone())
                .collect::<Vec<_>>(),
            vec![peer.id.clone()]
        );

        store.delete_machine(&peer.id).await.expect("delete peer");
        refresh_bootstrap_peer_records_from_store(&network_dir, &store, &local.id)
            .await
            .expect("refresh cache after delete");
        let records = load_bootstrap_peer_records(&network_dir).expect("load cache");
        assert!(records.is_empty());

        let _ = std::fs::remove_dir_all(&network_dir);
    }
}
