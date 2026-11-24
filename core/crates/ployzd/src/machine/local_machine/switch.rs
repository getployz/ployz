//! The switch lock: one per Machine, held by `SetVolumeCopy`, `Volume.Seed` and
//! `SetVolumeSize` through each switch, and waited out by every create or start of a
//! Provisioned Volume's holder. A create reads each Volume's origin, and no promote or
//! seed may land between that read and Docker.
//!
//! Every change to the keys this Machine trusts takes it too, through
//! [`LocalMachine::finish_key_change`], whose [`KeyChangeHeld`] the store's writes that
//! remove or narrow a key require, so a key change lands only between switch steps:
//! once `ployz device rm` returns, no step a removed caller asked for is still running,
//! and every later one checks the keys again under the lock. The order is the switch, then
//! an admission turn, then the installation guard: switch work takes its turns inside the
//! switch, and nothing holding a turn asks for the switch. A key change takes the floor
//! lock last, which a floor raise takes alone, so it may fence a cut mid-drain.
//!
//! Taking it checks the caller's access under it, as an admission turn does: a request
//! queued behind a long cut runs nothing for a caller removed while it waited, and acts
//! at the scope that check found.

use std::sync::Arc;

use tokio::sync::OwnedMutexGuard;

use super::{Checked, Error, LocalMachine, turns::Admission};

/// The switch lock, and this Machine serving the call inside it at the access its caller
/// holds under it. Only [`LocalMachine::lock_switch`] makes one, so a Volume step that
/// takes one never runs for a caller removed while it queued.
pub(crate) struct SwitchGuard {
    local: Checked,
    _held: OwnedMutexGuard<()>,
}

impl std::ops::Deref for SwitchGuard {
    type Target = Checked;

    fn deref(&self) -> &Checked {
        &self.local
    }
}

/// Whether a handle serves work inside a switch whose guard its caller holds, so it takes
/// no switch lock of its own. Only [`LocalMachine::lock_switch`] sets it, on the handle
/// inside the guard, and a clone never carries it, so a handle that outlives its guard
/// waits out the switch like any other.
#[derive(Default)]
pub(super) struct InSwitch(bool);

impl InSwitch {
    pub(super) fn held(&self) -> bool {
        self.0
    }
}

impl Clone for InSwitch {
    fn clone(&self) -> Self {
        Self(false)
    }
}

/// Proof that a write holds the switch, the exclusive admission turn and the floor lock,
/// which only [`LocalMachine::finish_key_change`] makes. Every store write that stops
/// trusting a key or narrows one takes it, so none lands while a switch step or a floor
/// raise a removed key asked for still runs.
pub(crate) struct KeyChangeHeld(());

#[cfg(test)]
impl KeyChangeHeld {
    /// A key change a test writes to the store directly, holding none of the locks, to
    /// land it at a moment the test picks.
    pub(crate) fn for_test() -> Self {
        Self(())
    }
}

impl LocalMachine {
    /// Wait out any switch on this Machine and hold it off until the guard drops, with the
    /// caller's access checked once the lock is held. Work inside the guard creates and
    /// starts a Provisioned Volume's holder without taking the lock again.
    ///
    /// # Errors
    /// Returns [`Error::ManagementRevoked`] when this Machine no longer accepts the
    /// caller's key.
    pub(crate) async fn lock_switch(&self) -> Result<SwitchGuard, Error> {
        let held = self.take_switch().await;
        let inside = Self {
            in_switch: InSwitch(true),
            ..self.clone()
        };
        Ok(SwitchGuard {
            local: inside.checked_under(&held)?,
            _held: held,
        })
    }

    /// Change the keys this Machine trusts alone, as [`Self::finish_mutation`] runs work,
    /// holding the switch from before the turn, and the floor lock after it, until the
    /// change is written. A switch step or a floor raise under way ends first, so a removal
    /// returns only once no step its caller asked for still runs. Once the change is
    /// written, the work a key it left short of Full scope started ends, and the change
    /// returns only once that work has stopped: an exec, a tunnel or a Build closes, and a
    /// Build Grant serves nothing more.
    pub(super) async fn finish_key_change<T, F, Fut>(&self, work: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(Admission, KeyChangeHeld) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, Error>> + Send + 'static,
    {
        let switch = self.take_switch().await;
        self.finish_mutation(move |local| async move {
            let _switch = switch;
            let containers = local.containers().cloned();
            #[cfg(test)]
            local.floor_asked.send_modify(|asked| *asked += 1);
            let _floor = match &containers {
                Some(containers) => Some(containers.lifecycle().floor_turn().await),
                None => None,
            };
            let bound = local.bound.clone();
            let changed = work(local, KeyChangeHeld(())).await;
            bound.end_unheld().await;
            changed
        })
        .await
    }

    /// The switch lock itself, checking nothing.
    async fn take_switch(&self) -> OwnedMutexGuard<()> {
        #[cfg(test)]
        self.switch_asked.send_modify(|asked| *asked += 1);
        Arc::clone(&self.switch).lock_owned().await
    }

    /// How many calls have asked for the switch lock so far.
    #[cfg(test)]
    pub(crate) fn switch_asked(&self) -> tokio::sync::watch::Receiver<usize> {
        self.switch_asked.subscribe()
    }

    /// How many floor raises and key changes have asked for the floor lock so far.
    #[cfg(test)]
    pub(crate) fn floor_asked(&self) -> tokio::sync::watch::Receiver<usize> {
        self.floor_asked.subscribe()
    }
}
