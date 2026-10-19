//! Which Machines' Ingress Proxies must ack a route change, sampled from the
//! replicated Machine rows and Corrosion membership.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use ployz_core::{Machine, MachineId, MachineName, ManagementAddress, MembershipEvidence};

use crate::corrosion::{AdminClient, ReplicatedObservations, membership_states_by_address};

const MEMBERSHIP_SAMPLE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Ackers {
    Known(BTreeMap<MachineId, MachineName>),
    Incomplete {
        named: BTreeMap<MachineId, MachineName>,
        unnamed: Vec<MachineId>,
    },
    Unread,
}

impl Ackers {
    pub(crate) fn unacked(&self, acked: impl Fn(&MachineId) -> bool) -> Vec<String> {
        let unacked_names = |machines: &BTreeMap<MachineId, MachineName>| {
            machines
                .iter()
                .filter(|(machine, _)| !acked(machine))
                .map(|(_, name)| name.to_string())
                .collect::<Vec<_>>()
        };
        match self {
            Self::Known(machines) => unacked_names(machines),
            Self::Incomplete { named, unnamed } => {
                let mut unacked = unacked_names(named);
                unacked.extend(
                    unnamed
                        .iter()
                        .filter(|machine| !acked(machine))
                        .map(|machine| machine.as_str().to_owned()),
                );
                unacked
            }
            Self::Unread => vec!["unknown, the Machine list was never read".to_owned()],
        }
    }

    pub(crate) fn lists(&self, machine: &MachineId) -> bool {
        match self {
            Self::Known(named) => named.contains_key(machine),
            Self::Incomplete { named, unnamed } => {
                named.contains_key(machine) || unnamed.contains(machine)
            }
            Self::Unread => false,
        }
    }
}

/// One round's membership: the Machines that must ack, and the Machines the
/// sample reported Down, whose ingress routes no longer count.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Membership {
    pub(crate) ackers: Ackers,
    pub(crate) down: BTreeSet<MachineId>,
}

/// Machines whose ingress must ack: every one not reported Down, or every one
/// when membership cannot be sampled.
pub(crate) async fn sample_ackers(
    machines: ReplicatedObservations<Machine, MachineId>,
    local: &MachineId,
    admin: &AdminClient,
) -> Membership {
    let states =
        match tokio::time::timeout(MEMBERSHIP_SAMPLE_TIMEOUT, admin.membership_states()).await {
            Ok(Ok(states)) => Some(membership_states_by_address(states)),
            _ => {
                tracing::warn!("membership sample failed; every Machine must ack");
                None
            }
        };
    required_ackers(machines, local, states.as_ref())
}

pub(crate) fn required_ackers(
    machines: ReplicatedObservations<Machine, MachineId>,
    local: &MachineId,
    states: Option<&BTreeMap<ManagementAddress, MembershipEvidence>>,
) -> Membership {
    let (down, up): (Vec<Machine>, Vec<Machine>) = machines
        .observations
        .into_iter()
        .partition(|machine| &machine.id != local && reported_down(machine, states));
    let named = up
        .into_iter()
        .map(|machine| (machine.id, machine.name))
        .collect();
    let ackers = if machines.incomplete_ids.is_empty() {
        Ackers::Known(named)
    } else {
        Ackers::Incomplete {
            named,
            unnamed: machines.incomplete_ids,
        }
    };
    Membership {
        ackers,
        down: down.into_iter().map(|machine| machine.id).collect(),
    }
}

fn reported_down(
    machine: &Machine,
    states: Option<&BTreeMap<ManagementAddress, MembershipEvidence>>,
) -> bool {
    states.is_some_and(|states| {
        states.get(&machine.management_address()) == Some(&MembershipEvidence::Down)
    })
}
