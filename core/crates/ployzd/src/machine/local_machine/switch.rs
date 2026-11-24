//! The switch lock: one per Machine, held by `SetVolumeCopy`, `Volume.Seed` and
//! `SetVolumeSize` through each switch, and waited out by every create or start of a
//! Provisioned Volume's holder. A create reads each Volume's origin, and no promote or
//! seed may land between that read and Docker.
//!
//! Taking it checks the caller's access under it, as an admission turn does: a request
//! queued behind a long cut runs nothing for a caller removed while it waited, and acts
//! at the scope that check found.

use std::sync::Arc;

use tokio::sync::OwnedMutexGuard;

use super::{Error, LocalMachine};

/// The switch lock, and this Machine serving the call inside it at the access its caller
/// holds under it. Only [`LocalMachine::lock_switch`] makes one, so a Volume step that
/// takes one never runs for a caller removed while it queued.
pub(crate) struct SwitchGuard {
    local: LocalMachine,
    _held: OwnedMutexGuard<()>,
}

impl std::ops::Deref for SwitchGuard {
    type Target = LocalMachine;

    fn deref(&self) -> &LocalMachine {
        &self.local
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
        #[cfg(test)]
        self.switch_asked.send_modify(|asked| *asked += 1);
        let held = Arc::clone(&self.switch).lock_owned().await;
        Ok(SwitchGuard {
            local: Self {
                in_switch: true,
                ..self.as_it_stands()?
            },
            _held: held,
        })
    }

    /// How many calls have asked for the switch lock so far.
    #[cfg(test)]
    pub(crate) fn switch_asked(&self) -> tokio::sync::watch::Receiver<usize> {
        self.switch_asked.subscribe()
    }

    /// The switch guard a create or start of a Provisioned Volume's holder takes, or
    /// `None` inside a switch its caller holds.
    ///
    /// # Errors
    /// As [`Self::lock_switch`].
    pub(super) async fn wait_out_switch(&self) -> Result<Option<SwitchGuard>, Error> {
        if self.in_switch {
            return Ok(None);
        }
        self.lock_switch().await.map(Some)
    }
}
