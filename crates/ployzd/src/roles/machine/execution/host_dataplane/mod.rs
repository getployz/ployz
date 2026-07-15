//! Host WireGuard/eBPF readiness for machine-local dataplane preparation.

use ployz_core::dataplane::{
    DEFAULT_WIREGUARD_LISTEN_PORT, EbpfAttachmentStatus, EbpfForwardingReady,
    MachineDataplaneStatus, NativeDataplaneProjectionStatus, NetworkStatusMode,
    PloyzNativeMeshComponent, PloyzNativeMeshReady, WireGuardEbpfEndpointRoute,
    WireGuardEbpfPrepareError, WireGuardPeer, WireGuardPublicKey, WireGuardReady,
    WireGuardReadyEvidence,
};
use ployz_core::ids::MachineId;
use ployz_core::ops::FailureMessage;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::roles::machine::service::MachinePloyzNativeMeshPreparer;

mod host_commands;
mod host_network;
mod host_routes;
mod host_status;

#[cfg(test)]
use host_commands::HostCommandAction;
use host_commands::{HostCommandPlan, HostDataplaneEvidence, default_command_plans, unavailable};
pub use host_network::WireGuardMtuPolicy;
pub(crate) use host_network::resolve_wireguard_mtu;
use host_network::{ensure_private_key, ensure_wireguard_interface, public_key_from_private_key};
use host_routes::HostDataplaneRouteProgramming;
pub(crate) use host_status::dataplane_status_budget;

const HOST_DATAPLANE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_WIREGUARD_PRIVATE_KEY: &str = "/etc/ployz/wireguard.key";

/// Everything the host preparer needs to provision and verify the local
/// WireGuard/eBPF dataplane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PloyzNativeMeshHostConfig {
    pub machine_id: MachineId,
    pub ebpf_bytecode_path: PathBuf,
    pub ebpf_ctl_path: PathBuf,
    pub bridge_ifname: String,
    pub wg_ifname: String,
    pub private_key_path: PathBuf,
    pub listen_port: u16,
    pub mtu_policy: WireGuardMtuPolicy,
    pub ebpf_pin_path: Option<PathBuf>,
}

impl PloyzNativeMeshHostConfig {
    /// Production defaults for key material: the canonical on-host private
    /// key path and the default WireGuard listen port, with no pin override.
    #[must_use]
    pub fn with_default_key_material(
        machine_id: MachineId,
        ebpf_bytecode_path: PathBuf,
        ebpf_ctl_path: PathBuf,
        bridge_ifname: String,
        wg_ifname: String,
    ) -> Self {
        Self {
            machine_id,
            ebpf_bytecode_path,
            ebpf_ctl_path,
            bridge_ifname,
            wg_ifname,
            private_key_path: PathBuf::from(DEFAULT_WIREGUARD_PRIVATE_KEY),
            listen_port: DEFAULT_WIREGUARD_LISTEN_PORT,
            mtu_policy: WireGuardMtuPolicy::Auto,
            ebpf_pin_path: None,
        }
    }

    #[must_use]
    pub const fn with_mtu_policy(mut self, mtu_policy: WireGuardMtuPolicy) -> Self {
        self.mtu_policy = mtu_policy;
        self
    }
}

#[derive(Debug, Clone)]
pub struct PloyzNativeMeshPreparer {
    machine_id: MachineId,
    plans: Vec<HostCommandPlan>,
    route_programming: Option<HostDataplaneRouteProgramming>,
    public_key: HostWireGuardPublicKey,
    private_key_path: PathBuf,
    listen_port: u16,
    mtu_policy: WireGuardMtuPolicy,
    wg_ifname: String,
    endpoint_rotation: Arc<WireGuardEndpointRotation>,
    command_timeout: Duration,
}

