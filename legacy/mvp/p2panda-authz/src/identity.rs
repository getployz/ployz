use std::fmt::{self, Debug, Display, Formatter};
use std::num::NonZeroU64;

use mvp_bus::{IslandId, PrincipalId};
use p2panda_auth::Access;
use p2panda_auth::traits::{Conditions, IdentityHandle, OperationId};
#[cfg(test)]
use p2panda_core::SigningKey;
use p2panda_core::VerifyingKey;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct AuthId(pub(crate) [u8; 32]);

impl AuthId {
    pub(crate) fn derive(tag: &'static [u8], parts: &[&[u8]]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hash_bytes(&mut hasher, tag);
        for part in parts {
            hash_bytes(&mut hasher, part);
        }
        Self(*hasher.finalize().as_bytes())
    }

    pub(crate) fn short_hex(&self) -> String {
        let mut value = String::with_capacity(16);
        for byte in &self.0[..8] {
            use std::fmt::Write as _;
            write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
        }
        value
    }
}

impl Debug for AuthId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AuthId").field(&self.short_hex()).finish()
    }
}

impl Display for AuthId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.short_hex())
    }
}

impl IdentityHandle for AuthId {}

pub(crate) fn hash_bytes(hasher: &mut blake3::Hasher, value: &[u8]) {
    hash_u64(hasher, value.len() as u64);
    hasher.update(value);
}

pub(crate) fn hash_str(hasher: &mut blake3::Hasher, value: &str) {
    hash_bytes(hasher, value.as_bytes());
}

pub(crate) fn hash_u64(hasher: &mut blake3::Hasher, value: u64) {
    hasher.update(&value.to_be_bytes());
}

pub(crate) fn hash_auth_id(hasher: &mut blake3::Hasher, value: AuthId) {
    hasher.update(&value.0);
}

pub(crate) fn hash_operation_id(hasher: &mut blake3::Hasher, value: IslandOperationId) {
    hasher.update(&value.as_bytes());
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IslandGroupId(pub(crate) AuthId);

impl IslandGroupId {
    #[must_use]
    pub fn from_island(island: &IslandId) -> Self {
        Self(AuthId::derive(
            b"ployz:island-group",
            &[island.as_str().as_bytes()],
        ))
    }

    pub(crate) fn auth_id(self) -> AuthId {
        self.0
    }
}

impl Display for IslandGroupId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IslandMemberId(pub(crate) AuthId);

impl IslandMemberId {
    pub(crate) fn auth_id(self) -> AuthId {
        self.0
    }
}

impl Display for IslandMemberId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IslandOperationId(pub(crate) AuthId);

impl IslandOperationId {
    pub(crate) fn from_parts(tag: &'static [u8], parts: &[&[u8]]) -> Self {
        Self(AuthId::derive(tag, parts))
    }

    pub(crate) fn as_bytes(self) -> [u8; 32] {
        self.0.0
    }
}

impl Display for IslandOperationId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl OperationId for IslandOperationId {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IslandMemberEpoch(NonZeroU64);

impl IslandMemberEpoch {
    #[must_use]
    pub fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IslandMemberAuthorKey(VerifyingKey);

impl IslandMemberAuthorKey {
    #[must_use]
    pub fn from_public_key(public_key: VerifyingKey) -> Self {
        Self(public_key)
    }

    #[cfg(test)]
    pub(crate) fn from_private_key(private_key: &SigningKey) -> Self {
        Self(private_key.verifying_key())
    }

    #[must_use]
    pub fn public_key(self) -> VerifyingKey {
        self.0
    }

    pub(crate) fn as_bytes(self) -> [u8; 32] {
        *self.0.as_bytes()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IslandMemberKeyBinding {
    island: IslandId,
    principal: PrincipalId,
    epoch: IslandMemberEpoch,
    author_key: IslandMemberAuthorKey,
    member_id: IslandMemberId,
}

impl IslandMemberKeyBinding {
    #[must_use]
    pub fn new(
        island: IslandId,
        principal: PrincipalId,
        epoch: IslandMemberEpoch,
        author_key: IslandMemberAuthorKey,
    ) -> Self {
        let member_id = IslandMemberId(AuthId::derive(
            b"ployz:island-member",
            &[
                island.as_str().as_bytes(),
                principal.as_str().as_bytes(),
                &epoch.get().to_be_bytes(),
                &author_key.as_bytes(),
            ],
        ));
        Self {
            island,
            principal,
            epoch,
            author_key,
            member_id,
        }
    }

    #[must_use]
    pub fn island(&self) -> &IslandId {
        &self.island
    }

    #[must_use]
    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    #[must_use]
    pub fn epoch(&self) -> IslandMemberEpoch {
        self.epoch
    }

    #[must_use]
    pub fn author_key(&self) -> IslandMemberAuthorKey {
        self.author_key
    }

    #[must_use]
    pub fn member_id(&self) -> IslandMemberId {
        self.member_id
    }
}

impl Serialize for IslandMemberKeyBinding {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct Wire<'a> {
            island: &'a str,
            principal: &'a str,
            epoch: u64,
            author_key: [u8; 32],
        }

        Wire {
            island: self.island.as_str(),
            principal: self.principal.as_str(),
            epoch: self.epoch.get(),
            author_key: self.author_key.as_bytes(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for IslandMemberKeyBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            island: String,
            principal: String,
            epoch: u64,
            author_key: [u8; 32],
        }

        let wire = Wire::deserialize(deserializer)?;
        let epoch = NonZeroU64::new(wire.epoch)
            .ok_or_else(|| serde::de::Error::custom("member epoch must be non-zero"))?;
        let author_key = VerifyingKey::try_from(&wire.author_key)
            .map_err(|error| serde::de::Error::custom(error.to_string()))?;
        Ok(Self::new(
            IslandId::new(wire.island),
            PrincipalId::new(wire.principal),
            IslandMemberEpoch::new(epoch),
            IslandMemberAuthorKey::from_public_key(author_key),
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum IslandMemberCondition {
    ReplicaImporter,
}

impl Conditions for IslandMemberCondition {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplicaImportAccess {
    Pull,
    Read,
}

impl ReplicaImportAccess {
    pub(crate) fn into_access(self) -> Access<IslandMemberCondition> {
        match self {
            Self::Pull => Access::pull(),
            Self::Read => Access::read(),
        }
        .with_conditions(IslandMemberCondition::ReplicaImporter)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IslandMemberRole {
    Manager,
    Writer,
    ReplicaImporter(ReplicaImportAccess),
}

impl IslandMemberRole {
    pub(crate) fn into_access(self) -> Access<IslandMemberCondition> {
        match self {
            Self::Manager => Access::manage(),
            Self::Writer => Access::write(),
            Self::ReplicaImporter(level) => level.into_access(),
        }
    }
}
