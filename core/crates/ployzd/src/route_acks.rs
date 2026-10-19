//! Which Machines' Ingress Proxies must ack a route change, sampled from the
//! replicated Machine rows and Corrosion membership.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    time::Duration,
};

use ployz_core::{
    ContainerId, Machine, MachineId, MachineName, ManagementAddress, MembershipEvidence,
    QualifiedService,
};

use crate::corrosion::{
    AdminClient, Error as CorrosionError, IngressRouteRow, MachineView, ReplicatedObservations,
    membership_states_by_address,
};

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
    let states = sample_states(admin)
        .await
        .inspect_err(|_| tracing::warn!("membership sample failed; every Machine must ack"))
        .ok();
    required_ackers(machines, local, states.as_ref())
}

/// The Machines a proxy drops from its routes, the same Down set `ReadRoutes`
/// and the LastIn guard read.
pub(crate) async fn load_down(
    machines: &MachineView,
    admin: &AdminClient,
    local: &MachineId,
) -> Result<BTreeSet<MachineId>, CorrosionError> {
    let machines = machines.current().ok_or_else(|| {
        CorrosionError::Io(io::Error::new(
            io::ErrorKind::NotConnected,
            "the Machine view has not observed the store yet",
        ))
    })?;
    let states = sample_states(admin).await?;
    Ok(machines
        .observations
        .iter()
        .filter(|machine| reported_down(machine, local, Some(&states)))
        .map(|machine| machine.id)
        .collect())
}

async fn sample_states(
    admin: &AdminClient,
) -> Result<BTreeMap<ManagementAddress, MembershipEvidence>, CorrosionError> {
    let states = tokio::time::timeout(MEMBERSHIP_SAMPLE_TIMEOUT, admin.membership_states())
        .await
        .map_err(|_| {
            CorrosionError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "membership sample timed out",
            ))
        })??;
    Ok(membership_states_by_address(states))
}

/// The Down set a proxy routes by, from the last sample that loaded. Nothing
/// is Down before the first one, and the local Machine never is.
pub(crate) struct DownMachines {
    local_id: MachineId,
    down: Option<BTreeSet<MachineId>>,
}

impl DownMachines {
    pub(crate) fn new(local_id: MachineId) -> Self {
        Self {
            local_id,
            down: None,
        }
    }

    pub(crate) fn local_id(&self) -> MachineId {
        self.local_id
    }

    pub(crate) fn contains(&self, machine: &MachineId) -> bool {
        *machine != self.local_id
            && self
                .down
                .as_ref()
                .is_some_and(|down| down.contains(machine))
    }

    /// Take one sample, keeping the last loaded set when it failed. True when
    /// the set changed.
    pub(crate) fn update(&mut self, loaded: Result<BTreeSet<MachineId>, CorrosionError>) -> bool {
        match loaded {
            Ok(down) if self.down.as_ref() == Some(&down) => false,
            Ok(down) => {
                self.down = Some(down);
                true
            }
            Err(error) => {
                let fallback = if self.down.is_some() {
                    "keeping the last successful filter"
                } else {
                    "routing every Machine"
                };
                eprintln!(
                    "failed to load membership for routing; {fallback}: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
                false
            }
        }
    }
}

pub(crate) fn required_ackers(
    machines: ReplicatedObservations<Machine, MachineId>,
    local: &MachineId,
    states: Option<&BTreeMap<ManagementAddress, MembershipEvidence>>,
) -> Membership {
    let (down, up): (Vec<Machine>, Vec<Machine>) = machines
        .observations
        .into_iter()
        .partition(|machine| reported_down(machine, local, states));
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

/// Only explicit Down evidence counts. A Machine with no evidence is unknown,
/// not Down, and the local Machine never is.
fn reported_down(
    machine: &Machine,
    local: &MachineId,
    states: Option<&BTreeMap<ManagementAddress, MembershipEvidence>>,
) -> bool {
    &machine.id != local
        && states.is_some_and(|states| {
            states.get(&machine.management_address()) == Some(&MembershipEvidence::Down)
        })
}

/// Every row whose containers arrived counts, whatever its deploy column holds.
pub(crate) fn routed(
    rows: BTreeMap<(MachineId, QualifiedService), IngressRouteRow>,
    service: &QualifiedService,
) -> BTreeMap<MachineId, BTreeSet<ContainerId>> {
    rows.into_iter()
        .filter(|((_, routed), _)| routed == service)
        .filter_map(|((machine, _), row)| match row {
            IngressRouteRow::Acked(ack) => Some((machine, ack.containers)),
            IngressRouteRow::UnknownDeploy(containers) => Some((machine, containers)),
            IngressRouteRow::UnknownContainers => None,
        })
        .collect()
}

#[cfg(test)]
#[path = "route_acks_tests.rs"]
mod tests;
