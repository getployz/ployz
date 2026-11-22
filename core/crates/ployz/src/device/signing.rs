//! The one place a key this CLI holds becomes a signature: this computer's Device key, or
//! a CI or agent key from `PLOYZ_KEY`. It loads the key and signs. The key never leaves
//! this type.

use std::path::Path;

use ployz_core::authority::{Signed, Signer, Statement};

use super::key::{self, KeyError};
use crate::failure::Failure as Error;

/// A key this CLI signs with, ready to sign.
pub(crate) struct DeviceSigning {
    signer: Signer,
}

impl DeviceSigning {
    /// The key at `path`, or `None` when this computer holds none.
    ///
    /// # Errors
    /// Returns the key file's error when it can't be read, is readable by other users,
    /// or isn't a key.
    pub(crate) fn load(path: &Path) -> Result<Option<Self>, KeyError> {
        Ok(key::load(path)?.map(|secret| Self {
            signer: Signer::from_seed(&secret),
        }))
    }

    /// The key whose 32-byte seed is `seed`.
    pub(crate) fn from_seed(seed: &[u8; 32]) -> Self {
        Self {
            signer: Signer::from_seed(seed),
        }
    }

    /// The key a body names as its signer.
    pub(crate) fn device_key(&self) -> ployz_core::DeviceKey {
        self.signer.public().into()
    }

    /// Sign `body` with this key.
    ///
    /// # Errors
    /// Returns a command error when the body has no canonical form.
    pub(crate) fn sign<T: Statement>(&self, body: &T) -> Result<Signed<T>, Error> {
        Signed::sign(body, &self.signer).map_err(Error::command)
    }
}

#[cfg(test)]
#[path = "signing_tests.rs"]
mod tests;
