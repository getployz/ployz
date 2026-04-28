use crate::model::{MachineId, MachineLifecycle, MachineMembership, PlacementCandidate, RegionName};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticRole {
    Blocking,
    Informational,
}

#[must_use]
pub fn is_new_placement_candidate(machine: &PlacementCandidate) -> bool {
    machine.lifecycle == MachineLifecycle::Active
}

#[must_use]
pub fn machine_region(machine: &MachineMembership) -> &RegionName {
    &machine.topology.region
}

#[must_use]
pub fn same_region(left: &MachineMembership, right: &MachineMembership) -> bool {
    left.topology.region == right.topology.region
}

#[must_use]
pub fn same_availability_zone(left: &MachineMembership, right: &MachineMembership) -> bool {
    left.topology.availability_zone.is_some()
        && left.topology.availability_zone == right.topology.availability_zone
}

#[must_use]
pub fn can_keep_existing_slot(machine: &PlacementCandidate) -> bool {
    matches!(
        machine.lifecycle,
        MachineLifecycle::Active | MachineLifecycle::Draining
    )
}

#[must_use]
pub fn is_coordination_peer(machine: &PlacementCandidate, self_id: &MachineId) -> bool {
    machine.id != *self_id
        && matches!(
            machine.lifecycle,
            MachineLifecycle::Active | MachineLifecycle::Draining
        )
}

#[must_use]
pub fn coordination_peers<'a>(
    machines: &'a [PlacementCandidate],
    self_id: &MachineId,
) -> Vec<&'a PlacementCandidate> {
    machines
        .iter()
        .filter(|machine| is_coordination_peer(machine, self_id))
        .collect()
}

#[must_use]
pub fn diagnostic_role(
    machine: &PlacementCandidate,
    local_machine_id: &MachineId,
) -> Option<DiagnosticRole> {
    if machine.id == *local_machine_id {
        return None;
    }

    Some(match machine.lifecycle {
        MachineLifecycle::Active | MachineLifecycle::Draining => DiagnosticRole::Blocking,
        MachineLifecycle::Standby => DiagnosticRole::Informational,
    })
}

#[must_use]
pub fn placement_candidates(machines: &[PlacementCandidate]) -> Vec<&PlacementCandidate> {
    machines
        .iter()
        .filter(|machine| is_new_placement_candidate(machine))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn machine(id: &str, lifecycle: MachineLifecycle) -> PlacementCandidate {
        PlacementCandidate {
            id: MachineId(id.into()),
            lifecycle,
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn enabled_is_new_placement_candidate() {
        assert!(is_new_placement_candidate(&machine(
            "enabled",
            MachineLifecycle::Active
        )));
        assert!(!is_new_placement_candidate(&machine(
            "draining",
            MachineLifecycle::Draining
        )));
        assert!(!is_new_placement_candidate(&machine(
            "disabled",
            MachineLifecycle::Standby
        )));
    }

    #[test]
    fn coordination_peers_include_draining_and_exclude_disabled() {
        let machines = vec![
            machine("self", MachineLifecycle::Active),
            machine("enabled", MachineLifecycle::Active),
            machine("draining", MachineLifecycle::Draining),
            machine("disabled", MachineLifecycle::Standby),
        ];

        let peers = coordination_peers(&machines, &MachineId("self".into()));
        let ids: Vec<_> = peers.iter().map(|machine| machine.id.0.as_str()).collect();
        assert_eq!(ids, vec!["enabled", "draining"]);
    }

    #[test]
    fn diagnostic_role_marks_disabled_as_informational() {
        assert_eq!(
            diagnostic_role(
                &machine("enabled", MachineLifecycle::Active),
                &MachineId("self".into())
            ),
            Some(DiagnosticRole::Blocking)
        );
        assert_eq!(
            diagnostic_role(
                &machine("disabled", MachineLifecycle::Standby),
                &MachineId("self".into())
            ),
            Some(DiagnosticRole::Informational)
        );
        assert_eq!(
            diagnostic_role(
                &machine("self", MachineLifecycle::Active),
                &MachineId("self".into())
            ),
            None
        );
    }
}
