//! Who one call to this Machine came from and the scope it claims, decided once at the
//! Machine API's edge and carried on the [`LocalMachine`] that serves it.
//!
//! Nothing reads that claim as it arrived. Work that acts learns its scope from a
//! [`Checked`] handle, which an admission turn or the switch makes by checking the claim
//! against the keys as they stand while it holds a lock every key change takes. A read,
//! and an admission that [`super::AdmittedSpec::still_holds`] checks again before Docker
//! acts, asks [`LocalMachine::scope_now`]. Work that outlives its call binds to its key
//! through [`LocalMachine::bind_full`], and ends with that key's Full scope.

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

/// One call's caller, the scope it claims, and whether Cloud routed it.
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

    fn scope(self) -> Scope {
        match self.scope {
            ManagementScope::Full => Scope::Full(FullCaller(self)),
            ManagementScope::Operator => Scope::Operator,
        }
    }
}

/// What one call may do on this Machine. Work that differs by scope matches on this, so
/// only a Full caller's path holds a [`FullCaller`].
#[derive(Clone, Copy, Debug)]
pub(crate) enum Scope {
    Full(FullCaller),
    Operator,
}

impl Scope {
    /// The Full caller, or [`Error::NeedsFullScope`] for an Operator.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] for an Operator.
    pub(crate) fn full(self) -> Result<FullCaller, Error> {
        match self {
            Self::Full(full) => Ok(full),
            Self::Operator => Err(Error::NeedsFullScope),
        }
    }
}

/// A call at Full access. Only a check of the caller against the keys as they stand
/// makes one, so an operation that takes one never runs for an Operator.
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

/// This Machine serving its call at the scope its caller held when a lock every key change
/// takes was held. Only the admission turns and the switch make one, each holding its lock
/// for as long as the handle lives, so its scope holds as long as it does. It is not
/// `Clone`: a clone of the [`LocalMachine`] inside reads no scope.
pub(crate) struct Checked(LocalMachine);

impl std::ops::Deref for Checked {
    type Target = LocalMachine;

    fn deref(&self) -> &LocalMachine {
        &self.0
    }
}

impl Checked {
    pub(crate) fn scope(&self) -> Scope {
        self.0.call.scope()
    }

    /// The Full caller this handle serves. The Machine API refuses an Operator's call to a
    /// route that needs one first, so this refuses only a caller downgraded since.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] for an Operator.
    pub(crate) fn full_caller(&self) -> Result<FullCaller, Error> {
        self.scope().full()
    }
}

/// A lock every key change takes: either admission turn, the switch, or the floor lock.
pub(super) trait Held {}

impl Held for tokio::sync::OwnedRwLockWriteGuard<()> {}
impl Held for tokio::sync::OwnedRwLockReadGuard<()> {}
impl Held for tokio::sync::OwnedMutexGuard<()> {}
impl Held for crate::docker::FloorTurn<'_> {}

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

    /// The scope the caller holds as the keys stand now: a management key no slot accepts
    /// any more is refused, and one that lost its Device slot but keeps another holds
    /// Operator scope. A call never widens past the scope it claims.
    fn scope_as_it_stands(&self) -> Result<ManagementScope, Error> {
        let Caller::Management(remote) = self.call.caller else {
            return Ok(self.call.scope);
        };
        let record = self.record();
        if !record.accepts_management_client(&remote) {
            return Err(Error::ManagementRevoked);
        }
        Ok(match (self.call.scope, record.management_scope(&remote)) {
            (ManagementScope::Full, Some(ManagementScope::Full)) => ManagementScope::Full,
            _ => ManagementScope::Operator,
        })
    }

    /// The scope the caller holds now, bound to no lock, so a key change may land right
    /// after. Only a read takes it, or an admission whose kept spec
    /// [`super::AdmittedSpec::still_holds`] checks again under the turn before Docker acts.
    ///
    /// # Errors
    /// Returns [`Error::ManagementRevoked`] when this Machine no longer accepts the
    /// caller's key.
    pub(crate) fn scope_now(&self) -> Result<Scope, Error> {
        Ok(Call {
            scope: self.scope_as_it_stands()?,
            ..self.call
        }
        .scope())
    }

    /// This Machine serving its call at the scope the caller holds now, checked while
    /// `_held` is held, so a key change lands only before the check or after the lock drops.
    ///
    /// # Errors
    /// Returns [`Error::ManagementRevoked`] when this Machine no longer accepts the
    /// caller's key.
    pub(super) fn checked_under(self, _held: &impl Held) -> Result<Checked, Error> {
        let scope = self.scope_as_it_stands()?;
        let call = Call { scope, ..self.call };
        Ok(Checked(self.at(call)))
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
