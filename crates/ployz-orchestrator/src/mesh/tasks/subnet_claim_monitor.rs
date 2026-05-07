use crate::error::Result as PloyzResult;
use crate::model::{MachineEvent, MachineMembership};
use std::collections::BTreeMap;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub(crate) async fn run_subnet_claim_monitor_task(
    snapshot: Vec<MachineMembership>,
    mut events: mpsc::Receiver<PloyzResult<MachineEvent>>,
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
            "subnet claim invariant violation detected"
        );
    }

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("subnet claim monitor task cancelled");
                break;
            }
            event = events.recv() => {
                let Some(event) = event else {
                    warn!("subnet claim monitor machine subscription closed");
                    break;
                };
                let event = match event {
                    Ok(event) => event,
                    Err(error) => {
                        warn!(%error, "subnet claim monitor machine subscription failed");
                        break;
                    }
                };
                match event {
                    MachineEvent::Upsert(machine) => {
                        machines.insert(machine.id.clone(), machine);
                    }
                    MachineEvent::Removed { id } => {
                        machines.remove(&id);
                    }
                }

                let next_duplicates =
                    duplicate_subnet_claims(&machines.values().cloned().collect::<Vec<_>>());
                if next_duplicates != previous_duplicates {
                    if next_duplicates.is_empty() {
                        info!("subnet claim invariant restored");
                    } else {
                        warn!(
                            ?next_duplicates,
                            "subnet claim invariant violation detected"
                        );
                    }
                    previous_duplicates = next_duplicates;
                }
            }
        }
    }
}

pub(crate) fn duplicate_subnet_claims(
    machines: &[MachineMembership],
) -> Vec<(String, Vec<String>)> {
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
    use crate::model::{MachineId, MachineLifecycle, MachineTopology, OverlayIp, PublicKey};
    use ipnet::Ipv4Net;
    use std::collections::BTreeMap;
    use std::net::Ipv6Addr;

    fn test_machine(id: &str, subnet: Option<Ipv4Net>) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.into()),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            topology: MachineTopology::local(),
            subnet,
            bridge_ip: None,
            endpoints: vec![],
            lifecycle: MachineLifecycle::Standby,
            storage: true,
            storage_participation: crate::model::StorageParticipation::default_authority(),
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

    #[tokio::test]
    async fn exits_when_machine_subscription_closes() {
        let (event_tx, event_rx) = mpsc::channel(4);
        drop(event_tx);

        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            run_subnet_claim_monitor_task(Vec::new(), event_rx, CancellationToken::new()),
        )
        .await
        .expect("subnet claim monitor should exit when machine subscription closes");
    }

    #[tokio::test]
    async fn exits_when_machine_subscription_reports_failure() {
        let (event_tx, event_rx) = mpsc::channel(4);
        event_tx
            .send(Err(crate::error::Error::operation(
                "test_subscription",
                "closed",
            )))
            .await
            .expect("failure event should send");

        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            run_subnet_claim_monitor_task(Vec::new(), event_rx, CancellationToken::new()),
        )
        .await
        .expect("subnet claim monitor should exit when machine subscription reports failure");
    }
}