impl PloyzNativeMeshPreparer {
    #[must_use]
    pub fn new(config: PloyzNativeMeshHostConfig) -> Self {
        let PloyzNativeMeshHostConfig {
            machine_id,
            ebpf_bytecode_path,
            ebpf_ctl_path,
            bridge_ifname,
            wg_ifname,
            private_key_path,
            listen_port,
            mtu_policy,
            ebpf_pin_path,
        } = config;
        let ebpf_ctl_program = ebpf_ctl_path.display().to_string();
        Self {
            machine_id,
            plans: default_command_plans(
                ebpf_bytecode_path,
                ebpf_ctl_path,
                bridge_ifname.clone(),
                wg_ifname.clone(),
                private_key_path.clone(),
                listen_port,
                ebpf_pin_path.clone(),
            ),
            route_programming: Some(HostDataplaneRouteProgramming {
                ebpf_ctl_program,
                bridge_ifname,
                wg_ifname: wg_ifname.clone(),
                ebpf_pin_path,
            }),
            endpoint_rotation: Arc::new(WireGuardEndpointRotation::new(wg_ifname.clone())),
            public_key: HostWireGuardPublicKey::LocalKey {
                private_key_path: private_key_path.clone(),
            },
            private_key_path,
            listen_port,
            mtu_policy,
            wg_ifname,
            command_timeout: HOST_DATAPLANE_COMMAND_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_command_plans(
        machine_id: MachineId,
        plans: impl IntoIterator<Item = HostCommandPlan>,
    ) -> Self {
        Self {
            machine_id,
            plans: plans.into_iter().collect(),
            route_programming: None,
            endpoint_rotation: Arc::new(WireGuardEndpointRotation::new(String::new())),
            public_key: HostWireGuardPublicKey::Static(
                WireGuardPublicKey::try_new("test-public-key").expect("test public key is valid"),
            ),
            private_key_path: PathBuf::from(DEFAULT_WIREGUARD_PRIVATE_KEY),
            listen_port: DEFAULT_WIREGUARD_LISTEN_PORT,
            mtu_policy: WireGuardMtuPolicy::Auto,
            wg_ifname: String::new(),
            command_timeout: HOST_DATAPLANE_COMMAND_TIMEOUT,
        }
    }

    #[must_use]
    pub const fn with_command_timeout(mut self, command_timeout: Duration) -> Self {
        self.command_timeout = command_timeout;
        self
    }
}

impl MachinePloyzNativeMeshPreparer for PloyzNativeMeshPreparer {
    async fn read_ployz_native_mesh_status(
        &self,
        mode: NetworkStatusMode,
    ) -> Result<MachineDataplaneStatus, String> {
        let wireguard =
            host_status::read_wireguard_status(&self.wg_ifname, self.mtu_policy, mode).await?;
        let ebpf_attachment = match &self.route_programming {
            Some(routes) => routes.attachment_status(self.command_timeout).await,
            None => EbpfAttachmentStatus::Unknown {
                message: "eBPF route programming is not configured".to_owned(),
            },
        };
        Ok(MachineDataplaneStatus {
            projection: NativeDataplaneProjectionStatus::unavailable(
                FailureMessage::try_new("dataplane projection has not been observed")
                    .expect("static failure message"),
            ),
            wireguard,
            ebpf_attachment,
        })
    }

    async fn read_wireguard_public_key(
        &self,
    ) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
        // Standalone reads happen before any prepare (e.g. the join
        // report), so the WireGuard interface must be provisioned first.
        self.public_key
            .provision_and_read(&self.machine_id, self.command_timeout)
            .await
    }

    async fn prepare_wireguard(
        &self,
        endpoint_routes: &[WireGuardEbpfEndpointRoute],
        peers: &[WireGuardPeer],
    ) -> Result<WireGuardReady, WireGuardEbpfPrepareError> {
        let mut wireguard = Vec::new();
        if !self.wg_ifname.is_empty() {
            let private_key = ensure_private_key(&self.private_key_path).map_err(|message| {
                unavailable(
                    &self.machine_id,
                    PloyzNativeMeshComponent::WireGuard,
                    message,
                )
            })?;
            let mtu = resolve_wireguard_mtu(self.mtu_policy, &self.wg_ifname).await;
            ensure_wireguard_interface(
                &self.wg_ifname,
                &private_key,
                self.listen_port,
                mtu,
                endpoint_routes,
                peers,
                &self.machine_id,
            )
            .await
            .map_err(|message| {
                unavailable(
                    &self.machine_id,
                    PloyzNativeMeshComponent::WireGuard,
                    message,
                )
            })?;
            wireguard.push(WireGuardReadyEvidence::HostPath {
                path: "/dev/net/tun".to_owned(),
            });
            wireguard.push(WireGuardReadyEvidence::HostPath {
                path: self.private_key_path.display().to_string(),
            });
        }
        for plan in self
            .plans
            .iter()
            .filter(|plan| plan.component() == PloyzNativeMeshComponent::WireGuard)
        {
            match plan.run(&self.machine_id, self.command_timeout).await? {
                HostDataplaneEvidence::WireGuard(evidence) => wireguard.push(evidence),
                HostDataplaneEvidence::EbpfForwarding(_) => {
                    unreachable!("wireguard plan returned eBPF evidence")
                }
            }
        }
        if let Some(route_programming) = &self.route_programming {
            for plan in route_programming.wireguard_plans_for(&self.machine_id, endpoint_routes)? {
                match plan.run(&self.machine_id, self.command_timeout).await? {
                    HostDataplaneEvidence::WireGuard(evidence) => wireguard.push(evidence),
                    HostDataplaneEvidence::EbpfForwarding(_) => {
                        unreachable!("wireguard route plan returned eBPF evidence")
                    }
                }
            }
        }
        self.endpoint_rotation.update_and_start(
            self.machine_id.clone(),
            peers,
            self.command_timeout,
        );
        // The plans above already provisioned the WireGuard interface, so
        // the public key only needs to be read here.
        let public_key = self
            .public_key
            .read_provisioned(&self.machine_id, self.command_timeout)
            .await?;
        if wireguard.is_empty() {
            return Err(unavailable(
                &self.machine_id,
                PloyzNativeMeshComponent::WireGuard,
                "wireguard readiness has no evidence".to_owned(),
            ));
        }

        Ok(WireGuardReady {
            public_key,
            evidence: wireguard,
        })
    }

    async fn prepare_ployz_native_mesh(
        &self,
        endpoint_routes: &[WireGuardEbpfEndpointRoute],
        peers: &[WireGuardPeer],
    ) -> Result<PloyzNativeMeshReady, WireGuardEbpfPrepareError> {
        let wireguard = self.prepare_wireguard(endpoint_routes, peers).await?;
        let mut ebpf_forwarding = Vec::new();
        for plan in self
            .plans
            .iter()
            .filter(|plan| plan.component() == PloyzNativeMeshComponent::EbpfForwarding)
        {
            match plan.run(&self.machine_id, self.command_timeout).await? {
                HostDataplaneEvidence::WireGuard(_) => {
                    unreachable!("eBPF plan returned WireGuard evidence")
                }
                HostDataplaneEvidence::EbpfForwarding(evidence) => ebpf_forwarding.push(evidence),
            }
        }
        if let Some(route_programming) = &self.route_programming {
            for plan in route_programming.ebpf_plans_for(&self.machine_id, endpoint_routes) {
                match plan.run(&self.machine_id, self.command_timeout).await? {
                    HostDataplaneEvidence::WireGuard(_) => {
                        unreachable!("eBPF route plan returned WireGuard evidence")
                    }
                    HostDataplaneEvidence::EbpfForwarding(evidence) => {
                        ebpf_forwarding.push(evidence);
                    }
                }
            }
            route_programming
                .verify_attachment(&self.machine_id, self.command_timeout)
                .await?;
        }
        if ebpf_forwarding.is_empty() {
            return Err(unavailable(
                &self.machine_id,
                PloyzNativeMeshComponent::EbpfForwarding,
                "eBPF forwarding readiness has no evidence".to_owned(),
            ));
        }

        Ok(PloyzNativeMeshReady {
            wireguard,
            ebpf_forwarding: EbpfForwardingReady {
                evidence: ebpf_forwarding,
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HostWireGuardPublicKey {
    #[cfg(test)]
    Static(WireGuardPublicKey),
    LocalKey {
        private_key_path: PathBuf,
    },
}

impl HostWireGuardPublicKey {
    /// Provisions the WireGuard interface, then reads its public key. Used
    /// where no prepare has run yet.
    async fn provision_and_read(
        &self,
        machine_id: &MachineId,
        _command_timeout: Duration,
    ) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
        match self {
            #[cfg(test)]
            Self::Static(public_key) => Ok(public_key.clone()),
            Self::LocalKey { private_key_path } => read_wireguard_public_key(private_key_path)
                .map_err(|message| {
                    unavailable(machine_id, PloyzNativeMeshComponent::WireGuard, message)
                }),
        }
    }

    /// Reads the public key from an interface the prepare plans already
    /// provisioned.
    async fn read_provisioned(
        &self,
        machine_id: &MachineId,
        _command_timeout: Duration,
    ) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
        match self {
            #[cfg(test)]
            Self::Static(public_key) => Ok(public_key.clone()),
            Self::LocalKey { private_key_path } => read_wireguard_public_key(private_key_path)
                .map_err(|message| {
                    unavailable(machine_id, PloyzNativeMeshComponent::WireGuard, message)
                }),
        }
    }
}

/// How often the rotation loop probes handshakes and applies any due rotation.
const ROTATION_TICK: Duration = Duration::from_secs(1);
/// A freshly-changed endpoint gets this long to complete a handshake before it is
/// eligible to rotate again — WireGuard retries handshakes roughly every 5s.
const ENDPOINT_SETTLE: Duration = Duration::from_secs(15);

#[derive(Debug)]
struct WireGuardEndpointRotation {
    wg_ifname: String,
    /// Set once when the rotation loop is first spawned; never cleared. The loop lives
    /// for the life of the mesh preparer and exits when the preparer is dropped (its
    /// `Weak` upgrade fails), so there is no stop/restart to coordinate.
    spawned: AtomicBool,
    peers: Mutex<std::collections::BTreeMap<String, RotatingWireGuardPeer>>,
}

#[derive(Debug, Clone)]
struct RotatingWireGuardPeer {
    endpoint_subnet: String,
    endpoints: Vec<std::net::SocketAddr>,
    active_endpoint: std::net::SocketAddr,
    last_endpoint_change: Instant,
}

impl WireGuardEndpointRotation {
    fn new(wg_ifname: String) -> Self {
        Self {
            wg_ifname,
            spawned: AtomicBool::new(false),
            peers: Mutex::new(std::collections::BTreeMap::new()),
        }
    }

    fn update_and_start(
        self: &Arc<Self>,
        machine_id: MachineId,
        peers: &[WireGuardPeer],
        command_timeout: Duration,
    ) {
        self.update_peers(machine_id, peers);

        if self.spawned.swap(true, Ordering::SeqCst) {
            return;
        }
        // A single tick loop for the life of the preparer. It holds a `Weak`, so when
        // the last preparer clone drops (process teardown) the next upgrade fails and
        // the loop exits — no shutdown channel, no leak. It idles when no peer has
        // multiple candidate endpoints.
        let rotation = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(ROTATION_TICK);
            loop {
                interval.tick().await;
                let Some(rotation) = rotation.upgrade() else {
                    break;
                };
                rotation.rotate_once(command_timeout).await;
            }
        });
    }

    /// Replace the tracked peer set from intent while preserving in-flight rotation
    /// progress: a peer already tracked keeps its `active_endpoint` and settle timer
    /// unless intent's candidate list no longer contains its active endpoint. Resetting
    /// every prepare would restart the settle timer faster than [`ENDPOINT_SETTLE`]
    /// under churn and rotation would never fire.
    fn update_peers(&self, machine_id: MachineId, peers: &[WireGuardPeer]) {
        let mut state = self
            .peers
            .lock()
            .expect("wireguard endpoint rotation lock is not poisoned");
        let mut present = std::collections::BTreeSet::new();
        for peer in peers.iter().filter(|peer| peer.machine_id != machine_id) {
            if peer.candidate_endpoints.len() < 2 {
                continue;
            }
            let key = peer.public_key.as_str().to_owned();
            present.insert(key.clone());
            match state.get_mut(&key) {
                Some(existing) if existing.endpoints == peer.candidate_endpoints => {}
                Some(existing) => {
                    existing.endpoints = peer.candidate_endpoints.clone();
                    if !existing.endpoints.contains(&existing.active_endpoint) {
                        existing.active_endpoint = peer.active_endpoint;
                        existing.last_endpoint_change = Instant::now();
                    }
                }
                None => {
                    state.insert(
                        key,
                        RotatingWireGuardPeer {
                            endpoint_subnet: peer.endpoint_subnet.clone(),
                            endpoints: peer.candidate_endpoints.clone(),
                            active_endpoint: peer.active_endpoint,
                            last_endpoint_change: Instant::now(),
                        },
                    );
                }
            }
        }
        state.retain(|key, _| present.contains(key));
    }

    async fn rotate_once(&self, _command_timeout: Duration) {
        let Some(handshakes) = read_latest_handshakes(&self.wg_ifname).await else {
            return;
        };
        for (public_key, endpoint, endpoint_subnet) in self.rotations_due(&handshakes) {
            let _ = host_network::configure_peer_endpoint(
                &self.wg_ifname,
                &public_key,
                endpoint,
                &endpoint_subnet,
            );
        }
    }

    fn rotations_due(
        &self,
        handshakes: &std::collections::BTreeMap<String, u64>,
    ) -> Vec<(String, std::net::SocketAddr, String)> {
        let mut state = self
            .peers
            .lock()
            .expect("wireguard endpoint rotation lock is not poisoned");
        let now_unix = current_unix_seconds();
        let mut rotations = Vec::new();
        for (public_key, peer) in state.iter_mut() {
            let last_handshake = handshakes.get(public_key).copied().unwrap_or_default();
            let after_change = peer.last_endpoint_change.elapsed() >= ENDPOINT_SETTLE;
            // Require the settle window even for an established-but-now-stale peer, so a
            // just-rotated endpoint gets a chance to handshake before we rotate again —
            // otherwise the loop cycles through every candidate each tick (thrash).
            let established_down = after_change
                && last_handshake != 0
                && now_unix.saturating_sub(last_handshake)
                    >= ployz_core::dataplane::MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS;
            let never_connected = last_handshake == 0 && after_change;
            if !established_down && !never_connected {
                continue;
            }
            let Some(index) = peer
                .endpoints
                .iter()
                .position(|endpoint| endpoint == &peer.active_endpoint)
            else {
                continue;
            };
            let Some(next) = peer
                .endpoints
                .get((index + 1) % peer.endpoints.len())
                .copied()
            else {
                continue;
            };
            peer.active_endpoint = next;
            peer.last_endpoint_change = Instant::now();
            rotations.push((public_key.clone(), next, peer.endpoint_subnet.clone()));
        }
        rotations
    }
}

async fn read_latest_handshakes(
    wg_ifname: &str,
) -> Option<std::collections::BTreeMap<String, u64>> {
    host_network::read_latest_handshakes(wg_ifname).ok()
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn read_wireguard_public_key(private_key_path: &Path) -> Result<WireGuardPublicKey, String> {
    let private_key = ensure_private_key(private_key_path)?;
    public_key_from_private_key(&private_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn host_preparer_rejects_empty_command_plan_set() {
        let preparer = PloyzNativeMeshPreparer::with_command_plans(
            machine_id("machine_a"),
            Vec::<HostCommandPlan>::new(),
        );

        let error = preparer
            .prepare_ployz_native_mesh(&[], &[])
            .await
            .expect_err("empty command plans fail");

        assert!(matches!(
            error,
            WireGuardEbpfPrepareError::Unavailable {
                machine_id,
                component: PloyzNativeMeshComponent::WireGuard,
                ..
            } if machine_id == self::machine_id("machine_a")
        ));
    }

    #[test]
    fn rotations_due_advances_a_never_connected_peer_after_the_settle_window() {
        let rotation = WireGuardEndpointRotation::new("wg-test".to_owned());
        let a: std::net::SocketAddr = "1.2.3.4:51820".parse().expect("addr");
        let b: std::net::SocketAddr = "5.6.7.8:51820".parse().expect("addr");
        rotation.peers.lock().expect("lock").insert(
            "peerkey".to_owned(),
            RotatingWireGuardPeer {
                endpoint_subnet: "10.42.2.0/24".to_owned(),
                endpoints: vec![a, b],
                active_endpoint: a,
                last_endpoint_change: Instant::now()
                    .checked_sub(ENDPOINT_SETTLE * 2)
                    .expect("past instant"),
            },
        );

        // No handshake yet and past the settle window → advance to the next candidate.
        let due = rotation.rotations_due(&std::collections::BTreeMap::new());
        assert_eq!(
            due,
            vec![("peerkey".to_owned(), b, "10.42.2.0/24".to_owned())]
        );
    }

    #[test]
    fn rotations_due_holds_a_freshly_rotated_peer_within_the_settle_window() {
        let rotation = WireGuardEndpointRotation::new("wg-test".to_owned());
        let a: std::net::SocketAddr = "1.2.3.4:51820".parse().expect("addr");
        let b: std::net::SocketAddr = "5.6.7.8:51820".parse().expect("addr");
        rotation.peers.lock().expect("lock").insert(
            "peerkey".to_owned(),
            RotatingWireGuardPeer {
                endpoint_subnet: "10.42.2.0/24".to_owned(),
                endpoints: vec![a, b],
                active_endpoint: a,
                last_endpoint_change: Instant::now(),
            },
        );

        // A just-changed endpoint is held even with no handshake — no per-tick thrash.
        assert!(
            rotation
                .rotations_due(&std::collections::BTreeMap::new())
                .is_empty()
        );
    }

    #[test]
    fn default_command_plans_ensure_ployz_tc_is_attached() {
        let plans = default_command_plans(
            "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".into(),
            "/usr/local/bin/ployz-ebpf-ctl".into(),
            "docker0".to_owned(),
            "ployz-wg0".to_owned(),
            "/etc/ployz/wireguard.key".into(),
            51820,
            None,
        );

        assert!(plans.iter().any(|plan| {
            matches!(
                &plan.action,
                HostCommandAction::CommandSucceeds {
                    component: PloyzNativeMeshComponent::EbpfForwarding,
                    program,
                    args,
                } if program == "/usr/local/bin/ployz-ebpf-ctl"
                    && args == &[
                        "ensure-attached",
                        "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
                        "docker0",
                        "ployz-wg0"
                    ]
            )
        }));
    }

    #[test]
    fn default_command_plans_ensure_bpffs_is_mounted() {
        let plans = default_command_plans(
            "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".into(),
            "/usr/local/bin/ployz-ebpf-ctl".into(),
            "docker0".to_owned(),
            "ployz-wg0".to_owned(),
            "/etc/ployz/wireguard.key".into(),
            51820,
            None,
        );

        assert!(plans.iter().any(|plan| {
            matches!(
                &plan.action,
                HostCommandAction::EnsureBpfFs { path } if path == std::path::Path::new("/sys/fs/bpf")
            )
        }));
    }

    #[tokio::test]
    async fn wireguard_prepare_skips_ebpf_requirements() {
        let preparer = PloyzNativeMeshPreparer::with_command_plans(
            machine_id("machine_a"),
            with_wireguard_evidence(HostCommandPlan::readiness_path(
                PloyzNativeMeshComponent::EbpfForwarding,
                "/definitely/missing",
            )),
        );

        let ready = preparer
            .prepare_wireguard(&[], &[])
            .await
            .expect("wireguard preparation ignores eBPF requirements");

        assert_eq!(ready.evidence.len(), 1);
    }

    #[tokio::test]
    async fn host_preparer_reports_missing_required_path() {
        let preparer = PloyzNativeMeshPreparer::with_command_plans(
            machine_id("machine_a"),
            with_wireguard_evidence(HostCommandPlan::readiness_path(
                PloyzNativeMeshComponent::EbpfForwarding,
                "/definitely/missing",
            )),
        );

        let error = preparer
            .prepare_ployz_native_mesh(&[], &[])
            .await
            .expect_err("missing path fails");

        assert!(matches!(
            error,
            WireGuardEbpfPrepareError::Unavailable {
                machine_id,
                component: PloyzNativeMeshComponent::EbpfForwarding,
                ..
            } if machine_id == self::machine_id("machine_a")
        ));
    }

    #[tokio::test]
    async fn host_preparer_times_out_hung_commands() {
        let preparer = PloyzNativeMeshPreparer::with_command_plans(
            machine_id("machine_a"),
            [HostCommandPlan::readiness_command(
                PloyzNativeMeshComponent::WireGuard,
                "sh",
                ["-c", "sleep 5"],
            )],
        )
        .with_command_timeout(Duration::from_millis(1));

        let error = preparer
            .prepare_ployz_native_mesh(&[], &[])
            .await
            .expect_err("hung command fails");

        assert!(matches!(
            error,
            WireGuardEbpfPrepareError::Unavailable {
                machine_id,
                component: PloyzNativeMeshComponent::WireGuard,
                ..
            } if machine_id == self::machine_id("machine_a")
        ));
    }

    #[tokio::test]
    async fn host_preparer_rejects_text_with_ployz_tc_symbol_names() {
        let path =
            std::env::temp_dir().join(format!("ployz-ebpf-bytecode-test-{}", std::process::id()));
        std::fs::write(&path, b"ployz_egress\0ployz_ingress\0ROUTES\0WG_IFINDEX\0")
            .expect("write test bytecode");
        let preparer = PloyzNativeMeshPreparer::with_command_plans(
            machine_id("machine_a"),
            with_wireguard_evidence(HostCommandPlan::readiness_ployz_tc_bytecode(&path)),
        );

        let error = preparer
            .prepare_ployz_native_mesh(&[], &[])
            .await
            .expect_err("text with symbols is not a BPF object");

        let _ = std::fs::remove_file(&path);
        assert!(matches!(
            error,
            WireGuardEbpfPrepareError::Unavailable {
                machine_id,
                component: PloyzNativeMeshComponent::EbpfForwarding,
                ..
            } if machine_id == self::machine_id("machine_a")
        ));
    }

    #[tokio::test]
    async fn host_preparer_rejects_non_ployz_tc_bytecode() {
        let path = std::env::temp_dir().join(format!(
            "ployz-ebpf-bad-bytecode-test-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"not the required bpf object").expect("write test bytecode");
        let preparer = PloyzNativeMeshPreparer::with_command_plans(
            machine_id("machine_a"),
            with_wireguard_evidence(HostCommandPlan::readiness_ployz_tc_bytecode(&path)),
        );

        let error = preparer
            .prepare_ployz_native_mesh(&[], &[])
            .await
            .expect_err("missing bytecode symbols fail");

        let _ = std::fs::remove_file(&path);
        assert!(matches!(
            error,
            WireGuardEbpfPrepareError::Unavailable {
                machine_id,
                component: PloyzNativeMeshComponent::EbpfForwarding,
                ..
            } if machine_id == self::machine_id("machine_a")
        ));
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    fn with_wireguard_evidence(plan: HostCommandPlan) -> [HostCommandPlan; 2] {
        [
            HostCommandPlan::readiness_path(PloyzNativeMeshComponent::WireGuard, "/dev/null"),
            plan,
        ]
    }
}
