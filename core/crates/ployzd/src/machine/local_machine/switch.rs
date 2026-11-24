//! The switch lock: one per Machine, held by `SetVolumeCopy`, `Volume.Seed` and
//! `SetVolumeSize` through each switch, and waited out by every create or start of a
//! Provisioned Volume's holder. A create reads each Volume's origin, and no promote or
//! seed may land between that read and Docker.
//!
//! Every change to the keys this Machine trusts takes it too, through
//! [`LocalMachine::finish_key_change`], so a key change lands only between switch steps:
//! once `ployz device rm` returns, no step a removed caller asked for is still running,
//! and every later one checks the keys again under the lock. The order is the switch, then
//! an admission turn, then the installation guard: switch work takes its turns inside the
//! switch, and nothing holding a turn asks for the switch.
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
    /// holding the switch from before the turn until the change is written. A switch step
    /// under way ends first, so a removal returns only once no step its caller asked for
    /// still runs.
    pub(super) async fn finish_key_change<T, F, Fut>(&self, work: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(Admission) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, Error>> + Send + 'static,
    {
        let switch = self.take_switch().await;
        self.finish_mutation(move |local| async move {
            let _switch = switch;
            work(local).await
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
}
