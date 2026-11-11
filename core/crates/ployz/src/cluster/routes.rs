//! The Full-scope connections a Client may dial itself. A Full RPC to a named Machine goes
//! over a connection of Full scope to that Machine, not through the entry: see
//! `Client::direct_client`. The entry stays the last Machine a Device mutation reaches,
//! as the Machine-local authority of each Server asks.

use std::{collections::BTreeSet, sync::Arc};

use ployz_core::MachineId;

use crate::context::{Connection, Transport};

/// What a command that needs Full scope holds: a connection to each Server it may dial, and
/// whether it reaches only the entry that way.
#[derive(Clone, Default)]
pub(crate) struct FullRoutes {
    connections: Arc<Vec<Connection>>,
    /// Whether only the entry Machine is reachable with Full scope, however many
    /// connections name others.
    single: bool,
}

impl FullRoutes {
    pub(crate) fn new(connections: Vec<Connection>, single: bool) -> Self {
        Self {
            connections: Arc::new(connections),
            single,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }

    /// The connections a Full call may dial.
    pub(crate) fn connections(&self) -> &[Connection] {
        &self.connections
    }

    pub(crate) fn single(&self) -> bool {
        self.single
    }

    /// The connection of Full scope that names `machine`.
    pub(super) fn to(&self, machine: &str) -> Option<&Connection> {
        self.connections.iter().find(|connection| {
            connection
                .machine_id()
                .is_some_and(|id| id.as_str() == machine)
        })
    }

    /// The Machines reached with Full scope: the entry and each Machine a connection names.
    /// None at all when no connection is held.
    pub(crate) fn machines(&self, entry: Option<MachineId>) -> BTreeSet<MachineId> {
        if self.connections.is_empty() {
            return BTreeSet::new();
        }
        entry
            .into_iter()
            .chain(
                self.connections
                    .iter()
                    .filter_map(|connection| connection.machine_id().copied()),
            )
            .collect()
    }

    /// Whether `machine` is reached with Full scope: over SSH or the local socket, or over
    /// a Management connection dialled as a Device when `device` says one is held. The entry
    /// is reached over `entry_connection`; any other Machine over its own connection.
    pub(crate) fn reach(
        &self,
        machine: &MachineId,
        device: bool,
        entry: Option<&MachineId>,
        entry_connection: &Connection,
    ) -> bool {
        let full = |connection: &Connection| match connection.transport() {
            Transport::Ssh { .. } | Transport::Unix(_) => true,
            Transport::Management(_) => device,
            Transport::Tcp(_) => false,
        };
        if self.connections.is_empty() {
            return false;
        }
        if entry == Some(machine) {
            return full(entry_connection);
        }
        self.connections
            .iter()
            .any(|connection| connection.machine_id() == Some(machine) && full(connection))
    }
}
