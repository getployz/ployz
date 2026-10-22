//! Backing up the Cluster's Sealing Key under a recovery code, and recovering it
//! onto this Machine after every holder is gone.
//!
//! The code never comes through the Ployz Cloud management tunnel: Cloud stores
//! the backup, so a Machine refuses Wrap and Recover from a Cloud-routed caller.

use std::io;

use ployz_core::{
    EnrollForRecoveryRequest, Enrolled, LocalMachinePhase, RecoverSealingKeyRequest,
    SealingKeyRecovered, SealingKeyWrapped, SealingPublicKey, WrapSealingKeyRequest,
};
use thiserror::Error as ThisError;

use super::LocalMachine;
use crate::{
    sealing_key::SealingKey,
    sealing_recovery::{RestoreError, back_up, restore},
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
        let local = self.clone();
        self.finish_mutation(async move { local.enroll_admitted(enroll, Some(recovery)).await })
            .await
    }

    /// Wrap this Machine's Sealing Key under the caller's recovery code.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CloudRouted`] through the management tunnel and
    /// [`Error::NotHeld`] without a key.
    pub(crate) fn wrap_sealing_key(
        &self,
        WrapSealingKeyRequest { code }: WrapSealingKeyRequest,
        cloud_routed: bool,
    ) -> Result<SealingKeyWrapped, super::Error> {
        if cloud_routed {
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
    /// names, then publish its public half.
    ///
    /// The code is checked even when this Machine already holds that key, so a
    /// caller learns a wrong code before it needs one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CloudRouted`] through the management tunnel,
    /// [`super::Error::NotParticipating`] before participation,
    /// [`Error::NothingAwaited`] or [`Error::NoBackup`] without a key to recover or
    /// a backup to open, a restore refusal, and [`Error::HeldAnother`] when this
    /// Machine holds a different key.
    pub(crate) async fn recover_sealing_key(
        &self,
        request: RecoverSealingKeyRequest,
        cloud_routed: bool,
    ) -> Result<SealingKeyRecovered, super::Error> {
        if cloud_routed {
            return Err(Error::CloudRouted.into());
        }
        let local = self.clone();
        self.finish_mutation(async move { local.recover_admitted(request).await })
            .await
    }

    async fn recover_admitted(
        &self,
        RecoverSealingKeyRequest { code, backup }: RecoverSealingKeyRequest,
    ) -> Result<SealingKeyRecovered, super::Error> {
        let record = self.record();
        if record.phase() != LocalMachinePhase::Participating {
            return Err(super::Error::NotParticipating);
        }
        let replicated = self.replicated()?;
        let expected = match replicated.sealing_public_key().await? {
            Some(published) => published,
            None => replicated
                .sealing_awaits_recovery()
                .await?
                .ok_or(Error::NothingAwaited)?,
        };
        let backup = backup
            .or_else(|| record.recovery_seed().and_then(|seed| seed.backup.clone()))
            .ok_or(Error::NoBackup)?;
        let key = restore(&backup, &code, &expected).map_err(|error| match error {
            RestoreError::Malformed => Error::Malformed,
            RestoreError::OtherKey => Error::OtherKey(expected),
            RestoreError::WrongCode => Error::WrongCode,
        })?;
        let data_dir = self.owner.data_dir();
        match SealingKey::load(data_dir).map_err(Error::Unreadable)? {
            Some(held) if held.public_key() != expected => {
                return Err(Error::HeldAnother {
                    held: held.public_key(),
                    recovered: expected,
                }
                .into());
            }
            Some(_) => {}
            None => SealingKey::set(data_dir, Some(&key)).map_err(Error::Unwritable)?,
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
