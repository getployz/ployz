use crate::model::{MachineEvent, MachineRecord};
use std::collections::BTreeMap;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub(crate) async fn run_subnet_claim_monitor_task(
    snapshot: Vec<MachineRecord>,
    mut events: mpsc::Receiver<MachineEvent>,
    cancel: CancellationToken,
) {
    let mut machines = snapshot
        .into_iter()
        .map(|machine| (machine.id.clone(), machine))
        .collect::<BTreeMap<_, _>>();
    let mut previous_duplicates =
        duplicate_subnet_claims(&machines.values().cloned().collect::<Vec<_>>());
    if !previous_duplicates.is_empty() {
        warn!(
            ?previous_duplicates,
            "duplicate machine subnet claims detected"
        );
    }

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("subnet claim monitor task cancelled");
                break;
            }
            Some(event) = events.recv() => {
                match event {
                    MachineEvent::Added(machine) | MachineEvent::Updated(machine) => {
                        machines.insert(machine.id.clone(), machine);
                    }
                    MachineEvent::Removed(machine) => {
                        machines.remove(&machine.id);
                    }
                }

                let next_duplicates =
                    duplicate_subnet_claims(&machines.values().cloned().collect::<Vec<_>>());
                if next_duplicates != previous_duplicates {
                    if next_duplicates.is_empty() {
                        info!("duplicate machine subnet claims resolved");
                    } else {
                        warn!(
                            ?next_duplicates,
                            "duplicate machine subnet claims detected"
                        );
                    }
                    previous_duplicates = next_duplicates;
                }
            }
        }
    }
}

pub(crate) fn duplicate_subnet_claims(machines: &[MachineRecord]) -> Vec<(String, Vec<String>)> {
    let mut claimants_by_subnet: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for machine in machines {
        let Some(subnet) = machine.subnet else {
            continue;
        };
        claimants_by_subnet
            .entry(subnet.to_string())
            .or_default()
            .push(machine.id.0.clone());
    }

    claimants_by_subnet
        .into_iter()
        .filter_map(|(subnet, mut claimants)| {
            if claimants.len() < 2 {
                return None;
            }
            claimants.sort();
            Some((subnet, claimants))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MachineId, MachineStatus, OverlayIp, Participation, PublicKey};
    use ipnet::Ipv4Net;
    use std::collections::BTreeMap;
    use std::net::Ipv6Addr;

    fn test_machine(id: &str, subnet: Option<Ipv4Net>) -> MachineRecord {
        MachineRecord {
            id: MachineId(id.into()),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            subnet,
            bridge_ip: None,
            endpoints: vec![],
            status: MachineStatus::Unknown,
            participation: Participation::Disabled,
            last_heartbeat: 0,
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn duplicate_subnet_claims_reports_overlaps() {
        let machines = vec![
            test_machine("m1", None),
            test_machine("m2", Some("10.210.2.0/24".parse().expect("valid subnet"))),
            test_machine("m3", Some("10.210.2.0/24".parse().expect("valid subnet"))),
            test_machine("m4", Some("10.210.3.0/24".parse().expect("valid subnet"))),
        ];

        assert_eq!(
            duplicate_subnet_claims(&machines),
            vec![(
                String::from("10.210.2.0/24"),
                vec![String::from("m2"), String::from("m3")]
            )]
        );
    }
}
