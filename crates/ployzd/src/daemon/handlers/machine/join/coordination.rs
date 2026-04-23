use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use ipnet::Ipv4Net;
use ployz_api::{CoordOp, DaemonRequest, ResourceKey as ApiResourceKey};
use ployz_orchestrator::coordination::{
    PrepareVote, Reservation, ReservationId, ResourceKey, Vote, quorum_prepare,
};
use ployz_orchestrator::ipam::pick_candidate_subnet;
use ployz_orchestrator::machine_policy::coordination_peers as policy_coordination_peers;
use ployz_store_api::{InviteStore, MachineStore, StoreDriver};
use ployz_types::model::{MachineId, MachineRecord, OverlayIp};
use ployz_types::time::now_unix_secs;

use crate::daemon::DaemonState;

use super::super::types::MachineAddContext;
use super::remote::{overlay_rpc, remote_response_error};

const SUBNET_RESERVATION_TTL_SECS: u64 = 30;
const MAX_SUBNET_ATTEMPTS: usize = 64;

#[derive(Debug, Clone)]
pub(in super::super) struct BootstrapSubnetClaim {
    reservation: Reservation,
    pub(super) subnet: Ipv4Net,
    quorum_peers: Vec<CoordinationPeer>,
    pub(super) peer_rpc_port: u16,
}

impl BootstrapSubnetClaim {
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub(in crate::daemon::handlers::machine) fn subnet(&self) -> Ipv4Net {
        self.subnet
    }

    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub(in crate::daemon::handlers::machine) fn reservation_key(&self) -> &ResourceKey {
        &self.reservation.key
    }

    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub(in crate::daemon::handlers::machine) fn reservation_nonce(&self) -> &str {
        &self.reservation.nonce
    }

    #[must_use]
    pub(in crate::daemon::handlers::machine) fn quorum_peer_ids(&self) -> Vec<MachineId> {
        self.quorum_peers
            .iter()
            .map(|peer| peer.machine_id.clone())
            .collect()
    }
}

#[derive(Debug, Clone)]
struct CoordinationPeer {
    machine_id: MachineId,
    overlay_ip: OverlayIp,
}

impl DaemonState {
    pub(in crate::daemon::handlers::machine) async fn reserve_machine_subnet(
        &self,
        owner: &MachineId,
    ) -> Result<BootstrapSubnetClaim, String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no running network".to_string())?;
        let peer_rpc_port = self
            .peer_control_port()
            .map_err(|error| error.to_string())?;
        let cluster: Ipv4Net = self
            .cluster_cidr
            .parse()
            .map_err(|err| format!("invalid cluster CIDR '{}': {err}", self.cluster_cidr))?;
        let now = now_unix_secs();
        let machines = active
            .mesh
            .store
            .list_machines()
            .await
            .map_err(|err| format!("list machines for subnet reservation: {err}"))?;
        let bias_seed = machine_bias_seed(owner);

        let mut taken = machines
            .iter()
            .filter_map(|machine| machine.subnet)
            .collect::<HashSet<_>>();
        taken.extend(self.reservations.active_subnets(now).await);
        let quorum_peers = coordination_peers(&machines, &self.identity.machine_id);

        for _ in 0..MAX_SUBNET_ATTEMPTS {
            let Some(candidate) =
                pick_candidate_subnet(cluster, self.subnet_prefix_len, &taken, bias_seed)
            else {
                return Err("no available subnets".into());
            };
            let reservation = Reservation {
                id: ReservationId::random(),
                key: ResourceKey::Subnet(candidate),
                owner: owner.clone(),
                nonce: ReservationId::random().0,
                expires_at: now.saturating_add(SUBNET_RESERVATION_TTL_SECS),
            };
            let committed_taken = machines
                .iter()
                .any(|machine| machine.subnet == Some(candidate));
            let local_vote = self
                .reservations
                .prepare(reservation.clone(), committed_taken, now)
                .await;
            match local_vote {
                Vote::Allow => {}
                Vote::Deny(_) => {
                    taken.insert(candidate);
                    continue;
                }
            }

            let decision = quorum_prepare(&quorum_peers, PrepareVote::Allow, |peer| {
                let reservation = reservation.clone();
                async move { remote_coord_prepare(&peer, &reservation, peer_rpc_port).await }
            })
            .await;
            if decision.allowed {
                return Ok(BootstrapSubnetClaim {
                    reservation,
                    subnet: candidate,
                    quorum_peers,
                    peer_rpc_port,
                });
            }
            let _ = self
                .reservations
                .release(&reservation.key, &reservation.nonce, now_unix_secs())
                .await;
            release_remote_reservation_holds(&quorum_peers, &reservation, peer_rpc_port, candidate)
                .await;
            if !decision.retry_could_succeed() {
                return Err(format!(
                    "failed to reach quorum for subnet reservation ({}/{})",
                    decision.votes_for, decision.votes_total
                ));
            }
            taken.insert(candidate);
        }

