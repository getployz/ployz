//! The recipient key an activation adds beside the Cluster's identity key.
//!
//! The identity stays the key every statement and sealed value binds. The recipient is what
//! new values seal to, so a past holder of the identity opens none of them. A Machine
//! installs only the recipient a verified `SealingRotation` names, for the activation its
//! own record committed, and re-seals only to the recipient its own file holds.

use ployz_core::{
    LocalMachinePhase, PLATFORM_SEALED_INFO, RecipientKeySet, RecipientSecret, ResealValue,
    ResealValuesRequest, SEALED_VALUE_INFO, SealingPublicKey, SetRecipientKeyRequest,
    ValuesResealed,
    authority::{
        AuthorityRefusal, Member, SealingRotation, Signed, Timestamp,
        rotation::{self, sealed_key_binding},
    },
};

use super::{Error, LocalMachine};
use crate::{
    machine::LocalMachineRecord,
    sealing_key::{ClusterKeys, SealingKey},
};

impl LocalMachine {
    /// The recipient key this Machine holds, on the phases a Sealing Key is reported.
    pub(crate) fn held_recipient(&self, phase: LocalMachinePhase) -> Option<SealingPublicKey> {
        if !matches!(
            phase,
            LocalMachinePhase::Joining | LocalMachinePhase::Participating
        ) {
            return None;
        }
        SealingKey::load_recipient(self.owner.data_dir())
            .ok()
            .flatten()
            .map(|key| key.public_key())
    }

    /// Generate, install or share the recipient key of the activation this record
    /// committed. See [`SetRecipientKeyRequest`].
    ///
    /// # Errors
    /// Returns [`Error::RecipientRefused`] before activation, for a rotation that does not
    /// verify against the trusted snapshot or names another activation or recipient, and
    /// for a Member the rotation does not name, and [`Error::RecipientKey`] when a key file
    /// can't be read or written.
    pub(crate) async fn set_recipient_key(
        &self,
        request: SetRecipientKeyRequest,
    ) -> Result<RecipientKeySet, Error> {
        let local = self.clone();
        self.finish_mutation(async move { local.recipient_admitted(request) })
            .await
    }

    fn recipient_admitted(
        &self,
        request: SetRecipientKeyRequest,
    ) -> Result<RecipientKeySet, Error> {
        let refused = |field| Error::RecipientRefused {
            capability: ployz_core::SET_RECIPIENT_KEY_CAPABILITY,
            refusal: AuthorityRefusal::Mismatch { field },
        };
        let record = self.record();
        let data_dir = self.owner.data_dir();
        let activation = record
            .activation()
            .ok_or_else(|| refused("an activation"))?;
        let identity = SealingKey::load(data_dir)
            .map_err(Error::RecipientKey)?
            .ok_or_else(|| refused("a Sealing Key"))?
            .public_key();
        match request {
            SetRecipientKeyRequest::Generate { operation } => {
                if operation != activation {
                    return Err(refused("an activation"));
                }
                let key =
                    SealingKey::generated_for(data_dir, operation).map_err(Error::RecipientKey)?;
                Ok(RecipientKeySet::Generated {
                    recipient: key.public_key(),
                })
            }
            SetRecipientKeyRequest::Install { rotation, sealed } => {
                let body = verified(&record, &rotation, identity, activation)?;
                let key = match sealed {
                    Some(sealed) => RecipientSecret::open(
                        record.wireguard_private_key.as_bytes(),
                        &sealed_key_binding(&identity, &body.operation),
                        &sealed,
                    )
                    .map(|secret| SealingKey::from_secret(*secret.secret()))
                    .ok_or_else(|| refused("the sealed recipient key"))?,
                    None => {
                        match SealingKey::load_generated(data_dir).map_err(Error::RecipientKey)? {
                            Some((operation, key)) if operation == body.operation => key,
                            _ => return Err(refused("the sealed recipient key")),
                        }
                    }
                };
                if key.public_key() != body.recipient {
                    return Err(refused("the recipient key"));
                }
                match SealingKey::load_recipient(data_dir).map_err(Error::RecipientKey)? {
                    Some(held) if held.public_key() == body.recipient => {}
                    Some(_) => return Err(refused("the recipient key")),
                    None => {
                        SealingKey::set_recipient(data_dir, &key).map_err(Error::RecipientKey)?;
                        tracing::info!(
                            recipient = %body.recipient,
                            operation = %body.operation,
                            "installed the Cluster's recipient key"
                        );
                    }
                }
                Ok(RecipientKeySet::Installed {
                    recipient: body.recipient,
                })
            }
            SetRecipientKeyRequest::Share { rotation, to } => {
                let body = verified(&record, &rotation, identity, activation)?;
                let held = SealingKey::load_recipient(data_dir)
                    .map_err(Error::RecipientKey)?
                    .filter(|held| held.public_key() == body.recipient)
                    .ok_or_else(|| refused("the recipient key"))?;
                let member = body
                    .members
                    .iter()
                    .find(|member| member.id == to)
                    .ok_or_else(|| refused("a Member"))?;
                let sealed = RecipientSecret::from_secret(*held.secret())
                    .seal_to(
                        &member.public_key,
                        &sealed_key_binding(&identity, &body.operation),
                    )
                    .map_err(|_| refused("a Member's WireGuard key"))?;
                Ok(RecipientKeySet::Shared { to, sealed })
            }
        }
    }

