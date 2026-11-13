//! The Sealing Key wrapped under a recovery code, so a Cluster that lost every
//! holder can open its sealed values again.

use ployz_core::{
    RecoveryCode, SealingKeyBackup, SealingPublicKey,
    sealing_backup::{self, BackupError},
};
use thiserror::Error;

use crate::sealing_key::SealingKey;

#[derive(Debug, Eq, Error, PartialEq)]
pub(crate) enum RestoreError {
    #[error("the recovery backup is malformed")]
    Malformed,
    #[error("the recovery backup belongs to a different Sealing Key")]
    OtherKey,
    #[error("the recovery code does not open this backup")]
    WrongCode,
}

pub(crate) fn back_up(key: &SealingKey, code: &RecoveryCode) -> SealingKeyBackup {
    sealing_backup::back_up(key.secret(), code)
}

/// Opens `backup` with `code`, only if it holds the key behind `expected`.
pub(crate) fn restore(
    backup: &SealingKeyBackup,
    code: &RecoveryCode,
    expected: &SealingPublicKey,
) -> Result<SealingKey, RestoreError> {
    let key = sealing_backup::open(backup, code)
        .map(|secret| SealingKey::from_secret(*secret))
        .map_err(|error| match error {
            BackupError::Malformed => RestoreError::Malformed,
            BackupError::WrongCode => RestoreError::WrongCode,
        })?;
    if key.public_key() == *expected {
        Ok(key)
    } else {
        Err(RestoreError::OtherKey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_restores_with_its_code() {
        let key = SealingKey::generate();
        let public_key = key.public_key();
        let code = RecoveryCode::generate();
        let backup = back_up(&key, &code);

        assert_eq!(
            restore(&backup, &RecoveryCode::generate(), &public_key),
            Err(RestoreError::WrongCode)
        );
        assert_eq!(restore(&backup, &code, &public_key), Ok(key));
    }

    #[test]
    fn backup_of_another_key_is_refused() {
        let code = RecoveryCode::generate();
        let backup = back_up(&SealingKey::generate(), &code);

        assert_eq!(
            restore(&backup, &code, &SealingKey::generate().public_key()),
            Err(RestoreError::OtherKey)
        );
    }

    #[test]
    fn malformed_backup_is_refused() {
        let key = SealingKey::generate();

        assert_eq!(
            restore(
                &SealingKeyBackup::from_bytes(Vec::new()),
                &RecoveryCode::generate(),
                &key.public_key()
            ),
            Err(RestoreError::Malformed)
        );
    }
}
