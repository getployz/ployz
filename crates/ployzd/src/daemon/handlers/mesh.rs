use crate::mesh_state::bootstrap::{
    BootstrapInfo, BootstrapPeerRecord, write_bootstrap_peer_record,
};
use crate::mesh_state::invite::{InviteClaims, parse_and_verify_invite_token};
use crate::mesh_state::network::NetworkConfig;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use ployz_api::{
    DaemonPayload, DaemonResponse, MembershipAttestation, MeshJoinPayload, MeshSelfRecordPayload,
    MeshStatusPayload, encode_join_response,
};
use ployz_api::{MachinePeerState, NodeStatusPayload};
use ployz_orchestrator::ipam::Ipam;
use ployz_types::model::{DrainState, JoinResponse, MachineId, MachineRecord, NetworkName, Phase};
use ployz_types::time::now_unix_secs;
use std::collections::HashMap;
use tracing::warn;

use crate::coordination::fanout::NodeStatusResult;
use crate::daemon::setup::MeshStartOptions;

#[cfg(test)]
use super::super::DaemonRuntimeConfig;
use super::super::DaemonState;

#[derive(Debug, Clone, Copy)]
pub(super) struct LocalPeerStatus {
    pub ready: bool,
    pub phase: Phase,
    pub drain_state: DrainState,
}

pub(super) enum PeerView<'a> {
    Local(LocalPeerStatus),
    Live(&'a NodeStatusPayload),
    Unreachable,
    Mismatch {
        reported: &'a MachineId,
        boot_id: &'a str,
    },
}

pub(super) struct NodeView<'a> {
    record: &'a MachineRecord,
    peer: PeerView<'a>,
}

impl PeerView<'_> {
    #[must_use]
    pub(super) fn peer_state(&self) -> MachinePeerState {
        match self {
            Self::Local(_) => MachinePeerState::Local,
            Self::Live(_) => MachinePeerState::Live,
            Self::Unreachable => MachinePeerState::Unreachable,
            Self::Mismatch { .. } => MachinePeerState::IdentityMismatch,
        }
    }

    #[must_use]
    pub(super) fn reachable(&self) -> bool {
        match self {
            Self::Local(_) | Self::Live(_) | Self::Mismatch { .. } => true,
            Self::Unreachable => false,
        }
    }

    #[must_use]
    pub(super) fn ready(&self) -> Option<bool> {
        match self {
            Self::Local(status) => Some(status.ready),
            Self::Live(status) => Some(status.ready),
            Self::Unreachable | Self::Mismatch { .. } => None,
        }
    }

    #[must_use]
    pub(super) fn phase(&self) -> Option<Phase> {
        match self {
            Self::Local(status) => Some(status.phase),
            Self::Live(status) => Some(status.phase),
            Self::Unreachable | Self::Mismatch { .. } => None,
        }
    }

    #[must_use]
    pub(super) fn drain_state(&self) -> Option<DrainState> {
        match self {
            Self::Local(status) => Some(status.drain_state),
            Self::Live(status) => Some(status.drain_state),
            Self::Unreachable | Self::Mismatch { .. } => None,
        }
    }

    #[must_use]
    pub(super) fn reported_machine_id(&self) -> Option<&MachineId> {
        match self {
            Self::Mismatch { reported, .. } => Some(reported),
            Self::Local(_) | Self::Live(_) | Self::Unreachable => None,
        }
    }

    #[must_use]
    pub(super) fn reported_boot_id(&self) -> Option<&str> {
        match self {
            Self::Mismatch { boot_id, .. } => Some(boot_id),
            Self::Local(_) | Self::Live(_) | Self::Unreachable => None,
        }
    }
}

impl<'a> NodeView<'a> {
    #[must_use]
    pub(super) fn new(record: &'a MachineRecord, peer: PeerView<'a>) -> Self {
        Self { record, peer }
    }

    #[must_use]
    pub(super) fn peer_state(&self) -> MachinePeerState {
        self.peer.peer_state()
    }

    #[must_use]
    pub(super) fn reachable(&self) -> bool {
        self.peer.reachable()
    }

