use crate::model::{
    MachineId, MachineLifecycle, MachineMembership, PlacementCandidate, RegionName, RegionRole,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticRole {
    Blocking,
    Informational,
}

#[must_use]
pub fn is_new_placement_candidate(machine: &PlacementCandidate) -> bool {
    machine.lifecycle == MachineLifecycle::Active
        && matches!(
            machine.region_role,
            RegionRole::HomeData | RegionRole::Compute
        )
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
    ) && machine.region_role != RegionRole::Disabled
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
        machine_in_region(id, lifecycle, RegionRole::HomeData)
    }

    fn machine_in_region(
        id: &str,
        lifecycle: MachineLifecycle,
        region_role: RegionRole,
    ) -> PlacementCandidate {
        PlacementCandidate {
            id: MachineId::new(id),
            lifecycle,
            region_role,
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
    fn compute_regions_are_new_placement_candidates() {
        assert!(is_new_placement_candidate(&machine_in_region(
            "home",
            MachineLifecycle::Active,
            RegionRole::HomeData
        )));
        assert!(is_new_placement_candidate(&machine_in_region(
            "compute",
            MachineLifecycle::Active,
            RegionRole::Compute
        )));
    }

    #[test]
    fn disabled_and_draining_regions_do_not_receive_new_placements() {
        let region_draining = machine_in_region(
            "region-draining",
            MachineLifecycle::Active,
            RegionRole::Draining,
        );
        let region_disabled = machine_in_region(
            "region-disabled",
            MachineLifecycle::Active,
            RegionRole::Disabled,
        );

        assert!(!is_new_placement_candidate(&region_draining));
        assert!(!is_new_placement_candidate(&region_disabled));
        assert!(can_keep_existing_slot(&region_draining));
        assert!(!can_keep_existing_slot(&region_disabled));
    }

    #[test]
    fn draining_machines_keep_existing_slots_but_do_not_receive_new_placements() {
        let active = machine("active", MachineLifecycle::Active);
        let draining = machine("draining", MachineLifecycle::Draining);
        let standby = machine("standby", MachineLifecycle::Standby);

        assert!(is_new_placement_candidate(&active));
        assert!(!is_new_placement_candidate(&draining));
        assert!(!is_new_placement_candidate(&standby));

        assert!(can_keep_existing_slot(&active));
        assert!(can_keep_existing_slot(&draining));
        assert!(!can_keep_existing_slot(&standby));

        assert_eq!(
            diagnostic_role(&draining, &MachineId::new("self")),
            Some(DiagnosticRole::Blocking)
        );
    }

    #[test]
    fn coordination_peers_include_draining_and_exclude_disabled() {
        let machines = vec![
            machine("self", MachineLifecycle::Active),
            machine("enabled", MachineLifecycle::Active),
            machine("draining", MachineLifecycle::Draining),
            machine("disabled", MachineLifecycle::Standby),
        ];

        let peers = coordination_peers(&machines, &MachineId::new("self"));
        let ids: Vec<_> = peers.iter().map(|machine| machine.id.as_str()).collect();
        assert_eq!(ids, vec!["enabled", "draining"]);
    }

    #[test]
    fn diagnostic_role_marks_disabled_as_informational() {
        assert_eq!(
            diagnostic_role(
                &machine("enabled", MachineLifecycle::Active),
                &MachineId::new("self")
            ),
            Some(DiagnosticRole::Blocking)
        );
        assert_eq!(
            diagnostic_role(
                &machine("disabled", MachineLifecycle::Standby),
                &MachineId::new("self")
            ),
            Some(DiagnosticRole::Informational)
        );
        assert_eq!(
            diagnostic_role(
                &machine("self", MachineLifecycle::Active),
                &MachineId::new("self")
            ),
            None
        );
    }
}