        Err("no available subnets".into())
    }
}

pub(super) async fn release_reserved_subnet(
    context: &MachineAddContext,
    subnet_claim: &BootstrapSubnetClaim,
) -> Result<(), String> {
    let now = now_unix_secs();
    let local_vote = context
        .reservations
        .release(
            &subnet_claim.reservation.key,
            &subnet_claim.reservation.nonce,
            now,
        )
        .await;
    if let Vote::Deny(conflict) = local_vote {
        tracing::warn!(
            ?conflict,
            subnet = %subnet_claim.subnet,
            "subnet reservation release denied locally"
        );
    }
    release_remote_reservation_holds(
        &subnet_claim.quorum_peers,
        &subnet_claim.reservation,
        subnet_claim.peer_rpc_port,
        subnet_claim.subnet,
    )
    .await;
    Ok(())
}

async fn release_remote_reservation_holds(
    peers: &[CoordinationPeer],
    reservation: &Reservation,
    peer_rpc_port: u16,
    subnet: Ipv4Net,
) {
    for peer in peers {
        if let Err(err) = remote_coord_release(peer, reservation, peer_rpc_port).await {
            tracing::warn!(
                peer = %peer.machine_id,
                subnet = %subnet,
                error = %err,
                "subnet reservation release fanout failed"
            );
        }
    }
}

pub(super) async fn consume_invite(
    context: &MachineAddContext,
    invite_id: &str,
    machine_id: &MachineId,
) -> Result<(), String> {
    context
        .store
        .redeem_invite(invite_id, machine_id, now_unix_secs())
        .await
        .map(|_| ())
        .map_err(|err| format!("consume invite: {err}"))
}

fn machine_bias_seed(machine_id: &MachineId) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    machine_id.hash(&mut hasher);
    hasher.finish()
}

fn coordination_peers(machines: &[MachineRecord], self_id: &MachineId) -> Vec<CoordinationPeer> {
    policy_coordination_peers(machines, self_id)
        .into_iter()
        .map(|machine| CoordinationPeer {
            machine_id: machine.id.clone(),
            overlay_ip: machine.overlay_ip,
        })
        .collect()
}

async fn remote_coord_prepare(
    peer: &CoordinationPeer,
    reservation: &Reservation,
    peer_rpc_port: u16,
) -> PrepareVote {
    match overlay_rpc(
        peer.overlay_ip,
        peer_rpc_port,
        DaemonRequest::Coord {
            op: CoordOp::Prepare {
                id: reservation.id.0.clone(),
                key: api_resource_key(&reservation.key),
                owner: reservation.owner.clone(),
                nonce: reservation.nonce.clone(),
                ttl_secs: reservation.expires_at.saturating_sub(now_unix_secs()),
            },
        },
    )
    .await
    {
        Ok(response) if response.ok => PrepareVote::Allow,
        Ok(response) => {
            tracing::warn!(
                peer = %peer.machine_id,
                code = %response.code,
                message = %response.message,
                "subnet reservation prepare denied by peer"
            );
            classify_remote_prepare_denial(&response.message)
        }
        Err(err) => {
            tracing::warn!(peer = %peer.machine_id, error = %err, "subnet reservation prepare rpc failed");
            PrepareVote::TerminalDeny
        }
    }
}

fn classify_remote_prepare_denial(message: &str) -> PrepareVote {
    if message.contains("HeldBy") || message.contains("AlreadyCommitted") {
        return PrepareVote::RetryableDeny;
    }
    PrepareVote::TerminalDeny
}