    #[must_use]
    pub(super) fn ready(&self) -> Option<bool> {
        self.peer.ready()
    }

    #[must_use]
    pub(super) fn phase(&self) -> Option<Phase> {
        self.peer.phase()
    }

    #[must_use]
    pub(super) fn drain_state(&self) -> DrainState {
        self.peer.drain_state().unwrap_or(self.record.drain_state)
    }

    #[must_use]
    pub(super) fn reported_machine_id(&self) -> Option<&MachineId> {
        self.peer.reported_machine_id()
    }

    #[must_use]
    pub(super) fn reported_boot_id(&self) -> Option<&str> {
        self.peer.reported_boot_id()
    }

    #[must_use]
    pub(super) fn is_running(&self) -> bool {
        self.phase() == Some(Phase::Running)
    }

    #[must_use]
    pub(super) fn is_join_usable(&self) -> bool {
        matches!(&self.peer, PeerView::Local(_) | PeerView::Live(_))
            && self.is_running()
            && !self.drain_state().is_drained()
    }

    #[must_use]
    pub(super) fn is_deploy_eligible(&self) -> bool {
        self.is_join_usable()
    }
}

pub(super) fn resolve_peer<'a>(
    machine_id: &MachineId,
    local_machine_id: &MachineId,
    local_status: LocalPeerStatus,
    node_status_by_machine: &'a HashMap<MachineId, NodeStatusResult>,
) -> PeerView<'a> {
    if machine_id == local_machine_id {
        return PeerView::Local(local_status);
    }

    let Some(status) = node_status_by_machine.get(machine_id) else {
        return PeerView::Unreachable;
    };

    match status {
        NodeStatusResult::Ok(status) => PeerView::Live(status),
        NodeStatusResult::Offline => PeerView::Unreachable,
        NodeStatusResult::InvalidIdentity { reported, boot_id } => {
            PeerView::Mismatch { reported, boot_id }
        }
    }
}

#[must_use]
pub(super) fn with_local_machine_record(
    mut machines: Vec<MachineRecord>,
    local_record: &MachineRecord,
) -> Vec<MachineRecord> {
    if machines.iter().all(|machine| machine.id != local_record.id) {
        machines.push(local_record.clone());
    }
    machines
}

impl DaemonState {
    pub(crate) fn handle_mesh_list(&self) -> DaemonResponse {
        let networks_dir = self.data_dir.join("networks");
        let entries = match std::fs::read_dir(&networks_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return self.ok("no networks found");
            }
            Err(err) => {
                return self.err("IO_ERROR", format!("failed to read networks dir: {err}"));
            }
        };

