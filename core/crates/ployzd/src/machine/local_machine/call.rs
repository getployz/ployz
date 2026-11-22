//! Who one call to this Machine came from and what it may do, decided once at the Machine
//! API's edge and carried on the [`LocalMachine`] that serves it.

use ployz_core::{ClientKey, ManagementScope};

use super::{Error, LocalMachine};

/// Where a call to this Machine came from, as its Machine API decided at the edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Caller {
    /// This Server's own local socket: its terminal, or SSH.
    LocalSocket,
    /// An authenticated management connection: Cloud's or a Device's.
    Management(ClientKey),
    /// A call routed here over the mesh, which carries no key of its own, or the daemon's
    /// own work.
    Routed,
}

/// One call's caller, scope, and whether Cloud routed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Call {
    caller: Caller,
    scope: ManagementScope,
    /// The call entered the mesh through Ployz Cloud on some Machine.
    cloud_routed: bool,
}

impl Call {
    pub(crate) fn new(caller: Caller, scope: ManagementScope, cloud_routed: bool) -> Self {
        Self {
            caller,
            scope,
            cloud_routed,
        }
    }

    /// The daemon's own work, which no caller asked for and which runs at Full access.
    pub(super) fn daemon() -> Self {
        Self::new(Caller::Routed, ManagementScope::Full, false)
    }

    pub(crate) fn caller(self) -> Caller {
        self.caller
    }

    pub(crate) fn cloud_routed(self) -> bool {
        self.cloud_routed
    }
}

/// What one call may do on this Machine. Work that differs by scope matches on this, so
/// only a Full caller's path holds a [`FullCaller`].
pub(crate) enum Scope {
    Full(FullCaller),
    Operator,
}

/// A call at Full access. Only [`LocalMachine::scope`] makes one, from the call a
/// [`LocalMachine`] serves, so an operation that takes one never runs for an Operator.
#[derive(Clone, Copy, Debug)]
pub struct FullCaller(Call);

impl FullCaller {
    pub(crate) fn caller(self) -> Caller {
        self.0.caller
    }

    pub(crate) fn cloud_routed(self) -> bool {
        self.0.cloud_routed
    }
}

impl LocalMachine {
    /// This Machine serving `call`.
    #[must_use]
    pub(crate) fn at(mut self, call: Call) -> Self {
        self.call = call;
        self
    }

    pub(crate) fn call(&self) -> Call {
        self.call
    }

    pub(crate) fn scope(&self) -> Scope {
        match self.call.scope {
            ManagementScope::Full => Scope::Full(FullCaller(self.call)),
            ManagementScope::Operator => Scope::Operator,
        }
    }

    /// The call this Machine serves at Full access, or `None` for an Operator's.
    #[must_use]
    pub fn full(&self) -> Option<FullCaller> {
        match self.scope() {
            Scope::Full(full) => Some(full),
            Scope::Operator => None,
        }
    }

    /// The call this Machine serves, for an operation only a Full caller makes. The Machine
    /// API refuses an Operator's call to its route first.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] for an Operator's call.
    pub(super) fn full_caller(&self) -> Result<FullCaller, Error> {
        self.full().ok_or(Error::NeedsFullScope)
    }

    /// Mark subsequent calls as from the authenticated management client `remote`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_management_client(mut self, remote: ClientKey) -> Self {
        self.call.caller = Caller::Management(remote);
        self
    }

    /// Mark subsequent calls as from this Server's local socket.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn on_local_socket(mut self) -> Self {
        self.call.caller = Caller::LocalSocket;
        self
    }

    /// Mark subsequent calls as an Operator's.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn at_operator_scope(mut self) -> Self {
        self.call.scope = ManagementScope::Operator;
        self
    }
}
