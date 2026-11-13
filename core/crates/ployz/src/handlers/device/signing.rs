//! The one place this computer's Device key becomes a signature: it loads the key,
//! opens the window an approval lasts, and signs. The key never leaves this type.

use std::path::Path;

use ployz_core::authority::{Signed, Signer, SignerKey, Statement, Timestamp, validity};

use super::key::{self, KeyError};
use crate::handlers::Error;

/// This computer's Device key, ready to sign.
pub(in crate::handlers) struct DeviceSigning {
    signer: Signer,
}

/// How long a Device approval lasts and which key it names as its signer: the three
/// fields every approval body carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::handlers) struct ApprovalWindow {
    pub(in crate::handlers) issued_at: Timestamp,
    pub(in crate::handlers) expires_at: Timestamp,
    pub(in crate::handlers) signer: SignerKey,
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

    /// A window that opens now and lasts the longest a Machine takes.
    pub(in crate::handlers) fn window(&self) -> ApprovalWindow {
        self.window_from(Timestamp::now())
    }

    pub(in crate::handlers) fn window_from(&self, issued_at: Timestamp) -> ApprovalWindow {
        ApprovalWindow {
            issued_at,
            expires_at: issued_at.plus(validity::MAX_LIFETIME_MILLIS),
            signer: SignerKey::Device(self.signer.public().into()),
        }
    }

    /// The key a body names as its signer when it holds no window.
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