        let mut names: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().to_str().map(ToOwned::to_owned))
            .collect();
        names.sort();

        if names.is_empty() {
            return self.ok("no networks found");
        }

        let running = self.active.as_ref().map(|a| a.config.name.0.as_str());
        let lines: Vec<String> = names
            .iter()
            .map(|name| {
                let state = if running == Some(name.as_str()) {
                    "running"
                } else {
                    "created"
                };
                format!("{name}: {state}")
            })
            .collect();

        self.ok(lines.join("\n"))
    }

    pub(crate) fn handle_mesh_status(&self, network: &str) -> DaemonResponse {
        let config_path = NetworkConfig::path(&self.data_dir, network);
        if !config_path.exists() {
            return self.ok_with_payload(
                format!("network: {network}\nstate:   missing"),
                Some(DaemonPayload::MeshStatus(MeshStatusPayload {
                    network_name: network.to_string(),
                    network_id: String::new(),
                    overlay: String::new(),
                    state: "missing".into(),
                    exists: false,
                })),
            );
        }

        let config = match NetworkConfig::load(&config_path) {
            Ok(config) => config,
            Err(err) => {
                return self.err("IO_ERROR", format!("failed to load network config: {err}"));
            }
        };

        let running = self
            .active
            .as_ref()
            .is_some_and(|a| a.config.name.0 == network);
        let state = if running { "running" } else { "created" };
        self.ok_with_payload(
            format!(
                "network: {}\noverlay: {}\nstate:   {}",
                config.name, config.overlay_ip, state
            ),
            Some(DaemonPayload::MeshStatus(MeshStatusPayload {
                network_name: config.name.0.clone(),
                network_id: config.id.0.clone(),
                overlay: config.overlay_ip.0.to_string(),
                state: state.into(),
                exists: true,
            })),
        )
    }

    pub(crate) async fn handle_mesh_join(&mut self, token: &str) -> DaemonResponse {
        if let Some(active) = &self.active {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh down` first",
                    active.config.name
                ),
            );
        }

        let invite = match parse_and_verify_invite_token(token) {
            Ok(invite) => invite,
            Err(err) => return self.err("INVALID_INVITE_TOKEN", err),
        };

        let now = now_unix_secs();
        if now > invite.expires_at {
            return self.err("INVITE_EXPIRED", "invite token has expired");
        }

        let network = invite.network_name.trim();
        if network.is_empty() {
            return self.err("INVALID_INVITE_TOKEN", "invite token network name is empty");
        }

        let config_path = NetworkConfig::path(&self.data_dir, network);
        if config_path.exists() {
            return self.err(
                "NETWORK_ALREADY_EXISTS",
                format!("network '{network}' already exists -- destroy it first"),
            );
        }

        let subnet = match invite.allocated_subnet.parse() {
            Ok(subnet) => subnet,
            Err(err) => {
                return self.err(
                    "INVALID_INVITE_TOKEN",
                    format!("invite allocated subnet is invalid: {err}"),
                );
            }
        };

        let mut net_config = NetworkConfig::new(
            NetworkName(network.to_string()),
            &self.identity.public_key,
            &self.cluster_cidr,
            subnet,
        );
        net_config.id = invite.network_id.clone();

        if let Err(e) = net_config.save(&config_path) {
            return self.err("IO_ERROR", format!("failed to save network config: {e}"));
        }

        if let Some(bootstrap_peer) = BootstrapPeerRecord::from_invite(&invite)
            && let Err(error) = write_bootstrap_peer_record(
                &NetworkConfig::dir(&self.data_dir, network),
                &bootstrap_peer,
            )
        {
            return self.err(
                "IO_ERROR",
                format!("failed to persist bootstrap founder peer: {error}"),
            );
        }

        let bootstrap = Self::extract_bootstrap(&invite);

        let options = MeshStartOptions {
            allow_disconnected_bootstrap: bootstrap.is_some(),
        };
        match self.start_mesh(net_config, bootstrap, options).await {
            Ok(_) => {}
            Err(e) => {
                return self.err(
                    "NETWORK_START_FAILED",
                    format!("join failed to start mesh: {e}"),
                );
            }
        }

        let Some(active) = self.active.as_ref() else {
            return self.err(
                "NETWORK_START_FAILED",
                format!("network '{network}' started without an active mesh"),
            );
        };
        let self_record = match mesh_self_record_payload(active, &self.identity).await {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let attestation = sign_membership_attestation(
            &self.identity,
            &invite.invite_id,
            &invite.challenge_nonce,
            &self.identity.machine_id.0,
        );
        let payload = MeshJoinPayload {
            encoded: self_record.encoded,
            record: self_record.record,
            attestation,
        };

        if let Some(self_down_tx) = self.self_down_tx.clone() {
            spawn_join_watchdog(
                active.store.machine(),
                self.identity.machine_id.clone(),
                self_down_tx,
            );
        }

        self.ok_with_payload(
            format!("joined and started network '{network}'"),
            Some(DaemonPayload::MeshJoin(payload)),
        )
    }

    pub(crate) fn handle_mesh_create(&self, network: &str) -> DaemonResponse {
        let net_config = match self.create_network_config(network) {
            Ok(config) => config,
            Err(message) => {
                return self.err("NETWORK_ALREADY_EXISTS", message);
            }
        };

        self.ok(format!(
            "created network '{}'\n  overlay: {}\n  state:   created",
            net_config.name, net_config.overlay_ip,
        ))
    }

    pub(crate) async fn handle_mesh_init(&mut self, network: &str) -> DaemonResponse {
        if let Some(active) = &self.active {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh down` first",
                    active.config.name,
                ),
            );
        }

        let net_config = match self.create_network_config(network) {
            Ok(config) => config,
            Err(message) => {
                return self.err("NETWORK_ALREADY_EXISTS", message);
            }
        };

        let network_name = net_config.name.clone();
        let overlay_ip = net_config.overlay_ip;
        match self
            .start_mesh(net_config, None, MeshStartOptions::default())
            .await
        {
            Ok(_) => {}
            Err(e) => {
                return self.err(
                    "NETWORK_START_FAILED",
                    format!(
                        "initialized network '{}' but failed to start: {e}\n  state:   created",
                        network_name,
                    ),
                );
            }
        }

        self.ok(format!(
            "initialized and started network '{}'\n  overlay: {}\n  state:   running",
            network_name, overlay_ip,
        ))
    }

    pub(crate) fn create_network_config(&self, network: &str) -> Result<NetworkConfig, String> {
        let config_path = NetworkConfig::path(&self.data_dir, network);
        if config_path.exists() {
            return Err(format!(
                "network '{network}' already exists -- use `mesh up {network}` or `mesh destroy {network}`"
            ));
        }

        let cluster: ipnet::Ipv4Net = self
            .cluster_cidr
            .parse()
            .map_err(|e| format!("invalid cluster CIDR '{}': {e}", self.cluster_cidr))?;
        let mut ipam = Ipam::new(cluster, self.subnet_prefix_len);
        let subnet = ipam
            .allocate()
            .ok_or_else(|| "no available subnets in cluster CIDR".to_string())?;

        let net_config = NetworkConfig::new(
            NetworkName(network.into()),
            &self.identity.public_key,
            &self.cluster_cidr,
            subnet,
        );

        if let Err(e) = net_config.save(&config_path) {
            return Err(format!("failed to save network config: {e}"));
        }

        Ok(net_config)
    }

    pub(crate) async fn handle_mesh_up(
        &mut self,
        network: &str,
        allow_disconnected_bootstrap: bool,
    ) -> DaemonResponse {
        if let Some(active) = &self.active {
            return self.err(
                "NETWORK_ALREADY_RUNNING",
                format!(
                    "network '{}' is already running -- run `mesh down` first",
                    active.config.name,
                ),
            );
        }

        let config_path = NetworkConfig::path(&self.data_dir, network);
        if !config_path.exists() {
            return self.err(
                "NETWORK_NOT_FOUND",
                format!(
                    "network '{network}' does not exist -- run `mesh create {network}` or `mesh init {network}`"
                ),
            );
        }

        let net_config = match NetworkConfig::load(&config_path) {
            Ok(config) => config,
            Err(e) => {
                return self.err("IO_ERROR", format!("failed to load network config: {e}"));
            }
        };

        let network_name = net_config.name.clone();
        let options = MeshStartOptions {
            allow_disconnected_bootstrap,
        };
        match self.start_mesh(net_config, None, options).await {
            Ok(_) => {}
            Err(e) => {
                return self.err("NETWORK_START_FAILED", e.to_string());
            }
        }

        self.ok(format!("mesh '{}' started", network_name))
    }

    pub(crate) async fn handle_mesh_down(&mut self) -> DaemonResponse {
        let Some(mut active) = self.active.take() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };

        if let Err(e) = active.mesh.destroy().await {
            self.active = Some(active);
            return self.err("NETWORK_STOP_FAILED", format!("mesh down failed: {e}"));
        }
        let _ = active.remote_control.shutdown().await;
        if let Err(e) = active.dns.shutdown().await {
            warn!(?e, "dns stop failed during mesh down");
        }
        if let Err(e) = active.gateway.shutdown().await {
            return self.err("NETWORK_STOP_FAILED", format!("gateway stop failed: {e}"));
        }

        self.clear_active_marker();
        self.ok("mesh stopped (config kept)")
    }

    pub(crate) async fn handle_mesh_destroy(&mut self, network: &str) -> DaemonResponse {
        let running_target = self
            .active
            .as_ref()
            .is_some_and(|a| a.config.name.0 == network);

        let config_path = NetworkConfig::path(&self.data_dir, network);
        if !running_target && !config_path.exists() {
            return self.err(
                "NETWORK_NOT_FOUND",
                format!("network '{network}' does not exist"),
            );
        }

        if running_target {
            let Some(mut active) = self.active.take() else {
                return self.err("NO_RUNNING_NETWORK", "no mesh running");
            };
            if let Err(e) = active.mesh.destroy().await {
                self.active = Some(active);
                return self.err("NETWORK_DESTROY_FAILED", format!("destroy failed: {e}"));
            }
            let _ = active.remote_control.shutdown().await;
            if let Err(e) = active.dns.shutdown().await {
                warn!(?e, "dns stop failed during mesh destroy");
            }
            if let Err(e) = active.gateway.shutdown().await {
                return self.err(
                    "NETWORK_DESTROY_FAILED",
                    format!("gateway stop failed: {e}"),
                );
            }
        }

        if let Err(e) = NetworkConfig::delete(&self.data_dir, network) {
            return self.err("IO_ERROR", format!("failed to delete network config: {e}"));
        }

        self.clear_active_marker();
        self.ok(format!("mesh '{network}' destroyed"))
    }

    pub(crate) async fn handle_mesh_self_record(&self) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(a) => a,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };
        let payload = match mesh_self_record_payload(active, &self.identity).await {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        self.ok_with_payload(
            payload.encoded.clone(),
            Some(DaemonPayload::MeshSelfRecord(payload)),
        )
    }

    pub(crate) async fn handle_mesh_accept(&self, response: &str) -> DaemonResponse {
        let _ = response;
        self.err(
            "UNSUPPORTED",
            "mesh accept is obsolete; use `machine add` so membership is committed explicitly",
        )
    }

    fn extract_bootstrap(invite: &InviteClaims) -> Option<BootstrapInfo> {
        let wg_key_b64 = invite.issuer_wg_public_key.as_deref()?;
        let overlay_str = invite.issuer_overlay_ip.as_deref()?;

        let key_bytes = URL_SAFE_NO_PAD.decode(wg_key_b64).ok()?;
        let peer_wg_public_key: [u8; 32] = key_bytes.as_slice().try_into().ok()?;
        let peer_overlay_ip: std::net::Ipv6Addr = overlay_str.parse().ok()?;

        if invite.issuer_endpoints.is_empty() {
            return None;
        }

        Some(BootstrapInfo {
            peer_id: invite.issued_by.clone(),
            peer_wg_public_key,
            peer_overlay_ip,
            peer_endpoints: invite.issuer_endpoints.clone(),
        })
    }
}

