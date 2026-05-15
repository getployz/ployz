use crate::daemon::ActiveMesh;
use crate::daemon::handlers::machine::render::format_lifecycle;
use ployz_api::{DoctorLocal, DoctorOverall, DoctorPayload, DoctorPeer};
use ployz_model::{MachineId, MachineMembership, PublicKey, StorageParticipation};
use ployz_orchestrator::machine_policy::{DiagnosticRole, diagnostic_role};
use ployz_orchestrator::mesh::DevicePeer;
use ployz_store_api::PeerRttObservation;
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;
use tokio::time::Instant;

const PARTICIPATION_HANDSHAKE_FRESHNESS_WINDOW: Duration = Duration::from_secs(30);

pub(super) fn build_doctor_payload(
    active: &ActiveMesh,
    machines: &[MachineMembership],
    local_record: &MachineMembership,
    device_peers: &[DevicePeer],
    rtt_observations: &[PeerRttObservation],
    detected_local_endpoints: &[String],
    endpoint_watch_supported: bool,
) -> DoctorPayload {
    let handshake_by_key = handshake_state_map(device_peers);
    let rtt_by_ip = rtt_state_map(rtt_observations);
    let peers = build_participation_rows(machines, &local_record.id, &handshake_by_key, &rtt_by_ip);
    DoctorPayload {
        overall: DoctorOverall {
            lifecycle: if peers.iter().any(|row| row.blocking) {
                String::from("blocked")
            } else {
                String::from("healthy")
            },
        },
        local: DoctorLocal {
            machine_id: local_record.id.as_str().to_string(),
            network: active.config.name.0.clone(),
            network_lifecycle: active.config.lifecycle.to_string(),
            machine_lifecycle: format_lifecycle(local_record).to_string(),
            storage: local_record.storage(),
            storage_participation: format_storage_participation(local_record),
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

fn format_storage_participation(machine: &MachineMembership) -> String {
    match &machine.storage_participation() {
        StorageParticipation::Candidate => String::from("candidate"),
        StorageParticipation::Authority { authority_id } => {
            format!("authority:{}", authority_id.as_str())
        }
    }
}

fn diagnostic_role_name(role: DiagnosticRole) -> &'static str {
    match role {
        DiagnosticRole::Blocking => "blocking",
        DiagnosticRole::Informational => "informational",
    }
}

fn cause_parts(handshake: HandshakeState) -> (&'static str, &'static str) {
    match handshake {
        HandshakeState::Fresh => (
            "fresh-wireguard-handshake",
            "wireguard has a recent peer handshake",
        ),
        HandshakeState::Absent => ("no-direct-peer", "no direct peer is configured"),
        HandshakeState::None => ("no-wireguard-handshake", "direct peer has no handshake yet"),
        HandshakeState::Stale => ("stale-wireguard-handshake", "wireguard handshake is stale"),
    }
}

fn build_participation_rows(
    machines: &[MachineMembership],
    local_machine_id: &MachineId,
    handshake_by_key: &HashMap<PublicKey, HandshakeState>,
    rtt_by_ip: &HashMap<IpAddr, RttState>,
) -> Vec<DoctorPeer> {
    let mut rows: Vec<DoctorPeer> = machines
        .iter()
        .filter_map(|machine| {
            let role = diagnostic_role(&machine.placement_candidate(), local_machine_id)?;
            let handshake_state = handshake_by_key
                .get(&machine.public_key)
                .copied()
                .unwrap_or(HandshakeState::Absent);
            let rtt = rtt_by_ip.get(&IpAddr::V6(machine.overlay_ip.0)).copied();
            let (cause_code, cause_message) = cause_parts(handshake_state);
            let healthy = handshake_state == HandshakeState::Fresh;
            Some(DoctorPeer {
                machine_id: machine.id.as_str().to_string(),
                role: diagnostic_role_name(role).to_string(),
                storage: machine.storage(),
                storage_participation: format_storage_participation(machine),
                blocking: role == DiagnosticRole::Blocking && !healthy,
                store_lifecycle: format_lifecycle(machine).to_string(),
                subnet: machine.subnet.map(|subnet| subnet.to_string()),
                wg_state: handshake_state.as_str().to_string(),
                probe_state: String::from("not-used"),
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
    let median = if sorted.len().is_multiple_of(2) {
        let (lower, upper) = sorted.split_at(mid);
        let Some(&left) = lower.last() else {
            return None;
        };
        let Some(&right) = upper.first() else {
            return None;
        };
        (left as f64 + right as f64) / 2.0
    } else {
        let Some(&value) = sorted.get(mid) else {
            return None;
        };
        value as f64
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