async fn remote_coord_release(
    peer: &CoordinationPeer,
    reservation: &Reservation,
    peer_rpc_port: u16,
) -> Result<(), String> {
    let response = overlay_rpc(
        peer.overlay_ip,
        peer_rpc_port,
        DaemonRequest::Coord {
            op: CoordOp::Release {
                id: reservation.id.0.clone(),
                key: api_resource_key(&reservation.key),
                nonce: reservation.nonce.clone(),
            },
        },
    )
    .await?;
    if response.ok {
        return Ok(());
    }
    Err(remote_response_error(&response))
}

pub(super) async fn persist_machine_control_target(
    context: &MachineAddContext,
    machine_id: &MachineId,
    control_target: &str,
) -> Result<(), String> {
    let Some(mut record) =
        super::super::list::find_machine_record(&context.store, machine_id).await?
    else {
        tracing::info!(
            machine_id = %machine_id,
            control_target,
            "machine record not visible in store yet; deferring control target persistence"
        );
        return Ok(());
    };
    record.control_target = Some(control_target.to_string());
    context
        .store
        .upsert_self_machine(&record)
        .await
        .map_err(|err| format!("persist control target: {err}"))
}

pub(super) async fn assert_subnet_unique(
    store: &StoreDriver,
    machine_id: &MachineId,
    claimed_subnet: Ipv4Net,
) -> Result<(), String> {
    let machines = store
        .list_machines()
        .await
        .map_err(|err| format!("list machines for subnet assertion: {err}"))?;
    let conflicting_machine_ids = machines
        .into_iter()
        .filter(|machine| machine.id != *machine_id && machine.subnet == Some(claimed_subnet))
        .map(|machine| machine.id.0)
        .collect::<Vec<_>>();
    if conflicting_machine_ids.is_empty() {
        return Ok(());
    }

    Err(format!(
        "subnet uniqueness invariant violated for machine '{}' subnet '{}'; conflicting machines: {}",
        machine_id,
        claimed_subnet,
        conflicting_machine_ids.join(", ")
    ))
}

fn api_resource_key(key: &ResourceKey) -> ApiResourceKey {
    match key {
        ResourceKey::Subnet(subnet) => ApiResourceKey::Subnet(*subnet),
        ResourceKey::DeployNamespace(namespace) => {
            ApiResourceKey::DeployNamespace(namespace.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::assert_subnet_unique;
    use ipnet::Ipv4Net;
    use ployz_store_api::{MachineStore, StoreDriver};
    use ployz_types::model::{MachineId, MachineRecord, OverlayIp, Participation, PublicKey};

    #[tokio::test]
    async fn subnet_assertion_rejects_duplicate_claims() {
        let store = StoreDriver::memory();
        let subnet: Ipv4Net = "10.210.1.0/24".parse().expect("valid subnet");
        store
            .upsert_self_machine(&machine_record("alpha", "::1", Some(subnet)))
            .await
            .expect("upsert alpha");
        store
            .upsert_self_machine(&machine_record("beta", "::2", Some(subnet)))
            .await
            .expect("upsert beta");

        let err = assert_subnet_unique(&store, &MachineId("alpha".into()), subnet)
            .await
            .expect_err("duplicate subnet should fail");

        assert!(err.contains("beta"));
    }

    #[tokio::test]
    async fn subnet_assertion_accepts_unique_claim() {
        let store = StoreDriver::memory();
        let claimed_subnet: Ipv4Net = "10.210.1.0/24".parse().expect("valid subnet");
        let other_subnet: Ipv4Net = "10.210.2.0/24".parse().expect("valid subnet");
        store
            .upsert_self_machine(&machine_record("alpha", "::1", Some(claimed_subnet)))
            .await
            .expect("upsert alpha");
        store
            .upsert_self_machine(&machine_record("beta", "::2", Some(other_subnet)))
            .await
            .expect("upsert beta");

        let result = assert_subnet_unique(&store, &MachineId("alpha".into()), claimed_subnet).await;

        assert!(result.is_ok());
    }

    fn machine_record(machine_id: &str, overlay_ip: &str, subnet: Option<Ipv4Net>) -> MachineRecord {
        let record = MachineRecord::seed(
            MachineId(machine_id.into()),
            PublicKey([1; 32]),
            overlay_ip.parse().map(OverlayIp).expect("valid overlay"),
            subnet,
            vec![],
        );
        MachineRecord {
            participation: Participation::Enabled,
            ..record
        }
    }
}
