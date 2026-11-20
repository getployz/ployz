//! The one place this computer's Device key becomes a signature: it loads the key and
//! signs. The key never leaves this type.

use std::path::Path;

use ployz_core::authority::{Signed, Signer, Statement};

use super::key::{self, KeyError};
use crate::handlers::Error;

/// This computer's Device key, ready to sign.
pub(in crate::handlers) struct DeviceSigning {
    signer: Signer,
}

impl DeviceSigning {
    /// The key at `path`, or `None` when this computer holds none.
    ///
    /// # Errors
    /// Returns the key file's error when it can't be read, is readable by other users,
    /// or isn't a key.
    pub(in crate::handlers) fn load(path: &Path) -> Result<Option<Self>, KeyError> {
        Ok(key::load(path)?.map(|secret| Self {
            signer: Signer::from_seed(&secret),
        }))
    }

    /// The key a body names as its signer.
    pub(in crate::handlers) fn device_key(&self) -> ployz_core::DeviceKey {
        self.signer.public().into()
    }

    /// Sign `body` with this Device's key.
    ///
    /// # Errors
    /// Returns a command error when the body has no canonical form.
    pub(in crate::handlers) fn sign<T: Statement>(&self, body: &T) -> Result<Signed<T>, Error> {
        Signed::sign(body, &self.signer).map_err(Error::command)
    }
}

#[cfg(test)]
#[path = "signing_tests.rs"]
mod tests;
