//! The Volume verbs an enforcing Machine refuses from an Operator caller. A Volume
//! approval a Device signs replaces each refusal; until then a Full caller still runs them.

use ployz_core::{
    SEED_REFUSED_BY_ENFORCEMENT, SEED_VOLUME_CAPABILITY, SET_VOLUME_COPY_CAPABILITY,
    VOLUME_START_REFUSED_BY_ENFORCEMENT,
};

use super::{Error, LocalMachine};

impl LocalMachine {
    /// Under Device enforcement an Operator's `SetVolumeCopy` that starts the Writer's
    /// Container is refused before the switch lock. A Create runs a spec the caller
    /// chose. A Kept start names a stopped Container that nothing on this Machine ties
    /// to the Volume or to a stop a person approved.
    ///
    /// # Errors
    /// Returns [`Error::VolumeRefused`] naming `SetVolumeCopy` under enforcement.
    pub(crate) fn admit_operator_volume_start(&self) -> Result<(), Error> {
        self.admit_operator_volume(SET_VOLUME_COPY_CAPABILITY)
    }

    /// Under Device enforcement an Operator's `SeedVolume` is refused before the switch
    /// lock. It writes a copy's data into a Volume name the caller picks.
    ///
    /// # Errors
    /// Returns [`Error::VolumeRefused`] naming `SeedVolume` under enforcement.
    pub(crate) fn admit_operator_seed(&self) -> Result<(), Error> {
        self.admit_operator_volume(SEED_VOLUME_CAPABILITY)
    }

    fn admit_operator_volume(&self, capability: &'static str) -> Result<(), Error> {
        if self.record().device_enforcement() {
            return Err(Error::VolumeRefused { capability });
        }
        Ok(())
    }
}

/// What a refused Volume verb answers, by the capability refused.
pub(super) fn refusal_text(capability: &str) -> &'static str {
    if capability == SEED_VOLUME_CAPABILITY {
        SEED_REFUSED_BY_ENFORCEMENT
    } else {
        VOLUME_START_REFUSED_BY_ENFORCEMENT
    }
}
