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
    origin: Origin,
}

/// Who picked a route's endpoint. A Server reached over a route Cloud handed over doesn't
/// answer for itself whatever the transport: Cloud can name an SSH endpoint of its own.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Origin {
    /// `--connect`, a saved context, or the local socket.
    Chosen,
    /// Cloud's Server Access. The default, so a route answers for its Server only when it
    /// was built as chosen.
    #[default]
    Cloud,
}

impl FullRoutes {
    /// Routes the person chose: `--connect`, a saved context, or the local socket.
    pub(crate) fn chosen(connections: Vec<Connection>, single: bool) -> Self {
        Self {
            connections: Arc::new(connections),
            single,
            origin: Origin::Chosen,
        }
    }

    /// Routes Cloud's Server Access handed over.
    pub(crate) fn from_cloud(connections: Vec<Connection>, single: bool) -> Self {
        Self {
            connections: Arc::new(connections),
            single,
            origin: Origin::Cloud,
        }
    }

    /// These routes, every one of them dialable on its own.
    pub(crate) fn spread(&self) -> Self {
        Self {
            single: false,
            ..self.clone()
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
    /// Whether `machine` answers for itself: reached over SSH or the local socket on a
    /// route the person chose, so what it reports is its own word and not Cloud's.
    pub(crate) fn answers_for_itself(
        &self,
        machine: &MachineId,
        entry: Option<&MachineId>,
        entry_connection: &Connection,
    ) -> bool {
        self.origin == Origin::Chosen && self.reach(machine, false, entry, entry_connection)
    }

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
