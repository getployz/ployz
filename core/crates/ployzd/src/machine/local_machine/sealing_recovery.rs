//! Backing up the Cluster's Sealing Key under a recovery code, and recovering it
//! onto this Machine after every holder is gone.
//!
//! The code never comes through the Ployz Cloud management tunnel: Cloud stores
//! the backup, so a Machine refuses Wrap and Recover from a Cloud-routed caller.

use std::io;

use ployz_core::{
    EnrollForRecoveryRequest, Enrolled, RecoverSealingKeyRequest, RecoveryCode, SealingKeyBackup,
    SealingKeyRecovered, SealingKeyWrapped, SealingPublicKey, WrapSealingKeyRequest,
    sealing_backup::{self, BackupError},
};
use thiserror::Error as ThisError;

use super::LocalMachine;
use crate::{
    corrosion::ClusterSealing,
    sealing_key::{KeyFile, SealingKey},
};

/// Why Wrap or Recover refused. The RPC adapter maps these once.
#[derive(Debug, ThisError)]
pub enum Error {
    #[error("a recovery code is accepted only over a direct connection to this Machine")]
    CloudRouted,
    #[error("this Machine's Sealing Key could not be read")]
    Unreadable(#[source] io::Error),
    #[error("this Machine's Sealing Key could not be written")]
    Unwritable(#[source] io::Error),
    #[error("this Machine holds no Sealing Key")]
    NotHeld,
    #[error("the Cluster names no Sealing Key to recover")]
    NothingAwaited,
    #[error("no recovery backup was sent and this Machine was not founded with one")]
    NoBackup,
    #[error("the recovery backup is malformed")]
    Malformed,
    #[error("the recovery backup holds a different Sealing Key than {0}")]
    OtherKey(SealingPublicKey),
    #[error("the recovery code does not open this backup")]
    WrongCode,
    #[error("this Machine holds Sealing public key {held}, not {recovered}")]
    HeldAnother {
        held: SealingPublicKey,
        recovered: SealingPublicKey,
    },
}

/// The Sealing Key wrapped under a recovery code, so a Cluster that lost every holder can
/// open its sealed values again.
pub(crate) fn back_up(key: &SealingKey, code: &RecoveryCode) -> SealingKeyBackup {
    sealing_backup::back_up(key.secret(), code)
}

/// Opens `backup` with `code`, only if it holds the key behind `expected`.
fn restore(
    backup: &SealingKeyBackup,
    code: &RecoveryCode,
    expected: SealingPublicKey,
) -> Result<SealingKey, Error> {
    let key = sealing_backup::open(backup, code)
        .map(|secret| SealingKey::from_secret(*secret))
        .map_err(|error| match error {
            BackupError::Malformed => Error::Malformed,
            BackupError::WrongCode => Error::WrongCode,
        })?;
    if key.public_key() == expected {
        Ok(key)
    } else {
        Err(Error::OtherKey(expected))
    }
}

impl LocalMachine {
    /// Found a Cluster whose Sealing Key comes from a recovery backup.
    ///
    /// # Errors
    ///
    /// Returns as [`LocalMachine::enroll`] does, and refuses a seed on a join.
    pub async fn enroll_for_recovery(
        &self,
        EnrollForRecoveryRequest { enroll, recovery }: EnrollForRecoveryRequest,
    ) -> Result<Enrolled, super::Error> {
        self.finish_mutation(move |local| async move {
            local.enroll_admitted(enroll, Some(recovery)).await
        })
        .await
    }

    /// Wrap this Machine's Sealing Key under the caller's recovery code.
    ///
    /// # Errors
    ///
    /// Returns [`super::Error::NeedsFullScope`] for an Operator, [`Error::CloudRouted`]
    /// through the management tunnel and [`Error::NotHeld`] without a key.
    pub(crate) fn wrap_sealing_key(
        &self,
        WrapSealingKeyRequest { code }: WrapSealingKeyRequest,
    ) -> Result<SealingKeyWrapped, super::Error> {
        if self.full_caller()?.cloud_routed() {
            return Err(Error::CloudRouted.into());
        }
        self.require_management_access()?;
        let key = SealingKey::load(self.owner.data_dir())
            .map_err(Error::Unreadable)?
            .ok_or(Error::NotHeld)?;
        let public_key = key.public_key();
        tracing::info!(
            machine = %self.record().id(),
            %public_key,
            "wrapped the Cluster's Sealing Key under a recovery code"
        );
        Ok(SealingKeyWrapped {
            public_key,
            backup: back_up(&key, &code),
        })
    }

    /// Open a recovery backup with its code and keep the Sealing Key the Cluster
    /// names, then insert its public half as the Cluster's (a no-op when a key is
    /// already published there).
    ///
    /// The code is checked even when this Machine already holds that key, so a
    /// caller learns a wrong code before it needs one. A key file that is not 32
    /// bytes holds no key, so Recover writes the recovered key over it.
    ///
    /// # Errors
    ///
    /// Returns [`super::Error::NeedsFullScope`] for an Operator, [`Error::CloudRouted`]
    /// through the management tunnel, [`super::Error::NotParticipating`] before
    /// participation,
    /// [`Error::NothingAwaited`] or [`Error::NoBackup`] without a key to recover or
    /// a backup to open, a restore refusal, [`Error::HeldAnother`] when this
    /// Machine holds a different key, [`Error::Unreadable`] when its key file
    /// exists but cannot be read, and [`Error::Unwritable`] when the recovered key
    /// cannot be written.
    pub(crate) async fn recover_sealing_key(
        &self,
        request: RecoverSealingKeyRequest,
    ) -> Result<SealingKeyRecovered, super::Error> {
        if self.full_caller()?.cloud_routed() {
            return Err(Error::CloudRouted.into());
        }
        self.finish_mutation(move |local| async move { local.recover_admitted(request).await })
            .await
    }

    /// Recover reads the Cluster's Sealing and opens the backup first, then
    /// answers from this Machine's key file:
    ///
    /// - the recovered key: keeps the file;
    /// - another key: refuses [`Error::HeldAnother`];
    /// - no file, or a file that is not 32 bytes: writes the recovered key over
    ///   the file;
    /// - a file that cannot be read: refuses [`Error::Unreadable`].
    ///
    /// Short of a refusal, it inserts the recovered public key as the Cluster's
    /// (a no-op when a key is already published there) and answers it.
    async fn recover_admitted(
        &self,
        RecoverSealingKeyRequest { code, backup }: RecoverSealingKeyRequest,
    ) -> Result<SealingKeyRecovered, super::Error> {
        let record = self.record();
        record.require_participating()?;
        let replicated = self.replicated()?;
        let expected = match replicated.cluster_sealing(record.recovery_seed()).await? {
            ClusterSealing::Published { key: published, .. } => published,
            // The marker names the awaited key, or, before the publisher
            // replicates it, a founder's seed does.
            ClusterSealing::AwaitsRecovery(awaited) => awaited,
            ClusterSealing::Unsealed => return Err(Error::NothingAwaited.into()),
        };
        let backup = backup
            .or_else(|| record.recovery_seed().and_then(|seed| seed.backup.clone()))
            .ok_or(Error::NoBackup)?;
        let key = restore(&backup, &code, expected)?;
        let data_dir = self.owner.data_dir();
        match SealingKey::read(data_dir).map_err(Error::Unreadable)? {
            KeyFile::Held(held) if held.public_key() != expected => {
                return Err(Error::HeldAnother {
                    held: held.public_key(),
                    recovered: expected,
                }
                .into());
            }
            KeyFile::Held(_) => {}
            file @ (KeyFile::Absent | KeyFile::Unusable) => {
                self.owner
                    .mutate(move |store| store.set_sealing_key(Some(&key)))
                    .await?
                    .map_err(Error::Unwritable)?;
                if file == KeyFile::Unusable {
                    tracing::warn!(
                        machine = %record.id(),
                        public_key = %expected,
                        "replaced a Sealing Key file that was not 32 bytes"
                    );
                }
            }
        }
        replicated.publish_sealing_public_key(expected).await?;
        tracing::info!(
            machine = %record.id(),
            public_key = %expected,
            "recovered the Cluster's Sealing Key"
        );
        Ok(SealingKeyRecovered {
            public_key: expected,
        })
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

        assert!(matches!(
            restore(&backup, &RecoveryCode::generate(), public_key),
            Err(Error::WrongCode)
        ));
        assert_eq!(restore(&backup, &code, public_key).unwrap(), key);
    }

    #[test]
    fn backup_of_another_key_is_refused() {
        let code = RecoveryCode::generate();
        let backup = back_up(&SealingKey::generate(), &code);
        let other = SealingKey::generate().public_key();

        assert!(matches!(
            restore(&backup, &code, other),
            Err(Error::OtherKey(named)) if named == other
        ));
    }

    #[test]
    fn malformed_backup_is_refused() {
        let key = SealingKey::generate();

        assert!(matches!(
            restore(
                &SealingKeyBackup::from_bytes(Vec::new()),
                &RecoveryCode::generate(),
                key.public_key()
            ),
            Err(Error::Malformed)
        ));
    }
}