pub(crate) fn membership_attestation_message(
    invite_id: &str,
    challenge_nonce: &str,
    machine_id: &str,
) -> Vec<u8> {
    let mut buf =
        Vec::with_capacity(invite_id.len() + challenge_nonce.len() + machine_id.len() + 2);
    buf.extend_from_slice(invite_id.as_bytes());
    buf.push(0);
    buf.extend_from_slice(challenge_nonce.as_bytes());
    buf.push(0);
    buf.extend_from_slice(machine_id.as_bytes());
    buf
}

pub(crate) fn sign_membership_attestation(
    identity: &ployz_types::model::Identity,
    invite_id: &str,
    challenge_nonce: &str,
    machine_id: &str,
) -> MembershipAttestation {
    let signing_key = SigningKey::from_bytes(&identity.private_key.0);
    let verify_key = signing_key.verifying_key();
    let message = membership_attestation_message(invite_id, challenge_nonce, machine_id);
    let signature = signing_key.sign(&message);
    MembershipAttestation {
        challenge_nonce: challenge_nonce.to_string(),
        ed25519_public_key: URL_SAFE_NO_PAD.encode(verify_key.to_bytes()),
        signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
    }
}

pub(crate) fn verify_membership_attestation(
    record: &MachineRecord,
    invite_id: &str,
    challenge_nonce: &str,
    attestation: &MembershipAttestation,
) -> Result<(), String> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    if attestation.challenge_nonce != challenge_nonce {
        return Err("membership challenge nonce mismatch".to_string());
    }

    let key_bytes = URL_SAFE_NO_PAD
        .decode(&attestation.ed25519_public_key)
        .map_err(|error| format!("decode attestation ed25519 key: {error}"))?;
    let key_arr: [u8; 32] = key_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "invalid ed25519 key length".to_string())?;
    let verify_key = VerifyingKey::from_bytes(&key_arr)
        .map_err(|error| format!("parse attestation ed25519 key: {error}"))?;

    let sig_bytes = URL_SAFE_NO_PAD
        .decode(&attestation.signature)
        .map_err(|error| format!("decode attestation signature: {error}"))?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "invalid attestation signature length".to_string())?;
    let signature = Signature::from_bytes(&sig_arr);

    let message = membership_attestation_message(invite_id, challenge_nonce, &record.id.0);
    verify_key
        .verify(&message, &signature)
        .map_err(|error| format!("verify attestation: {error}"))
}

