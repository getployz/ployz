use crate::machine_reachability::{ReachabilityStatus, probe_overlay_ips};
use crate::model::{MachineId, MachineRecord};
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParticipantReachability {
    pub(super) unreachable: Vec<MachineId>,
}

pub(super) async fn probe_participants(
    participants: &BTreeSet<MachineId>,
    machine_map: &HashMap<MachineId, MachineRecord>,
) -> ParticipantReachability {
    let participant_machines: Vec<_> = participants
        .iter()
        .filter_map(|machine_id| machine_map.get(machine_id))
        .collect();
    let overlay_ips: Vec<_> = participant_machines
        .iter()
        .map(|machine| machine.overlay_ip)
        .collect();
    let results = probe_overlay_ips(&overlay_ips).await;

    let mut unreachable = Vec::new();
    for machine in participant_machines {
        if results
            .get(&machine.overlay_ip)
            .is_some_and(|result| result.status == ReachabilityStatus::Unreachable)
        {
            unreachable.push(machine.id.clone());
        }
    }
    unreachable.sort_by(|left, right| left.0.cmp(&right.0));
    ParticipantReachability { unreachable }
}

pub(super) fn warnings_from_reachability(reachability: &ParticipantReachability) -> Vec<String> {
    reachability
        .unreachable
        .iter()
        .map(|machine_id| format!("machine '{}' is unreachable", machine_id))
        .collect()
}
