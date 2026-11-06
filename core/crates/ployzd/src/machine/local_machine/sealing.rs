//! Backfilling the Cluster's Sealing Key onto this Machine, one route at a time.
//!
//! An operator drives these: Generate on one Machine of a Cluster founded before
//! Sealing Keys existed, then Share from a holder and Install on each Machine that
//! still needs access. No route elects anything; each only checks that what it is
//! asked to do agrees with its key file and with the Cluster's Sealing as this
//! Machine reads it: the key its replica publishes or awaits, or the key its own
//! RecoverySeed awaits.
//!
//! A key file that is not 32 bytes ([`KeyFile::Unusable`]) holds no key to keep,
//! so Generate and Install read it as a missing file and may replace it. Share
//! needs a key and refuses it. A file that cannot be read is never replaced.

use std::io;

use ployz_core::{
    GenerateSealingKeyRequest, InstallSealingKeyRequest, LocalMachinePhase, MachineId,
    SealingKeyHeld, SealingKeyShared, SealingPublicKey, ShareSealingKeyRequest,
};
use thiserror::Error as ThisError;

use super::LocalMachine;
use crate::corrosion::ClusterSealing;
use crate::sealing_key::{KeyFile, OpenError, SealingKey};

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
    pub(crate) async fn generate_sealing_key(
        &self,
        GenerateSealingKeyRequest {}: GenerateSealingKeyRequest,
    ) -> Result<SealingKeyHeld, super::Error> {
        self.require_management_access()?;
        let local = self.clone();
        self.finish_mutation(async move { local.generate_admitted().await })
            .await
    }

    /// Generate answers from this Machine's key file first, and reads the
    /// Cluster's Sealing only when the file holds no key:
    ///
    /// - a key: answers that key, with no Cluster read;
    /// - no file, or a file that is not 32 bytes: refuses [`Error::Published`]
    ///   or [`Error::AwaitsRecovery`] when the Cluster names a key, and under
    ///   [`ClusterSealing::Unsealed`] writes a new key to the file, then
    ///   inserts its public half as the Cluster's (a no-op when a key is
    ///   already published there) and answers it;
    /// - a file that cannot be read: refuses [`Error::Unreadable`].
    async fn generate_admitted(&self) -> Result<SealingKeyHeld, super::Error> {
        let record = self.record();
        if record.phase() != LocalMachinePhase::Participating {
            return Err(super::Error::NotParticipating);
        }
        let data_dir = self.owner.data_dir();
        let file = SealingKey::read(data_dir).map_err(Error::Unreadable)?;
        match &file {
            KeyFile::Held(held) => {
                let public_key = held.public_key();
                tracing::info!(
                    machine = %record.id(),
                    %public_key,
                    "this Machine already holds the Cluster's Sealing Key"
                );
                return Ok(SealingKeyHeld { public_key });
            }
            KeyFile::Absent | KeyFile::Unusable => {}
        }
        let replicated = self.replicated()?;
        match replicated.cluster_sealing(record.recovery_seed()).await? {
            ClusterSealing::Published { key: published, .. } => {
                return Err(Error::Published(published).into());
            }
            ClusterSealing::AwaitsRecovery(awaited) => {
                return Err(Error::AwaitsRecovery(awaited).into());
            }
            ClusterSealing::Unsealed => {}
        }
        let key = SealingKey::generate();
        let public_key = key.public_key();
        self.owner
            .mutate(move |store| store.set_sealing_key(Some(&key)))
            .await?
            .map_err(Error::Unwritable)?;
        if file == KeyFile::Unusable {
            tracing::warn!(
                machine = %record.id(),
                %public_key,
                "replaced a Sealing Key file that was not 32 bytes"
            );
        }
        replicated.publish_sealing_public_key(public_key).await?;
        tracing::info!(
            machine = %record.id(),
            %public_key,
            "generated the Cluster's Sealing Key"
        );
        Ok(SealingKeyHeld { public_key })
    }

    pub(crate) async fn share_sealing_key(
        &self,
        request: ShareSealingKeyRequest,
    ) -> Result<SealingKeyShared, super::Error> {
        self.require_management_access()?;
        if !self.record().device_enforcement() {
            return self.share_sealing_key_admitted(request).await;
        }
        let local = self.clone();
        self.finish_mutation(async move { local.share_sealing_key_admitted(request).await })
            .await
    }

    async fn share_sealing_key_admitted(
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
        if record.device_enforcement() {
            let approval = request
                .join_approval
                .as_ref()
                .ok_or(super::Error::JoinRefused(
                    ployz_core::authority::AuthorityRefusal::Unsigned {
                        purpose: ployz_core::authority::Purpose::Join,
                    },
                ))?;
            let body = approval.open().map_err(super::Error::JoinRefused)?;
            if body.machine.id != request.machine || body.sealing_recipient != request.public_key {
                return Err(super::Error::JoinRefused(
                    ployz_core::authority::AuthorityRefusal::Mismatch {
                        field: "a sealing recipient",
                    },
                ));
            }
            self.verify_join(approval, &body.machine, body.network)?;
            self.consume_join(&body)?;
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

    pub(crate) async fn install_sealing_key(
        &self,
        request: InstallSealingKeyRequest,
    ) -> Result<SealingKeyHeld, super::Error> {
        self.require_management_access()?;
        let local = self.clone();
        self.finish_mutation(async move { local.install_admitted(request).await })
            .await
    }

    /// Install opens the sealed key and checks it is the one named before it
    /// reads this Machine's key file, then answers from the file:
    ///
    /// - the same key: reads the Cluster's Sealing, then
    ///   - refuses [`Error::AwaitsRecovery`] and inserts nothing when this
    ///     Machine reads its Cluster awaiting recovery of another key
    ///     ([`ClusterSealing::awaits_another`]);
    ///   - otherwise inserts its public half as the Cluster's (a no-op when a
    ///     key is already published there, this one or another) and answers
    ///     it;
    ///   - when the read fails, logs the failure, inserts and answers as
    ///     above;
    /// - another key: refuses [`Error::HeldAnother`];
    /// - no file, or a file that is not 32 bytes: reads the Cluster's Sealing
    ///   and refuses [`Error::Published`] when it publishes another key, or
    ///   [`Error::AwaitsRecovery`] when it awaits recovery of another key (the
    ///   rule `awaits_another` states), and otherwise writes the key over the
    ///   file, inserts its public half as the Cluster's and answers it. A
    ///   failed read refuses with its error;
    /// - a file that cannot be read: refuses [`Error::Unreadable`].
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
        let file = SealingKey::read(data_dir).map_err(Error::Unreadable)?;
        match &file {
            KeyFile::Held(held) if *held == key => {
                match replicated.cluster_sealing(record.recovery_seed()).await {
                    Ok(cluster) => {
                        if let Some(awaited) = cluster.awaits_another(public_key) {
                            return Err(Error::AwaitsRecovery(awaited).into());
                        }
                    }
                    Err(error) => tracing::warn!(
                        machine = %record.id(),
                        %public_key,
                        error = %ployz_core::error_chain::inline(&error),
                        "could not read whether the Cluster awaits recovery of another Sealing Key; publishing the held key"
                    ),
                }
                replicated.publish_sealing_public_key(public_key).await?;
                tracing::info!(
                    machine = %record.id(),
                    from = %request.from,
                    %public_key,
                    "this Machine already holds this Sealing Key"
                );
                return Ok(SealingKeyHeld { public_key });
            }
            KeyFile::Held(held) => {
                return Err(Error::HeldAnother {
                    held: held.public_key(),
                    requested: public_key,
                }
                .into());
            }
            KeyFile::Absent | KeyFile::Unusable => {}
        }
        match replicated.cluster_sealing(record.recovery_seed()).await? {
            ClusterSealing::Published { key: published, .. } if published != public_key => {
                return Err(Error::Published(published).into());
            }
            ClusterSealing::AwaitsRecovery(awaited) if awaited != public_key => {
                return Err(Error::AwaitsRecovery(awaited).into());
            }
            ClusterSealing::Published { .. }
            | ClusterSealing::AwaitsRecovery(_)
            | ClusterSealing::Unsealed => {}
        }
        self.owner
            .mutate(move |store| store.set_sealing_key(Some(&key)))
            .await?
            .map_err(Error::Unwritable)?;
        if file == KeyFile::Unusable {
            tracing::warn!(
                machine = %record.id(),
                %public_key,
                "replaced a Sealing Key file that was not 32 bytes"
            );
        }
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