const JOIN_WATCHDOG_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
const JOIN_WATCHDOG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn spawn_join_watchdog(
    store: std::sync::Arc<dyn ployz_store_api::MachineStore>,
    self_id: MachineId,
    self_down_tx: tokio::sync::mpsc::Sender<String>,
) {
    tokio::spawn(async move {
        let start = std::time::Instant::now();
        loop {
            tokio::time::sleep(JOIN_WATCHDOG_POLL_INTERVAL).await;
            match store.list_machines().await {
                Ok(machines) => {
                    if machines.iter().any(|machine| machine.id == self_id) {
                        tracing::info!(
                            machine_id = %self_id,
                            elapsed_ms = start.elapsed().as_millis(),
                            "join watchdog: self-record observed, exiting"
                        );
                        return;
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        machine_id = %self_id,
                        "join watchdog: store list_machines failed"
                    );
                }
            }
            if start.elapsed() >= JOIN_WATCHDOG_TIMEOUT {
                let reason = format!(
                    "joiner '{}' did not observe self-record within {:?}; tearing down mesh",
                    self_id.0, JOIN_WATCHDOG_TIMEOUT
                );
                tracing::warn!(%reason, "join watchdog: timeout reached");
                let _ = self_down_tx.send(reason).await;
                return;
            }
        }
    });
}