    /// Seal each value again to this Machine's own recipient key. The request names no key,
    /// so a value only ever moves to the key a person named. A value that does not open
    /// answers `None`.
    ///
    /// # Errors
    /// Returns [`Error::RecipientRefused`] when no recipient key is installed, and
    /// [`Error::RecipientKey`] when the keys can't be read.
    pub(crate) async fn reseal_values(
        &self,
        ResealValuesRequest { values }: ResealValuesRequest,
    ) -> Result<ValuesResealed, Error> {
        let keys = ClusterKeys::load(self.owner.data_dir())
            .map_err(Error::RecipientKey)?
            .filter(|keys| keys.recipient().is_some())
            .ok_or(Error::RecipientRefused {
                capability: ployz_core::RESEAL_VALUES_CAPABILITY,
                refusal: AuthorityRefusal::Mismatch {
                    field: "a recipient key",
                },
            })?;
        let recipient = keys.sealing_to();
        let cluster = keys.identity().public_key();
        let values = values
            .iter()
            .map(|value| match value {
                ResealValue::Env { identity, value } => {
                    let aad = value.associated_data(identity);
                    keys.open_value(&aad, &value.sealed)
                        .map(|plaintext| recipient.seal_under(SEALED_VALUE_INFO, &aad, &plaintext))
                }
                ResealValue::Platform { purpose, sealed } => {
                    keys.open_platform(*purpose, sealed).map(|plaintext| {
                        recipient.seal_under(
                            PLATFORM_SEALED_INFO,
                            &purpose.associated_data(&cluster),
                            &plaintext,
                        )
                    })
                }
            })
            .collect();
        Ok(ValuesResealed {
            recipient: recipient.public_key(),
            values,
        })
    }
}

/// Open `rotation` against this activated record's trusted keys, for its identity, naming
/// this Machine and the activation it committed.
fn verified(
    record: &LocalMachineRecord,
    rotation: &Signed<SealingRotation>,
    identity: SealingPublicKey,
    activation: ployz_core::OperationId,
) -> Result<SealingRotation, Error> {
    let refused = |refusal| Error::RecipientRefused {
        capability: ployz_core::SET_RECIPIENT_KEY_CAPABILITY,
        refusal,
    };
    let own: Member = record.machine().ok_or(Error::NotParticipating)?.into();
    let (body, approver) = rotation::verify(
        rotation,
        &record.trusted_keys(),
        identity,
        &own,
        Timestamp::now(),
    )
    .map_err(refused)?;
    if body.operation != activation {
        return Err(refused(AuthorityRefusal::Mismatch {
            field: "an activation",
        }));
    }
    tracing::info!(%approver, recipient = %body.recipient, "recipient key approved");
    Ok(body)
}
