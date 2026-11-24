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

use ployz_core::authority::MismatchField;
use std::io;

use ployz_core::{
    GenerateSealingKeyRequest, InstallSealingKeyRequest, MachineId, SealingKeyHeld,
    SealingKeyShared, SealingPublicKey, ShareSealingKeyRequest,
};
use thiserror::Error as ThisError;

use super::join_approval::Entered;
use super::{KeyChangeHeld, LocalMachine};
use crate::corrosion::ClusterSealing;
use crate::sealing_key::{Floors, KeyFile, OpenError, Removals, SealingKey};

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
    /// Write `key` as this Machine's Sealing Key, over `replaced`, the file read before: none,
    /// or one that was not 32 bytes.
    async fn store_sealing_key(
        &self,
        key: SealingKey,
        replaced: &KeyFile,
    ) -> Result<(), super::Error> {
        let public_key = key.public_key();
        self.owner
            .mutate(move |store| store.set_sealing_key(Some(&key)))
            .await?
            .map_err(Error::Unwritable)?;
        if *replaced == KeyFile::Unusable {
            tracing::warn!(
                machine = %self.record().id(),
                %public_key,
                "replaced a Sealing Key file that was not 32 bytes"
            );
        }
        Ok(())
    }

    pub(crate) async fn generate_sealing_key(
        &self,
        GenerateSealingKeyRequest {}: GenerateSealingKeyRequest,
    ) -> Result<SealingKeyHeld, super::Error> {
        self.finish_mutation(move |local| async move { local.generate_admitted().await })
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
        record.require_participating()?;
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
        self.store_sealing_key(key, &file).await?;
        replicated.publish_sealing_public_key(public_key).await?;
        tracing::info!(
            machine = %record.id(),
            %public_key,
            "generated the Cluster's Sealing Key"
        );
        Ok(SealingKeyHeld { public_key })
    }

    /// The floors to seal beside `key` for a recipient that `opens_floors`. `None` for one
    /// that doesn't, since an older ployzd refuses a payload that carries them; it then
    /// misses them, as it would miss a forget sent before it joined.
    pub(super) fn floors_for(
        &self,
        key: &SealingKey,
        opens_floors: bool,
    ) -> Result<Option<Floors>, super::Error> {
        if !opens_floors {
            return Ok(None);
        }
        self.approvals.floors_in(key.public_key()).map(Some)
    }

    pub(crate) async fn share_sealing_key(
        &self,
        request: ShareSealingKeyRequest,
    ) -> Result<SealingKeyShared, super::Error> {
        self.finish_mutation(
            move |local| async move { local.share_sealing_key_admitted(request).await },
        )
        .await
    }

    async fn share_sealing_key_admitted(
        &self,
        request: ShareSealingKeyRequest,
    ) -> Result<SealingKeyShared, super::Error> {
        self.refuse_removed(request.machine, Some(request.public_key))
            .await?;
        let record = self.record();
        let key = SealingKey::load(self.owner.data_dir())
            .map_err(Error::Unreadable)?
            .ok_or(Error::NotHeld)?;
        let recipient = self
            .replicated()?
            .machine(&request.machine.to_string())
            .await?;
        if let Some(row) = &recipient
            && row.public_key != request.public_key
        {
            return Err(Error::WireGuardKeyMismatch(request.machine).into());
        }
        let Some(approval) = request.join_approval.as_ref() else {
            // A Member's signed join already named its WireGuard key as a recipient of this
            // key, so sealing to it again gives it nothing new. Anyone else needs a person's
            // approval, which a caller can't stand in for.
            let recipient = recipient.filter(|row| record.trusts_member(row)).ok_or(
                super::Error::JoinRefused(ployz_core::authority::AuthorityRefusal::Unsigned {
                    purpose: ployz_core::authority::Purpose::Join,
                }),
            )?;
            let sealed = self.seal_to(&key, &recipient)?;
            tracing::info!(
                machine = %record.id(),
                recipient = %request.machine,
                public_key = %key.public_key(),
                "shared the Cluster's Sealing Key with a Member"
            );
            return Ok(SealingKeyShared { sealed });
        };
        let recipient = recipient.ok_or(Error::UnknownMachine(request.machine))?;
        let body = approval.open().map_err(super::Error::JoinRefused)?;
        if body.machine.id != request.machine || body.sealing_recipient != request.public_key {
            return Err(super::Error::JoinRefused(
                ployz_core::authority::AuthorityRefusal::Mismatch {
                    field: MismatchField::SealingRecipient,
                },
            ));
        }
        let fresh = match self
            .enter_join(request.machine, |stored| Ok(stored.approval == *approval))?
        {
            Entered::Fresh(fresh) => fresh,
            Entered::Replayed(stored) => {
                return match stored.reply {
                    super::consumption::JoinReply::Share(sealed) => Ok(SealingKeyShared { sealed }),
                    super::consumption::JoinReply::Register(_) => {
                        Err(super::Error::JoinConsumed(request.machine))
                    }
                };
            }
        };
        let network = super::published_network(&record, self.replicated()?).await?;
        self.verify_join(
            approval,
            &ployz_core::authority::join::JoinAllocation::from(&recipient),
            network,
        )?;
        let sealed = self.seal_to(&key, &recipient)?;
        self.consume_join(
            fresh,
            super::consumption::JoinResult {
                approval: approval.clone(),
                reply: super::consumption::JoinReply::Share(sealed.clone()),
                sealed_trust: false,
            },
        )?;
        tracing::info!(
            machine = %record.id(),
            recipient = %request.machine,
            public_key = %key.public_key(),
            "shared the Cluster's Sealing Key"
        );
        Ok(SealingKeyShared { sealed })
    }

    /// Seal `key` to `recipient`'s WireGuard key, with the floors its daemon opens.
    fn seal_to(
        &self,
        key: &SealingKey,
        recipient: &ployz_core::Machine,
    ) -> Result<ployz_core::SealedSealingKey, super::Error> {
        // The recipient's own row says what its daemon opens: it publishes that row itself.
        let opens_floors = recipient
            .runtime
            .claim(ployz_core::FORGET_VERSIONS_CAPABILITY)
            == ployz_core::Claim::Advertises;
        let floors = self.floors_for(key, opens_floors)?;
        // A recovery founder may already participate, so it takes the removals this Machine
        // holds, as a joiner takes them with its restrictions. An older ployzd refuses them.
        let record = self.record();
        let removals = (opens_floors
            && recipient.runtime.claim(ployz_core::JOIN_TRUST_CAPABILITY)
                == ployz_core::Claim::Advertises)
            .then(|| Removals {
                removed_keys: record.removed_keys().to_vec(),
                removed_grants: record.removed_grants().to_vec(),
                removed_members: record.removed_members().to_vec(),
            });
        Ok(key.seal_from(
            record.private_key(),
            &recipient.public_key,
            floors.as_ref(),
            removals.as_ref(),
        )?)
    }

    pub(crate) async fn install_sealing_key(
        &self,
        request: InstallSealingKeyRequest,
    ) -> Result<SealingKeyHeld, super::Error> {
        // The removals it takes stop trusting keys, so it runs as a key change: a Volume
        // step a removed key asked for ends first.
        self.finish_key_change(move |local, key_change| async move {
            local.install_admitted(request, key_change).await
        })
        .await
    }

    /// Install opens the sealed key and checks it is the one named before it
    /// reads this Machine's key file, then answers from the file:
    ///
    /// - the same key: reads the Cluster's Sealing, then
    ///   - refuses [`Error::AwaitsRecovery`] and inserts nothing when this
    ///     Machine reads its Cluster awaiting recovery of another key
    ///     ([`ClusterSealing::awaits_another`]);
    ///   - otherwise takes the sender's removals, inserts its public half as
    ///     the Cluster's (a no-op when a key is already published there, this
    ///     one or another) and answers it;
    ///   - when the read fails, logs the failure, inserts and answers as
    ///     above;
    /// - another key: refuses [`Error::HeldAnother`];
    /// - no file, or a file that is not 32 bytes: reads the Cluster's Sealing
    ///   and refuses [`Error::Published`] when it publishes another key, or
    ///   [`Error::AwaitsRecovery`] when it awaits recovery of another key (the
    ///   rule `awaits_another` states), and otherwise takes the sender's
    ///   removals, writes the key over the file, inserts its public half as the
    ///   Cluster's and answers it. A failed read refuses with its error;
    /// - a file that cannot be read: refuses [`Error::Unreadable`].
    async fn install_admitted(
        &self,
        request: InstallSealingKeyRequest,
        key_change: KeyChangeHeld,
    ) -> Result<SealingKeyHeld, super::Error> {
        let record = self.record();
        record.require_participating()?;
        let replicated = self.replicated()?;
        let sender = replicated
            .machine(&request.from.to_string())
            .await?
            .ok_or(Error::UnknownMachine(request.from))?;
        let (key, floors, removals) =
            SealingKey::open_from(&request.sealed, record.private_key(), &sender.public_key)
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
        self.approvals.install_floors(public_key, &floors)?;
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
                self.take_removals(key_change, removals).await?;
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
        // Removals land before the key, so an Install cut off after either write leaves a
        // Machine that already enforces them, and a retry finds it holding the key.
        self.take_removals(key_change, removals).await?;
        self.store_sealing_key(key, &file).await?;
        replicated.publish_sealing_public_key(public_key).await?;
        tracing::info!(
            machine = %record.id(),
            from = %request.from,
            %public_key,
            "installed the Cluster's Sealing Key"
        );
        Ok(SealingKeyHeld { public_key })
    }

    /// Stop trusting each key the sender removed and hold each Member it removed. These only
    /// add restrictions, so they go over this Machine's own record and never replace its keys.
    async fn take_removals(
        &self,
        key_change: KeyChangeHeld,
        removals: Removals,
    ) -> Result<(), super::Error> {
        if removals == Removals::default() {
            return Ok(());
        }
        let grants = removals.removed_grants;
        let set = ployz_core::JoinRestrictionSet {
            removed_keys: removals.removed_keys,
            removed_members: removals.removed_members,
            floors: Default::default(),
        };
        self.owner
            .mutate(move |store| {
                store.apply_join_restrictions(&key_change, &set)?;
                store.hold_removed_grants(&grants)
            })
            .await??;
        Ok(())
    }
}