async fn mesh_self_record_payload(
    active: &crate::daemon::ActiveMesh,
    identity: &ployz_types::model::Identity,
) -> Result<MeshSelfRecordPayload, DaemonResponse> {
    let endpoints = active
        .mesh
        .detect_endpoints()
        .await
        .map_err(|error| DaemonResponse {
            ok: false,
            code: "ENDPOINT_DISCOVERY_FAILED".into(),
            message: format!("failed to detect mesh endpoints: {error}"),
            payload: None,
        })?;
    let response = JoinResponse {
        machine_id: identity.machine_id.clone(),
        public_key: identity.public_key.clone(),
        overlay_ip: active.config.overlay_ip,
        subnet: Some(active.config.subnet),
        endpoints,
    };
    let encoded = encode_join_response(&response).map_err(|error| DaemonResponse {
        ok: false,
        code: "ENCODE_FAILED".into(),
        message: format!("failed to encode self-record: {error}"),
        payload: None,
    })?;
    Ok(MeshSelfRecordPayload {
        encoded,
        record: response.into_seed_machine_record(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::ActiveMesh;
    use crate::daemon::store::StoreDriver;
    use crate::mesh_state::invite::{InviteTokenContext, issue_invite_token};
    use crate::mesh_state::network::NetworkConfig;
    use ployz_api::{DaemonPayload, encode_join_response};
    use ployz_orchestrator::Mesh;
    use ployz_store_api::MachineStore;
    use ployz_test_support::{
        MemoryServiceRuntime, MemoryStore, MemoryWireGuard, StaticEndpointDiscovery,
        memory_wireguard_driver,
    };
    use ployz_types::model::Identity;
    use ployz_types::model::{MachineId, OverlayIp, PublicKey};
    use ployz_types::time::now_unix_secs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn membership_attestation_rejects_wrong_challenge_nonce() {
        let identity = Identity::generate(MachineId("joiner".into()), [9; 32]);
        let record = JoinResponse {
            machine_id: identity.machine_id.clone(),
            public_key: identity.public_key.clone(),
            overlay_ip: "fd00::9".parse().map(OverlayIp).expect("valid overlay"),
            subnet: Some("10.210.9.0/24".parse().expect("valid subnet")),
            endpoints: vec![String::from("203.0.113.9:51820")],
        }
        .into_seed_machine_record();
        let attestation =
            sign_membership_attestation(&identity, "invite-1", "challenge-a", &record.id.0);

        let error = verify_membership_attestation(&record, "invite-1", "challenge-b", &attestation)
            .expect_err("mismatched challenge must fail");
        assert!(error.contains("challenge nonce mismatch"));
    }

    #[tokio::test]
    async fn mesh_join_uses_founder_allocated_subnet_exactly() {
        let founder_identity =
            Identity::generate(ployz_types::model::MachineId("founder".into()), [7; 32]);
        let joiner_identity =
            Identity::generate(ployz_types::model::MachineId("joiner".into()), [8; 32]);
        let founder_subnet: ipnet::Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
        let allocated_subnet = "10.210.42.0/24";
        let network = NetworkConfig::new(
            ployz_types::model::NetworkName("alpha".into()),
            &founder_identity.public_key,
            "10.210.0.0/16",
            founder_subnet,
        );

        let (token, _) = issue_invite_token(
            &founder_identity,
            &network,
            600,
            now_unix_secs(),
            InviteTokenContext {
                issuer_endpoints: Vec::new(),
                issuer_overlay_ip: Some(network.overlay_ip.0.to_string()),
                issuer_wg_public_key: Some("wg-public".into()),
                issuer_subnet: Some(network.subnet.to_string()),
                allocated_subnet: allocated_subnet.into(),
            },
        )
        .expect("issue invite");

        let data_dir = unique_temp_dir("ployz-mesh-join");
        let mut state = DaemonState::new_for_tests(
            &data_dir,
            joiner_identity,
            DaemonRuntimeConfig {
                cluster_cidr: "10.210.0.0/16".into(),
                subnet_prefix_len: 24,
                remote_control_port: 4317,
                coordination_rpc_port: 0,
                gateway_listen_addr: "127.0.0.1:0".into(),
                gateway_threads: 1,
            },
        );

        let response = state.handle_mesh_join(&token).await;
        assert!(response.ok, "{}", response.message);

        let active = state.active.as_ref().expect("mesh active");
        assert_eq!(active.config.subnet.to_string(), allocated_subnet);

        if let Some(active) = state.active.as_mut() {
            active.mesh.destroy().await.expect("destroy mesh");
        }
    }

    #[tokio::test]
    async fn mesh_accept_is_rejected() {
        let (mut state, _store, _network) = make_active_state().await;
        let response = JoinResponse {
            machine_id: MachineId("joiner".into()),
            public_key: PublicKey([2; 32]),
            overlay_ip: "fd00::2".parse().map(OverlayIp).expect("valid overlay"),
            subnet: Some("10.210.1.0/24".parse().expect("valid subnet")),
            endpoints: vec!["203.0.113.10:51820".into()],
        };
        let response = encode_join_response(&response).expect("encode join response");

        let result = state.handle_mesh_accept(&response).await;
        assert!(!result.ok);
        assert_eq!(result.code, "UNSUPPORTED");
        assert!(result.message.contains("machine add"));

        if let Some(active) = state.active.as_mut() {
            active.mesh.destroy().await.expect("destroy mesh");
        }
    }

    #[test]
    fn mesh_status_returns_missing_payload_when_network_does_not_exist() {
        let state = DaemonState::new_for_tests(
            &unique_temp_dir("ployz-mesh-status-missing"),
            Identity::generate(MachineId("founder".into()), [1; 32]),
            DaemonRuntimeConfig {
                cluster_cidr: "10.210.0.0/16".into(),
                subnet_prefix_len: 24,
                remote_control_port: 4317,
                coordination_rpc_port: 0,
                gateway_listen_addr: "127.0.0.1:0".into(),
                gateway_threads: 1,
            },
        );

        let response = state.handle_mesh_status("missing-net");
        assert!(response.ok, "{}", response.message);
        assert_eq!(response.code, "OK");
        assert!(response.message.contains("state:   missing"));

        let Some(DaemonPayload::MeshStatus(payload)) = response.payload else {
            panic!("expected mesh status payload");
        };
        assert_eq!(payload.network_name, "missing-net");
        assert_eq!(payload.network_id, "");
        assert_eq!(payload.overlay, "");
        assert_eq!(payload.state, "missing");
        assert!(!payload.exists);
    }

    #[test]
    fn mesh_status_returns_present_payload_when_network_exists() {
        let data_dir = unique_temp_dir("ployz-mesh-status-present");
        let identity = Identity::generate(MachineId("founder".into()), [1; 32]);
        let config = NetworkConfig::new(
            ployz_types::model::NetworkName("alpha".into()),
            &identity.public_key,
            "10.210.0.0/16",
            "10.210.0.0/24".parse().expect("valid subnet"),
        );
        let config_path = NetworkConfig::path(&data_dir, &config.name.0);
        config.save(&config_path).expect("save network config");

        let state = DaemonState::new_for_tests(
            &data_dir,
            identity,
            DaemonRuntimeConfig {
                cluster_cidr: "10.210.0.0/16".into(),
                subnet_prefix_len: 24,
                remote_control_port: 4317,
                coordination_rpc_port: 0,
                gateway_listen_addr: "127.0.0.1:0".into(),
                gateway_threads: 1,
            },
        );

        let response = state.handle_mesh_status("alpha");
        assert!(response.ok, "{}", response.message);

        let Some(DaemonPayload::MeshStatus(payload)) = response.payload else {
            panic!("expected mesh status payload");
        };
        assert_eq!(payload.network_name, "alpha");
        assert_eq!(payload.state, "created");
        assert!(payload.exists);
    }

    async fn make_active_state() -> (DaemonState, Arc<MemoryStore>, Arc<MemoryWireGuard>) {
        let identity = Identity::generate(MachineId("founder".into()), [1; 32]);
        let config = NetworkConfig::new(
            ployz_types::model::NetworkName("alpha".into()),
            &identity.public_key,
            "10.210.0.0/16",
            "10.210.0.0/24".parse().expect("valid subnet"),
        );
        let store = Arc::new(MemoryStore::new());
        store
            .upsert_self_machine(&ployz_types::model::MachineRecord {
                id: identity.machine_id.clone(),
                public_key: identity.public_key.clone(),
                overlay_ip: config.overlay_ip,
                subnet: Some(config.subnet),
                bridge_ip: None,
                endpoints: vec!["127.0.0.1:51820".into()],
                status: ployz_types::model::MachineStatus::Unknown,
                drain_state: ployz_types::model::DrainState::Active,
                created_at: 0,
                updated_at: 0,
                labels: std::collections::BTreeMap::new(),
            })
            .await
            .expect("upsert founder");
        let network = Arc::new(MemoryWireGuard::new());
        let service = Arc::new(MemoryServiceRuntime::new());
        let mut mesh = Mesh::new(
            memory_wireguard_driver(network.clone()),
            store.clone(),
            store.clone(),
            service,
            None,
            Arc::new(StaticEndpointDiscovery::empty()),
            None,
            identity.machine_id.clone(),
            String::from("boot-test"),
            51820,
        );
        mesh.up().await.expect("mesh up");

        let mut state = DaemonState::new_for_tests(
            &unique_temp_dir("ployz-mesh-accept"),
            identity,
            DaemonRuntimeConfig {
                cluster_cidr: "10.210.0.0/16".into(),
                subnet_prefix_len: 24,
                remote_control_port: 4317,
                coordination_rpc_port: 0,
                gateway_listen_addr: "127.0.0.1:0".into(),
                gateway_threads: 1,
            },
        );
        state.active = Some(ActiveMesh {
            config,
            mesh,
            store: StoreDriver::from_store(store.clone()),
            remote_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        });

        (state, store, network)
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }
}
