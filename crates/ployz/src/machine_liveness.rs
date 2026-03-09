use crate::model::{MachineRecord, MachineStatus};

pub(crate) const STALE_HEARTBEAT_SECS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MachineLiveness {
    Fresh,
    Stale,
    Down,
}

pub(crate) fn machine_liveness(machine: &MachineRecord, now: u64) -> MachineLiveness {
    if machine.status == MachineStatus::Down {
        return MachineLiveness::Down;
    }

    if machine.last_heartbeat == 0
        || now.saturating_sub(machine.last_heartbeat) > STALE_HEARTBEAT_SECS
    {
        return MachineLiveness::Stale;
    }

    MachineLiveness::Fresh
}

pub(crate) fn machine_is_fresh(machine: &MachineRecord, now: u64) -> bool {
    machine_liveness(machine, now) == MachineLiveness::Fresh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MachineId, OverlayIp, Participation, PublicKey};
    use std::net::Ipv6Addr;

    fn test_machine(status: MachineStatus, last_heartbeat: u64) -> MachineRecord {
        MachineRecord {
            id: MachineId("machine-1".into()),
            public_key: PublicKey([7; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            subnet: None,
            bridge_ip: None,
            endpoints: vec![],
            status,
            participation: Participation::Disabled,
            last_heartbeat,
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn machine_liveness_marks_down_status_as_down() {
        let machine = test_machine(MachineStatus::Down, STALE_HEARTBEAT_SECS);

        assert_eq!(
            machine_liveness(&machine, STALE_HEARTBEAT_SECS),
            MachineLiveness::Down
        );
    }

    #[test]
    fn machine_liveness_marks_missing_heartbeat_as_stale() {
        let machine = test_machine(MachineStatus::Unknown, 0);

        assert_eq!(machine_liveness(&machine, 100), MachineLiveness::Stale);
    }

    #[test]
    fn machine_liveness_marks_recent_heartbeat_as_fresh() {
        let machine = test_machine(MachineStatus::Up, 90);

        assert_eq!(machine_liveness(&machine, 100), MachineLiveness::Fresh);
    }
}
