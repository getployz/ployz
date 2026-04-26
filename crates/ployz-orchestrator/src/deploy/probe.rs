use crate::model::{MachineId, MachineRecord};
use ployz_store_api::{
    PeerMembershipObservation, PeerMembershipState, PeerMembershipStore, StoreDriver,
};
use std::collections::{BTreeSet, HashMap};
use std::net::IpAddr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParticipantReachability {
    pub(super) unreachable: Vec<MachineId>,
    details: HashMap<MachineId, ParticipantReachabilityDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParticipantReachabilityDetail {
    membership: &'static str,
    wg: &'static str,
    rpc: &'static str,
}

pub(super) async fn probe_participants(
    store: &StoreDriver,
    participants: &BTreeSet<MachineId>,
    machine_map: &HashMap<MachineId, MachineRecord>,
) -> ParticipantReachability {
    let observations = store
        .peer_membership_observations()
        .await
        .unwrap_or_default();
    reachability_from_membership(participants, machine_map, observations.as_slice())
}

fn reachability_from_membership(
    participants: &BTreeSet<MachineId>,
    machine_map: &HashMap<MachineId, MachineRecord>,
    observations: &[PeerMembershipObservation],
) -> ParticipantReachability {
    let membership_by_ip: HashMap<_, _> = observations
        .iter()
        .map(|observation| (observation.addr.ip(), observation.state))
        .collect();

    let mut unreachable = Vec::new();
    let mut details = HashMap::new();
    for machine_id in participants {
        let Some(machine) = machine_map.get(machine_id) else {
            continue;
        };
        let Some(membership) = membership_by_ip.get(&IpAddr::V6(machine.overlay_ip.0)) else {
            continue;
        };
        let membership = match membership {
            PeerMembershipState::Alive => "alive",
            PeerMembershipState::Suspect => "suspect",
            PeerMembershipState::Down => "down",
        };
        if membership != "alive" {
            unreachable.push(machine.id.clone());
            details.insert(
                machine.id.clone(),
                ParticipantReachabilityDetail {
                    membership,
                    wg: "unknown",
                    rpc: "not-checked",
                },
            );
        }
    }
    unreachable.sort_by(|left, right| left.0.cmp(&right.0));
    ParticipantReachability {
        unreachable,
        details,
    }
}

pub(super) fn warnings_from_reachability(reachability: &ParticipantReachability) -> Vec<String> {
    reachability
        .unreachable
        .iter()
        .map(|machine_id| {
            let detail = reachability.details.get(machine_id);
            format!(
                "machine '{}' is unreachable (membership={} wg={} rpc={})",
                machine_id,
                detail.map(|detail| detail.membership).unwrap_or("unknown"),
                detail.map(|detail| detail.wg).unwrap_or("unknown"),
                detail.map(|detail| detail.rpc).unwrap_or("not-checked"),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MachineLifecycle, OverlayIp, PublicKey};
    use std::collections::BTreeMap;
    use std::net::{IpAddr, Ipv6Addr, SocketAddr};

    #[test]
    fn reachability_warnings_include_corrosion_context() {
        let machine = test_machine("machine-2", "fd00::2");
        let participants = BTreeSet::from([machine.id.clone()]);
        let machine_map = HashMap::from([(machine.id.clone(), machine.clone())]);
        let observations = vec![PeerMembershipObservation {
            addr: SocketAddr::new(IpAddr::V6(machine.overlay_ip.0), 51001),
            actor_id: String::from("actor-2"),
            state: PeerMembershipState::Suspect,
            timestamp: 123,
        }];

        let reachability =
            reachability_from_membership(&participants, &machine_map, observations.as_slice());
        let warnings = warnings_from_reachability(&reachability);

        assert_eq!(reachability.unreachable, vec![machine.id]);
        assert_eq!(
            warnings,
            vec![
                "machine 'machine-2' is unreachable (membership=suspect wg=unknown rpc=not-checked)"
            ]
        );
    }

    #[test]
    fn reachability_skips_alive_unknown_and_unmapped_membership() {
        let alive = test_machine("machine-1", "fd00::1");
        let unknown = test_machine("machine-2", "fd00::2");
        let participants = BTreeSet::from([alive.id.clone(), unknown.id.clone()]);
        let machine_map = HashMap::from([
            (alive.id.clone(), alive.clone()),
            (unknown.id.clone(), unknown.clone()),
        ]);
        let observations = vec![
            PeerMembershipObservation {
                addr: SocketAddr::new(IpAddr::V6(alive.overlay_ip.0), 51001),
                actor_id: String::from("actor-1"),
                state: PeerMembershipState::Alive,
                timestamp: 123,
            },
            PeerMembershipObservation {
                addr: "[fd00::99]:51001".parse().expect("valid addr"),
                actor_id: String::from("actor-99"),
                state: PeerMembershipState::Down,
                timestamp: 124,
            },
        ];

        let reachability =
            reachability_from_membership(&participants, &machine_map, observations.as_slice());

        assert!(reachability.unreachable.is_empty());
    }

    fn test_machine(id: &str, overlay_ip: &str) -> MachineRecord {
        MachineRecord {
            id: MachineId(id.into()),
            public_key: PublicKey([7; 32]),
            overlay_ip: OverlayIp(overlay_ip.parse::<Ipv6Addr>().expect("valid overlay ip")),
            control_target: None,
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: MachineLifecycle::Active,
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }
}
