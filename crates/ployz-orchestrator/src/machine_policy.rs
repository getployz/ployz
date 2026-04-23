use crate::model::{MachineId, MachineRecord, Participation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticRole {
    Blocking,
    Informational,
}

#[must_use]
pub fn is_new_placement_candidate(machine: &MachineRecord) -> bool {
    machine.participation == Participation::Enabled
}

#[must_use]
pub fn can_keep_existing_slot(machine: &MachineRecord) -> bool {
    match machine.participation {
        Participation::Enabled | Participation::Draining => true,
        Participation::Disabled => false,
    }
}

#[must_use]
pub fn is_coordination_peer(machine: &MachineRecord, self_id: &MachineId) -> bool {
    machine.id != *self_id
        && match machine.participation {
            Participation::Enabled | Participation::Draining => true,
            Participation::Disabled => false,
        }
}

pub fn coordination_peers<'a>(
    machines: &'a [MachineRecord],
    self_id: &MachineId,
) -> Vec<&'a MachineRecord> {
    machines
        .iter()
        .filter(|machine| is_coordination_peer(machine, self_id))
        .collect()
}

#[must_use]
pub fn diagnostic_role(
    machine: &MachineRecord,
    local_machine_id: &MachineId,
) -> Option<DiagnosticRole> {
    if machine.id == *local_machine_id {
        return None;
    }

    Some(match machine.participation {
        Participation::Enabled | Participation::Draining => DiagnosticRole::Blocking,
        Participation::Disabled => DiagnosticRole::Informational,
    })
}

pub fn placement_candidates<'a>(machines: &'a [MachineRecord]) -> Vec<&'a MachineRecord> {
    machines
        .iter()
        .filter(|machine| is_new_placement_candidate(machine))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MachineStatus, OverlayIp, PublicKey};
    use std::collections::BTreeMap;
    use std::net::Ipv6Addr;

    fn machine(id: &str, participation: Participation) -> MachineRecord {
        MachineRecord {
            id: MachineId(id.into()),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            control_target: None,
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            status: MachineStatus::Unknown,
            participation,
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn enabled_is_new_placement_candidate() {
        assert!(is_new_placement_candidate(&machine(
            "enabled",
            Participation::Enabled
        )));
        assert!(!is_new_placement_candidate(&machine(
            "draining",
            Participation::Draining
        )));
        assert!(!is_new_placement_candidate(&machine(
            "disabled",
            Participation::Disabled
        )));
    }

    #[test]
    fn coordination_peers_include_draining_and_exclude_disabled() {
        let machines = vec![
            machine("self", Participation::Enabled),
            machine("enabled", Participation::Enabled),
            machine("draining", Participation::Draining),
            machine("disabled", Participation::Disabled),
        ];

        let peers = coordination_peers(&machines, &MachineId("self".into()));
        let ids: Vec<_> = peers.iter().map(|machine| machine.id.0.as_str()).collect();
        assert_eq!(ids, vec!["enabled", "draining"]);
    }

    #[test]
    fn diagnostic_role_marks_disabled_as_informational() {
        assert_eq!(
            diagnostic_role(
                &machine("enabled", Participation::Enabled),
                &MachineId("self".into())
            ),
            Some(DiagnosticRole::Blocking)
        );
        assert_eq!(
            diagnostic_role(
                &machine("disabled", Participation::Disabled),
                &MachineId("self".into())
            ),
            Some(DiagnosticRole::Informational)
        );
        assert_eq!(
            diagnostic_role(
                &machine("self", Participation::Enabled),
                &MachineId("self".into())
            ),
            None
        );
    }
}
