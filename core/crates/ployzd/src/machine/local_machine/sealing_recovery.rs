//! Backing up the Cluster's Sealing Key under a recovery code, and recovering it
//! onto this Machine after every holder is gone.
//!
//! The code never comes through the Ployz Cloud management tunnel: Cloud stores
//! the backup, so a Machine refuses Wrap and Recover from a Cloud-routed caller.

use std::io;

use ployz_core::{
    EnrollForRecoveryRequest, Enrolled, LocalMachinePhase, RecoverSealingKeyRequest, RecoveryCode,
    SealingKeyBackup, SealingKeyRecovered, SealingKeyRole, SealingKeyWrapped, SealingPublicKey,
    WrapSealingKeyRequest,
    authority::{AuthorityRefusal, SealingRotation, Signed, Timestamp, rotation},
};
use thiserror::Error as ThisError;

use super::LocalMachine;
use crate::{
    corrosion::ClusterSealing,
    sealing_key::{KeyFile, SealingKey},
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
    #[error("this Machine holds no recipient key a rotation named")]
    NoRecipient,
    #[error("the rotation naming the recipient key was refused")]
    RotationRefused(#[source] AuthorityRefusal),
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
        WrapSealingKeyRequest { code, key: role }: WrapSealingKeyRequest,
        cloud_routed: bool,
    ) -> Result<SealingKeyWrapped, super::Error> {
        if cloud_routed {
            return Err(Error::CloudRouted.into());
        }
        self.require_management_access()?;
        let data_dir = self.owner.data_dir();
        let (key, rotation) = match role {
            SealingKeyRole::Identity => (
                SealingKey::load(data_dir)
                    .map_err(Error::Unreadable)?
                    .ok_or(Error::NotHeld)?,
                None,
            ),
            SealingKeyRole::Recipient => {
                let (Some(key), Some(rotation)) = (
                    SealingKey::load_recipient(data_dir).map_err(Error::Unreadable)?,
                    SealingKey::load_recipient_rotation(data_dir).map_err(Error::Unreadable)?,
                ) else {
                    return Err(Error::NoRecipient.into());
                };
                (key, Some(rotation))
            }
        };
        let public_key = key.public_key();
        tracing::info!(
            machine = %self.record().id(),
            %public_key,
            ?role,
            "wrapped a Sealing key of the Cluster under a recovery code"
        );
        Ok(SealingKeyWrapped {
            public_key,
            backup: back_up(&key, &code),
            rotation,
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
    /// Returns [`Error::CloudRouted`] through the management tunnel,
    /// [`super::Error::NotParticipating`] before participation,
    /// [`Error::NothingAwaited`] or [`Error::NoBackup`] without a key to recover or
    /// a backup to open, a restore refusal, [`Error::HeldAnother`] when this
    /// Machine holds a different key, [`Error::Unreadable`] when its key file
    /// exists but cannot be read, and [`Error::Unwritable`] when the recovered key
    /// cannot be written.
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
        RecoverSealingKeyRequest {
            code,
            backup,
            rotation,
        }: RecoverSealingKeyRequest,
    ) -> Result<SealingKeyRecovered, super::Error> {
        if let Some(rotation) = rotation {
            return self.recover_recipient(&code, backup, &rotation);
        }
        let record = self.record();
        if record.phase() != LocalMachinePhase::Participating {
            return Err(super::Error::NotParticipating);
        }
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
        let key = restore(&backup, &code, &expected).map_err(|error| match error {
            RestoreError::Malformed => Error::Malformed,
            RestoreError::OtherKey => Error::OtherKey(expected),
            RestoreError::WrongCode => Error::WrongCode,
        })?;
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

    /// Recover the recipient key `rotation` names. The rotation must verify against this
    /// Machine's trusted keys and name its identity, and a Machine that committed another
    /// activation refuses it. Neither membership nor this activation is required: a Server
    /// that joined later isn't among the Members, and after every holder is gone the
    /// recovering founder never committed it.
    fn recover_recipient(
        &self,
        code: &RecoveryCode,
        backup: Option<SealingKeyBackup>,
        signed: &Signed<SealingRotation>,
    ) -> Result<SealingKeyRecovered, super::Error> {
        let record = self.record();
        if record.phase() != LocalMachinePhase::Participating {
            return Err(super::Error::NotParticipating);
        }
        let data_dir = self.owner.data_dir();
        let identity = SealingKey::load(data_dir)
            .map_err(Error::Unreadable)?
            .ok_or(Error::NotHeld)?
            .public_key();
        // The code is checked first, so a caller trying a code on each backup learns a
        // wrong one before anything about the rotation.
        let named = signed.open().map_err(Error::RotationRefused)?.recipient;
        let backup = backup.ok_or(Error::NoBackup)?;
        let key = restore(&backup, code, &named).map_err(|error| match error {
            RestoreError::Malformed => Error::Malformed,
            RestoreError::OtherKey => Error::OtherKey(named),
            RestoreError::WrongCode => Error::WrongCode,
        })?;
        let (body, approver) =
            rotation::verify_for_join(signed, &record.trusted_keys(), identity, Timestamp::now())
                .map_err(Error::RotationRefused)?;
        if record
            .activation()
            .is_some_and(|activation| activation != body.operation)
        {
            return Err(Error::RotationRefused(AuthorityRefusal::Mismatch {
                field: "an activation",
            })
            .into());
        }
        match SealingKey::load_recipient(data_dir).map_err(Error::Unreadable)? {
            Some(held) if held.public_key() != body.recipient => {
                return Err(Error::HeldAnother {
                    held: held.public_key(),
                    recovered: body.recipient,
                }
                .into());
            }
            Some(_) => {}
            None => {
                SealingKey::set_recipient_rotation(data_dir, signed).map_err(Error::Unwritable)?;
                SealingKey::set_recipient(data_dir, &key).map_err(Error::Unwritable)?;
            }
        }
        tracing::info!(
            machine = %record.id(),
            recipient = %body.recipient,
            %approver,
            "recovered the Cluster's recipient key"
        );
        Ok(SealingKeyRecovered {
            public_key: body.recipient,
        })
    }
}
