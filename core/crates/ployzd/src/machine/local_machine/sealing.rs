//! Backfilling the Cluster's Sealing Key onto this Machine, one route at a time.
//!
//! An operator drives these: Generate on one Machine of a Cluster founded before
//! Sealing Keys existed, then Share from a holder and Install on each Machine that
//! still needs access. No route elects anything; each only checks that what it is
//! asked to do agrees with its key file and with what the Cluster published.

use std::io;

use ployz_core::{
    GenerateSealingKeyRequest, InstallSealingKeyRequest, LocalMachinePhase, MachineId,
    SealingKeyHeld, SealingKeyShared, SealingPublicKey, ShareSealingKeyRequest,
};
use thiserror::Error as ThisError;

use super::LocalMachine;
use crate::sealing_key::{OpenError, SealingKey};

/// Why a Sealing Key route refused. The RPC adapter maps these once.
#[derive(Debug, ThisError)]
pub enum Error {
    #[error("this Machine's Sealing Key could not be read")]
    Unreadable(#[source] io::Error),
    #[error("this Machine's Sealing Key could not be written")]
    Unwritable(#[source] io::Error),
    #[error("the Cluster's Sealing public key is {0}")]
    Published(SealingPublicKey),
    #[error("the Cluster awaits recovery of Sealing public key {0}")]
    AwaitsRecovery(SealingPublicKey),
    #[error("this Machine holds no Sealing Key")]
    NotHeld,
    #[error("this Machine holds Sealing public key {held}, not {requested}")]
    HeldAnother {
        held: SealingPublicKey,
        requested: SealingPublicKey,
    },
    #[error("Machine {0} is not in the Cluster")]
    UnknownMachine(MachineId),
    #[error("Machine {0} is not published with that WireGuard key")]
    WireGuardKeyMismatch(MachineId),
    #[error("the sealed Sealing Key does not open as sent by Machine {from}")]
    Unopenable {
        from: MachineId,
        #[source]
        source: OpenError,
    },
    #[error("the sealed Sealing Key is {opened}, not {requested}")]
    SealedAnother {
        opened: SealingPublicKey,
        requested: SealingPublicKey,
    },
}

impl LocalMachine {
    /// Generate the Cluster's Sealing Key on this Machine and publish its public half.
    ///
    /// A Machine that already holds a key answers with that key's public half. One that
    /// holds none generates only when the Cluster has published no key and awaits no
    /// recovery; otherwise the operator should carry the existing key instead.
    ///
    /// # Errors
    ///
    /// Returns [`super::Error::NotParticipating`] before participation, [`Error::Published`]
    /// or [`Error::AwaitsRecovery`] when the Cluster already names a key, and
    /// [`Error::Unreadable`] or [`Error::Unwritable`] when the key file cannot be used.
    pub(crate) async fn generate_sealing_key(
        &self,
        GenerateSealingKeyRequest {}: GenerateSealingKeyRequest,
    ) -> Result<SealingKeyHeld, super::Error> {
        self.require_management_access()?;
        let local = self.clone();
        self.finish_mutation(async move { local.generate_admitted().await })
            .await
    }

    async fn generate_admitted(&self) -> Result<SealingKeyHeld, super::Error> {
        let record = self.record();
        if record.phase() != LocalMachinePhase::Participating {
            return Err(super::Error::NotParticipating);
        }
        let data_dir = self.owner.data_dir();
        if let Some(held) = SealingKey::load(data_dir).map_err(Error::Unreadable)? {
            let public_key = held.public_key();
            tracing::info!(
                machine = %record.id(),
                %public_key,
                "this Machine already holds the Cluster's Sealing Key"
            );
            return Ok(SealingKeyHeld { public_key });
        }
        let replicated = self.replicated()?;
        if let Some(published) = replicated.sealing_public_key().await? {
            return Err(Error::Published(published).into());
        }
        if let Some(awaited) = replicated.sealing_awaits_recovery().await? {
            return Err(Error::AwaitsRecovery(awaited).into());
        }
        let key = SealingKey::generate();
        let public_key = key.public_key();
        SealingKey::set(data_dir, Some(&key)).map_err(Error::Unwritable)?;
        replicated.publish_sealing_public_key(public_key).await?;
        tracing::info!(
            machine = %record.id(),
            %public_key,
            "generated the Cluster's Sealing Key"
        );
        Ok(SealingKeyHeld { public_key })
    }

    /// Seal this Machine's Sealing Key to another Machine's WireGuard key, as this
    /// Machine. The recipient must be published with exactly that key, so a stale
    /// roster never seals to a key the Cluster no longer trusts.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotHeld`] without a key, [`Error::UnknownMachine`] or
    /// [`Error::WireGuardKeyMismatch`] when the recipient is not published as named,
    /// and [`super::Error::Seal`] for a WireGuard key nothing can seal to.
    pub(crate) async fn share_sealing_key(
        &self,
        request: ShareSealingKeyRequest,
    ) -> Result<SealingKeyShared, super::Error> {
        self.require_management_access()?;
        let record = self.record();
        let key = SealingKey::load(self.owner.data_dir())
            .map_err(Error::Unreadable)?
            .ok_or(Error::NotHeld)?;
        let recipient = self
            .replicated()?
            .machine(&request.machine.to_string())
            .await?
            .ok_or(Error::UnknownMachine(request.machine))?;
        if recipient.public_key != request.public_key {
            return Err(Error::WireGuardKeyMismatch(request.machine).into());
        }
        let sealed = key.seal_from(record.private_key(), &request.public_key)?;
        tracing::info!(
            machine = %record.id(),
            recipient = %request.machine,
            public_key = %key.public_key(),
            "shared the Cluster's Sealing Key"
        );
        Ok(SealingKeyShared { sealed })
    }

    /// Install a Sealing Key another Machine sealed to this one, and publish its
    /// public half. Installing the key this Machine already holds is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`super::Error::NotParticipating`] before participation,
    /// [`Error::UnknownMachine`] or [`Error::Unopenable`] when the sender is not who the
    /// caller named, [`Error::SealedAnother`], [`Error::HeldAnother`],
    /// [`Error::Published`], or [`Error::AwaitsRecovery`] when the key disagrees with
    /// what the caller, this Machine, or the Cluster names, and [`Error::Unreadable`] or
    /// [`Error::Unwritable`] when the key file cannot be used.
    pub(crate) async fn install_sealing_key(
        &self,
        request: InstallSealingKeyRequest,
    ) -> Result<SealingKeyHeld, super::Error> {
        self.require_management_access()?;
        let local = self.clone();
        self.finish_mutation(async move { local.install_admitted(request).await })
            .await
    }

    async fn install_admitted(
        &self,
        request: InstallSealingKeyRequest,
    ) -> Result<SealingKeyHeld, super::Error> {
        let record = self.record();
        if record.phase() != LocalMachinePhase::Participating {
            return Err(super::Error::NotParticipating);
        }
        let replicated = self.replicated()?;
        let sender = replicated
            .machine(&request.from.to_string())
            .await?
            .ok_or(Error::UnknownMachine(request.from))?;
        let key = SealingKey::open_from(&request.sealed, record.private_key(), &sender.public_key)
            .map_err(|source| Error::Unopenable {
                from: request.from,
                source,
            })?;
        let public_key = key.public_key();
        if public_key != request.public_key {
            return Err(Error::SealedAnother {
                opened: public_key,
                requested: request.public_key,
            }
            .into());
        }
        let data_dir = self.owner.data_dir();
        match SealingKey::load(data_dir).map_err(Error::Unreadable)? {
            Some(held) if held == key => {
                replicated.publish_sealing_public_key(public_key).await?;
                tracing::info!(
                    machine = %record.id(),
                    from = %request.from,
                    %public_key,
                    "this Machine already holds the Cluster's Sealing Key"
                );
                return Ok(SealingKeyHeld { public_key });
            }
            Some(held) => {
                return Err(Error::HeldAnother {
                    held: held.public_key(),
                    requested: public_key,
                }
                .into());
            }
            None => {}
        }
        match replicated.sealing_public_key().await? {
            Some(published) if published != public_key => {
                return Err(Error::Published(published).into());
            }
            Some(_) => {}
            None => {
                if let Some(awaited) = replicated.sealing_awaits_recovery().await?
                    && awaited != public_key
                {
                    return Err(Error::AwaitsRecovery(awaited).into());
                }
            }
        }
        SealingKey::set(data_dir, Some(&key)).map_err(Error::Unwritable)?;
        replicated.publish_sealing_public_key(public_key).await?;
        tracing::info!(
            machine = %record.id(),
            from = %request.from,
            %public_key,
            "installed the Cluster's Sealing Key"
        );
        Ok(SealingKeyHeld { public_key })
    }
}
