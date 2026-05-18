use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Debug, Display, Formatter};
use std::num::NonZeroU64;

use mvp_bus::{IslandId, PrincipalId};
use p2panda_auth::group::resolver::StrongRemove;
use p2panda_auth::group::{GroupAction, GroupCrdt, GroupCrdtError, GroupCrdtState, GroupMember};
use p2panda_auth::traits::{Conditions, IdentityHandle, Operation, OperationId};
use p2panda_auth::{Access, AccessLevel};
use p2panda_core::cbor::encode_cbor;
use p2panda_core::{Body, Header, Operation as PandaOperation, SigningKey, VerifyingKey};
use p2panda_core::{Signature, validate_operation};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
struct AuthId([u8; 32]);

impl AuthId {
    fn derive(tag: &'static [u8], parts: &[&[u8]]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hash_bytes(&mut hasher, tag);
        for part in parts {
            hash_bytes(&mut hasher, part);
        }
        Self(*hasher.finalize().as_bytes())
    }

    fn short_hex(&self) -> String {
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

fn hash_bytes(hasher: &mut blake3::Hasher, value: &[u8]) {
    hash_u64(hasher, value.len() as u64);
    hasher.update(value);
}

fn hash_str(hasher: &mut blake3::Hasher, value: &str) {
    hash_bytes(hasher, value.as_bytes());
}

fn hash_u64(hasher: &mut blake3::Hasher, value: u64) {
    hasher.update(&value.to_be_bytes());
}

fn hash_auth_id(hasher: &mut blake3::Hasher, value: AuthId) {
    hasher.update(&value.0);
}

fn hash_operation_id(hasher: &mut blake3::Hasher, value: IslandOperationId) {
    hasher.update(&value.as_bytes());
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IslandGroupId(AuthId);

impl IslandGroupId {
    #[must_use]
    pub fn from_island(island: &IslandId) -> Self {
        Self(AuthId::derive(
            b"ployz:island-group",
            &[island.as_str().as_bytes()],
        ))
    }

    fn auth_id(self) -> AuthId {
        self.0
    }
}

impl Display for IslandGroupId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IslandMemberId(AuthId);

impl IslandMemberId {
    fn auth_id(self) -> AuthId {
        self.0
    }
}

impl Display for IslandMemberId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IslandOperationId(AuthId);

impl IslandOperationId {
    fn from_parts(tag: &'static [u8], parts: &[&[u8]]) -> Self {
        Self(AuthId::derive(tag, parts))
    }

    fn as_bytes(self) -> [u8; 32] {
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
    fn from_private_key(private_key: &SigningKey) -> Self {
        Self(private_key.verifying_key())
    }

    #[must_use]
    pub fn public_key(self) -> VerifyingKey {
        self.0
    }

    fn as_bytes(self) -> [u8; 32] {
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
    fn into_access(self) -> Access<IslandMemberCondition> {
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
    fn into_access(self) -> Access<IslandMemberCondition> {
        match self {
            Self::Manager => Access::manage(),
            Self::Writer => Access::write(),
            Self::ReplicaImporter(level) => level.into_access(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct IslandAuthOperation {
    id: IslandOperationId,
    author: AuthId,
    dependencies: Vec<IslandOperationId>,
    group_id: AuthId,
    action: GroupAction<AuthId, IslandMemberCondition>,
}

impl Operation<AuthId, IslandOperationId, IslandMemberCondition> for IslandAuthOperation {
    fn id(&self) -> IslandOperationId {
        self.id
    }

    fn author(&self) -> AuthId {
        self.author
    }

    fn dependencies(&self) -> Vec<IslandOperationId> {
        self.dependencies.clone()
    }

    fn group_id(&self) -> AuthId {
        self.group_id
    }

    fn action(&self) -> GroupAction<AuthId, IslandMemberCondition> {
        self.action.clone()
    }
}

type AuthResolver =
    StrongRemove<AuthId, IslandOperationId, IslandAuthOperation, IslandMemberCondition>;
type AuthCrdt =
    GroupCrdt<AuthId, IslandOperationId, IslandAuthOperation, IslandMemberCondition, AuthResolver>;
type AuthState =
    GroupCrdtState<AuthId, IslandOperationId, IslandAuthOperation, IslandMemberCondition>;
type AuthCrdtError = GroupCrdtError<
    AuthId,
    IslandOperationId,
    IslandAuthOperation,
    IslandMemberCondition,
    AuthResolver,
>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IslandMembershipPayload([u8; 32]);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IslandSignedOperation {
    operation: IslandAuthOperation,
    signer: IslandMemberId,
    introduced_binding: Option<IslandMemberKeyBinding>,
    signature: Signature,
}

impl IslandSignedOperation {
    fn sign(
        operation: IslandAuthOperation,
        signer: IslandMemberId,
        signer_private_key: &SigningKey,
        introduced_binding: Option<IslandMemberKeyBinding>,
    ) -> Self {
        let payload = membership_operation_payload(&operation, signer, introduced_binding.as_ref());
        let signature = signer_private_key.sign(&payload.0);
        Self {
            operation,
            signer,
            introduced_binding,
            signature,
        }
    }

    #[must_use]
    pub fn operation_id(&self) -> IslandOperationId {
        self.operation.id()
    }

    #[must_use]
    pub fn signer(&self) -> IslandMemberId {
        self.signer
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct IslandMembershipExtensions {
    island: String,
    group: AuthId,
    actor: AuthId,
}

impl IslandMembershipExtensions {
    fn new(island: &IslandId, signed: &IslandSignedOperation) -> Self {
        Self {
            island: island.as_str().to_owned(),
            group: signed.operation.group_id(),
            actor: signed.signer.auth_id(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IslandAuthorityMember {
    member_id: IslandMemberId,
    principal: PrincipalId,
    epoch: IslandMemberEpoch,
    author_key: IslandMemberAuthorKey,
}

impl IslandAuthorityMember {
    fn from_binding(binding: &IslandMemberKeyBinding) -> Self {
        Self {
            member_id: binding.member_id(),
            principal: binding.principal().clone(),
            epoch: binding.epoch(),
            author_key: binding.author_key(),
        }
    }

    #[must_use]
    pub fn member_id(&self) -> IslandMemberId {
        self.member_id
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IslandAuthoritySnapshot {
    island: IslandId,
    active_writers: BTreeMap<PrincipalId, IslandAuthorityMember>,
    active_replica_importers: BTreeMap<PrincipalId, IslandAuthorityMember>,
}

impl IslandAuthoritySnapshot {
    fn from_authz(authz: &IslandAuthz) -> Self {
        let mut active_writers = BTreeMap::new();
        let mut active_replica_importers = BTreeMap::new();

        for binding in authz.bindings.values() {
            let member = IslandAuthorityMember::from_binding(binding);
            if authz.can_write_member(binding.member_id()) {
                insert_newest_authority_member(&mut active_writers, member.clone());
            }
            if authz.can_import_replica(binding.member_id()) {
                insert_newest_authority_member(&mut active_replica_importers, member);
            }
        }

        Self {
            island: authz.island.clone(),
            active_writers,
            active_replica_importers,
        }
    }

    #[must_use]
    pub fn island(&self) -> &IslandId {
        &self.island
    }

    #[must_use]
    pub fn active_writer(&self, principal: &PrincipalId) -> Option<&IslandAuthorityMember> {
        self.active_writers.get(principal)
    }

    #[must_use]
    pub fn active_replica_importer(
        &self,
        principal: &PrincipalId,
    ) -> Option<&IslandAuthorityMember> {
        self.active_replica_importers.get(principal)
    }

    pub fn active_writers(&self) -> impl Iterator<Item = &IslandAuthorityMember> {
        self.active_writers.values()
    }
}

fn insert_newest_authority_member(
    members: &mut BTreeMap<PrincipalId, IslandAuthorityMember>,
    member: IslandAuthorityMember,
) {
    match members.get(member.principal()) {
        Some(existing) if existing.epoch() > member.epoch() => {}
        Some(_) | None => {
            members.insert(member.principal().clone(), member);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IslandRootAuthority {
    binding: IslandMemberKeyBinding,
}

impl IslandRootAuthority {
    #[must_use]
    pub fn new(binding: IslandMemberKeyBinding) -> Self {
        Self { binding }
    }

    #[must_use]
    pub fn binding(&self) -> &IslandMemberKeyBinding {
        &self.binding
    }
}

#[derive(Clone, Debug)]
pub struct IslandAuthzMemoryLog {
    island: IslandId,
    operations: Vec<(Header<IslandMembershipExtensions>, IslandSignedOperation)>,
}

impl IslandAuthzMemoryLog {
    #[must_use]
    pub fn new(island: IslandId) -> Self {
        Self {
            island,
            operations: Vec::new(),
        }
    }

    #[must_use]
    #[cfg(test)]
    fn from_store(
        island: IslandId,
        operations: Vec<(Header<IslandMembershipExtensions>, IslandSignedOperation)>,
    ) -> Self {
        Self { island, operations }
    }

    #[must_use]
    #[cfg(test)]
    fn store(&self) -> Vec<(Header<IslandMembershipExtensions>, IslandSignedOperation)> {
        self.operations.clone()
    }

    pub async fn create_root(
        &mut self,
        root: IslandMemberKeyBinding,
        root_key: &SigningKey,
    ) -> Result<IslandAuthz, IslandAuthzError> {
        if root.author_key().public_key() != root_key.verifying_key() {
            return Err(IslandAuthzError::RootAuthorityMismatch(root.member_id()));
        }
        let mut authz = IslandAuthz::empty_with_root(self.island.clone(), root.clone())?;
        let operation = authz.create_group_operation(root.member_id())?;
        let signed = IslandSignedOperation::sign(operation, root.member_id(), root_key, None);
        self.insert_signed(&signed, root_key).await?;
        Ok(authz)
    }

    pub async fn apply_signed(
        &mut self,
        authz: &mut IslandAuthz,
        signed: IslandSignedOperation,
        signer_private_key: &SigningKey,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        authz.validate_signed(&signed)?;
        let mut candidate = authz.clone();
        let change = candidate.apply_validated_signed(signed.clone())?;
        self.insert_signed(&signed, signer_private_key).await?;
        *authz = candidate;
        Ok(change)
    }

    pub async fn add_writer(
        &mut self,
        authz: &mut IslandAuthz,
        manager: &IslandMemberKeyBinding,
        manager_private_key: &SigningKey,
        member: IslandMemberKeyBinding,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        self.add_member(
            authz,
            manager,
            manager_private_key,
            member,
            IslandMemberRole::Writer,
        )
        .await
    }

    pub async fn add_replica_importer(
        &mut self,
        authz: &mut IslandAuthz,
        manager: &IslandMemberKeyBinding,
        manager_private_key: &SigningKey,
        member: IslandMemberKeyBinding,
        access: ReplicaImportAccess,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        self.add_member(
            authz,
            manager,
            manager_private_key,
            member,
            IslandMemberRole::ReplicaImporter(access),
        )
        .await
    }

    pub async fn remove_member(
        &mut self,
        authz: &mut IslandAuthz,
        manager: &IslandMemberKeyBinding,
        manager_private_key: &SigningKey,
        member: IslandMemberId,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        self.validate_membership_mutation(authz, manager, manager_private_key)?;
        let mut candidate = authz.clone();
        let operation = candidate.remove_member_operation(manager.member_id(), member)?;
        let change = IslandAuthChange {
            operation_id: operation.id(),
            actor: manager.member_id(),
        };
        let signed =
            IslandSignedOperation::sign(operation, manager.member_id(), manager_private_key, None);
        self.insert_signed(&signed, manager_private_key).await?;
        *authz = candidate;
        Ok(change)
    }

    pub async fn demote_to_replica_importer(
        &mut self,
        authz: &mut IslandAuthz,
        manager: &IslandMemberKeyBinding,
        manager_private_key: &SigningKey,
        member: IslandMemberId,
        access: ReplicaImportAccess,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        self.validate_membership_mutation(authz, manager, manager_private_key)?;
        let mut candidate = authz.clone();
        let operation = candidate.demote_member_operation(
            manager.member_id(),
            member,
            IslandMemberRole::ReplicaImporter(access),
        )?;
        let change = IslandAuthChange {
            operation_id: operation.id(),
            actor: manager.member_id(),
        };
        let signed =
            IslandSignedOperation::sign(operation, manager.member_id(), manager_private_key, None);
        self.insert_signed(&signed, manager_private_key).await?;
        *authz = candidate;
        Ok(change)
    }

    async fn add_member(
        &mut self,
        authz: &mut IslandAuthz,
        manager: &IslandMemberKeyBinding,
        manager_private_key: &SigningKey,
        member: IslandMemberKeyBinding,
        role: IslandMemberRole,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        self.validate_membership_mutation(authz, manager, manager_private_key)?;
        if member.island() != &self.island {
            return Err(IslandAuthzError::WrongIsland {
                expected: self.island.clone(),
                actual: member.island().clone(),
            });
        }
        let mut candidate = authz.clone();
        let member_id = member.member_id();
        let operation = candidate.add_member_operation(manager.member_id(), member_id, role)?;
        let change = IslandAuthChange {
            operation_id: operation.id(),
            actor: manager.member_id(),
        };
        let signed = IslandSignedOperation::sign(
            operation,
            manager.member_id(),
            manager_private_key,
            Some(member),
        );
        self.insert_signed(&signed, manager_private_key).await?;
        if let Some(binding) = signed.introduced_binding.clone() {
            candidate.bindings.insert(member_id, binding);
        }
        *authz = candidate;
        Ok(change)
    }

    fn validate_membership_mutation(
        &self,
        authz: &IslandAuthz,
        manager: &IslandMemberKeyBinding,
        manager_private_key: &SigningKey,
    ) -> Result<(), IslandAuthzError> {
        if authz.island() != &self.island {
            return Err(IslandAuthzError::WrongIsland {
                expected: self.island.clone(),
                actual: authz.island().clone(),
            });
        }
        if manager.island() != &self.island {
            return Err(IslandAuthzError::WrongIsland {
                expected: self.island.clone(),
                actual: manager.island().clone(),
            });
        }
        if manager.author_key().public_key() != manager_private_key.verifying_key() {
            return Err(IslandAuthzError::MemberKeyMismatch(manager.member_id()));
        }
        Ok(())
    }

    pub async fn replay(
        &self,
        root_authority: &IslandRootAuthority,
    ) -> Result<IslandAuthz, IslandAuthzError> {
        if root_authority.binding().island() != &self.island {
            return Err(IslandAuthzError::WrongIsland {
                expected: self.island.clone(),
                actual: root_authority.binding().island().clone(),
            });
        }
        let mut signed_operations = self.signed_operations();
        signed_operations.sort_by_key(|(header, _)| (header.seq_num, header.hash()));
        let Some((_, root_signed)) = signed_operations.first() else {
            return Err(IslandAuthzError::EmptyMembershipLog {
                island: self.island.clone(),
            });
        };
        Self::validate_root_anchor(&self.island, root_authority.binding(), root_signed)?;
        let mut authz =
            IslandAuthz::empty_with_root(self.island.clone(), root_authority.binding().clone())?;
        authz.apply_signed(root_signed.clone())?;

        for (_, signed) in signed_operations.into_iter().skip(1) {
            authz.apply_signed(signed)?;
        }
        Ok(authz)
    }

    fn signed_operations(
        &self,
    ) -> Vec<(Header<IslandMembershipExtensions>, IslandSignedOperation)> {
        self.operations.clone()
    }

    async fn insert_signed(
        &mut self,
        signed: &IslandSignedOperation,
        signer_private_key: &SigningKey,
    ) -> Result<(), IslandAuthzError> {
        let public_key = signer_private_key.verifying_key();
        verify_private_key_signed(signed, signer_private_key)?;
        let latest = self
            .operations
            .iter()
            .rev()
            .find(|(header, _)| header.verifying_key == public_key);
        let (seq_num, backlink) = latest
            .map(|(header, _)| (header.seq_num + 1, Some(header.hash())))
            .unwrap_or((0, None));
        let body_bytes = encode_cbor(signed).map_err(encode_error)?;
        let body = Body::new(&body_bytes);
        let mut header = Header {
            version: 1,
            verifying_key: public_key,
            signature: None,
            payload_size: body.size(),
            payload_hash: Some(body.hash()),
            timestamp: p2panda_core::Timestamp::now(),
            seq_num,
            backlink,
            extensions: IslandMembershipExtensions::new(&self.island, signed),
        };
        header.sign(signer_private_key);
        let operation = PandaOperation {
            hash: header.hash(),
            header: header.clone(),
            body: Some(body.clone()),
        };
        validate_operation(&operation).map_err(|error| {
            IslandAuthzError::InvalidPandaOperation {
                message: error.to_string(),
            }
        })?;
        self.operations.push((header, signed.clone()));
        Ok(())
    }

    fn validate_root_anchor(
        island: &IslandId,
        root: &IslandMemberKeyBinding,
        signed: &IslandSignedOperation,
    ) -> Result<(), IslandAuthzError> {
        if root.island() != island {
            return Err(IslandAuthzError::WrongIsland {
                expected: island.clone(),
                actual: root.island().clone(),
            });
        }
        if signed.signer != root.member_id()
            || signed.operation.author() != root.member_id().auth_id()
        {
            return Err(IslandAuthzError::UnanchoredRootCreate(root.member_id()));
        }
        if !matches!(signed.operation.action, GroupAction::Create { .. }) {
            return Err(IslandAuthzError::UnanchoredRootCreate(root.member_id()));
        }
        let signature_payload =
            membership_operation_payload(&signed.operation, signed.signer, None);
        if !root
            .author_key()
            .public_key()
            .verify(&signature_payload.0, &signed.signature)
        {
            return Err(IslandAuthzError::InvalidSignature(signed.operation_id()));
        }
        Ok(())
    }
}

fn verify_private_key_signed(
    signed: &IslandSignedOperation,
    signer_private_key: &SigningKey,
) -> Result<(), IslandAuthzError> {
    let expected = signer_private_key.verifying_key();
    let payload = membership_operation_payload(
        &signed.operation,
        signed.signer,
        signed.introduced_binding.as_ref(),
    );
    if expected.verify(&payload.0, &signed.signature) {
        Ok(())
    } else {
        Err(IslandAuthzError::InvalidSignature(signed.operation_id()))
    }
}

fn encode_error(error: impl Display) -> IslandAuthzError {
    IslandAuthzError::Encode {
        message: error.to_string(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IslandAuthChange {
    operation_id: IslandOperationId,
    actor: IslandMemberId,
}

impl IslandAuthChange {
    #[must_use]
    pub fn operation_id(&self) -> IslandOperationId {
        self.operation_id
    }

    #[must_use]
    pub fn actor(&self) -> IslandMemberId {
        self.actor
    }
}

#[derive(Clone)]
pub struct IslandAuthz {
    island: IslandId,
    group_id: IslandGroupId,
    state: Option<AuthState>,
    next_sequence: NonZeroU64,
    bindings: BTreeMap<IslandMemberId, IslandMemberKeyBinding>,
}

impl IslandAuthz {
    pub fn create(
        island: IslandId,
        root: IslandMemberKeyBinding,
    ) -> Result<Self, IslandAuthzError> {
        if root.island() != &island {
            return Err(IslandAuthzError::WrongIsland {
                expected: island,
                actual: root.island().clone(),
            });
        }
        let actor = root.member_id();
        let mut authz = Self::empty_with_root(island, root)?;
        let _operation = authz.create_group_operation(actor)?;
        Ok(authz)
    }

    fn empty_with_root(
        island: IslandId,
        root: IslandMemberKeyBinding,
    ) -> Result<Self, IslandAuthzError> {
        if root.island() != &island {
            return Err(IslandAuthzError::WrongIsland {
                expected: island,
                actual: root.island().clone(),
            });
        }
        let group_id = IslandGroupId::from_island(&island);
        let actor = root.member_id();
        Ok(Self {
            island,
            group_id,
            state: Some(AuthCrdt::init()),
            next_sequence: NonZeroU64::MIN,
            bindings: BTreeMap::from([(actor, root)]),
        })
    }

    #[must_use]
    pub fn island(&self) -> &IslandId {
        &self.island
    }

    #[must_use]
    pub fn group_id(&self) -> IslandGroupId {
        self.group_id
    }

    #[must_use]
    pub fn binding(&self, member_id: IslandMemberId) -> Option<&IslandMemberKeyBinding> {
        self.bindings.get(&member_id)
    }

    #[must_use]
    pub fn authority_snapshot(&self) -> IslandAuthoritySnapshot {
        IslandAuthoritySnapshot::from_authz(self)
    }

    #[cfg(test)]
    fn add_manager(
        &mut self,
        manager: IslandMemberId,
        member: IslandMemberKeyBinding,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        self.add_member(manager, member, IslandMemberRole::Manager)
    }

    #[cfg(test)]
    fn add_writer(
        &mut self,
        manager: IslandMemberId,
        member: IslandMemberKeyBinding,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        self.add_member(manager, member, IslandMemberRole::Writer)
    }

    #[cfg(test)]
    fn add_replica_importer(
        &mut self,
        manager: IslandMemberId,
        member: IslandMemberKeyBinding,
        access: ReplicaImportAccess,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        self.add_member(manager, member, IslandMemberRole::ReplicaImporter(access))
    }

    pub fn apply_signed(
        &mut self,
        signed: IslandSignedOperation,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        self.validate_signed(&signed)?;
        self.apply_validated_signed(signed)
    }

    fn apply_validated_signed(
        &mut self,
        signed: IslandSignedOperation,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        let introduced_binding = signed.introduced_binding.clone();
        let change = self.process_imported(signed.operation.clone())?;
        if let Some(binding) = introduced_binding {
            self.bindings.insert(binding.member_id(), binding);
        }
        Ok(change)
    }

    #[cfg(test)]
    fn remove_member(
        &mut self,
        manager: IslandMemberId,
        member: IslandMemberId,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        let operation = self.remove_member_operation(manager, member)?;
        Ok(IslandAuthChange {
            operation_id: operation.id,
            actor: manager,
        })
    }

    #[cfg(test)]
    fn demote_member(
        &mut self,
        manager: IslandMemberId,
        member: IslandMemberId,
        role: IslandMemberRole,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        let operation = self.demote_member_operation(manager, member, role)?;
        Ok(IslandAuthChange {
            operation_id: operation.id,
            actor: manager,
        })
    }

    #[must_use]
    pub fn can_write_member(&self, member: IslandMemberId) -> bool {
        self.access(member).is_some_and(|access| {
            access.conditions != Some(IslandMemberCondition::ReplicaImporter)
                && matches!(access.level, AccessLevel::Write | AccessLevel::Manage)
        })
    }

    #[must_use]
    pub fn can_import_replica(&self, member: IslandMemberId) -> bool {
        self.access(member).is_some_and(|access| {
            access.conditions == Some(IslandMemberCondition::ReplicaImporter)
                && matches!(access.level, AccessLevel::Pull | AccessLevel::Read)
        })
    }

    #[must_use]
    pub fn is_active_member(&self, member: IslandMemberId) -> bool {
        self.access(member).is_some()
    }

    fn create_group_operation(
        &mut self,
        actor: IslandMemberId,
    ) -> Result<IslandAuthOperation, IslandAuthzError> {
        self.append_group_operation(
            actor,
            GroupAction::Create {
                initial_members: vec![(
                    GroupMember::Individual(actor.auth_id()),
                    IslandMemberRole::Manager.into_access(),
                )],
            },
        )
    }

    fn add_member_operation(
        &mut self,
        manager: IslandMemberId,
        member: IslandMemberId,
        role: IslandMemberRole,
    ) -> Result<IslandAuthOperation, IslandAuthzError> {
        self.require_manager(manager)?;
        if self.is_active_member(member) {
            return Err(IslandAuthzError::AlreadyMember {
                member,
                group: self.group_id,
            });
        }
        self.append_group_operation(
            manager,
            GroupAction::Add {
                member: GroupMember::Individual(member.auth_id()),
                access: role.into_access(),
            },
        )
    }

    fn remove_member_operation(
        &mut self,
        manager: IslandMemberId,
        member: IslandMemberId,
    ) -> Result<IslandAuthOperation, IslandAuthzError> {
        self.require_manager(manager)?;
        self.require_member(member)?;
        self.append_group_operation(
            manager,
            GroupAction::Remove {
                member: GroupMember::Individual(member.auth_id()),
            },
        )
    }

    fn demote_member_operation(
        &mut self,
        manager: IslandMemberId,
        member: IslandMemberId,
        role: IslandMemberRole,
    ) -> Result<IslandAuthOperation, IslandAuthzError> {
        self.require_manager(manager)?;
        let current = self.require_member(member)?;
        let next = role.into_access();
        if current.level == next.level && current.conditions == next.conditions {
            return Err(IslandAuthzError::SameAccess {
                member,
                access: IslandAccess::from(&current),
                group: self.group_id,
            });
        }
        self.append_group_operation(
            manager,
            GroupAction::Demote {
                member: GroupMember::Individual(member.auth_id()),
                access: next,
            },
        )
    }

    fn require_manager(&self, actor: IslandMemberId) -> Result<(), IslandAuthzError> {
        let access = self.require_member(actor)?;
        if access.level == AccessLevel::Manage {
            Ok(())
        } else {
            Err(IslandAuthzError::InsufficientAccess {
                actor,
                access: IslandAccess::from(&access),
                group: self.group_id,
            })
        }
    }

    fn require_member(
        &self,
        member: IslandMemberId,
    ) -> Result<Access<IslandMemberCondition>, IslandAuthzError> {
        self.access(member).ok_or(IslandAuthzError::NotMember {
            member,
            group: self.group_id,
        })
    }

    #[cfg(test)]
    fn add_member(
        &mut self,
        manager: IslandMemberId,
        member: IslandMemberKeyBinding,
        role: IslandMemberRole,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        if member.island() != &self.island {
            return Err(IslandAuthzError::WrongIsland {
                expected: self.island.clone(),
                actual: member.island().clone(),
            });
        }
        let member_id = member.member_id();
        let operation = self.add_member_operation(manager, member_id, role)?;
        let change = IslandAuthChange {
            operation_id: operation.id,
            actor: manager,
        };
        self.bindings.insert(member_id, member);
        Ok(change)
    }

    fn append_group_operation(
        &mut self,
        actor: IslandMemberId,
        action: GroupAction<AuthId, IslandMemberCondition>,
    ) -> Result<IslandAuthOperation, IslandAuthzError> {
        let operation = self.next_operation(actor, action)?;
        self.process_imported(operation.clone())?;
        Ok(operation)
    }

    fn next_operation(
        &mut self,
        actor: IslandMemberId,
        action: GroupAction<AuthId, IslandMemberCondition>,
    ) -> Result<IslandAuthOperation, IslandAuthzError> {
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or(IslandAuthzError::OperationSequenceOverflow(sequence))?;
        let dependencies = self.current_heads();
        let actor_id = actor.auth_id();
        let sequence_bytes = sequence.get().to_be_bytes();
        let action_hash = self.next_operation_action_hash(&action);
        let id = IslandOperationId::from_parts(
            b"ployz:island-operation",
            &[&actor_id.0, &sequence_bytes, &action_hash],
        );
        Ok(IslandAuthOperation {
            id,
            author: actor_id,
            dependencies: dependencies.iter().copied().collect(),
            group_id: self.group_id.auth_id(),
            action,
        })
    }

    fn next_operation_action_hash(
        &self,
        action: &GroupAction<AuthId, IslandMemberCondition>,
    ) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hash_payload(&mut hasher, self.group_id.auth_id(), action);
        *hasher.finalize().as_bytes()
    }

    fn process_imported(
        &mut self,
        operation: IslandAuthOperation,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        let Some(state) = self.state.take() else {
            return Err(IslandAuthzError::StateUnavailable);
        };
        let actor = IslandMemberId(operation.author());
        let operation_id = operation.id();
        let original_state = state.clone();
        let state = match AuthCrdt::process(state, &operation) {
            Ok(state) => state,
            Err(error) => {
                self.state = Some(original_state);
                return Err(IslandAuthzError::from_crdt(error));
            }
        };
        self.state = Some(state);
        Ok(IslandAuthChange {
            operation_id,
            actor,
        })
    }

    fn access(&self, member: IslandMemberId) -> Option<Access<IslandMemberCondition>> {
        let state = self.state.as_ref()?;
        state
            .members(self.group_id.auth_id())
            .into_iter()
            .find_map(|(member_id, access)| (member_id == member.auth_id()).then_some(access))
    }

    fn current_heads(&self) -> BTreeSet<IslandOperationId> {
        self.state
            .as_ref()
            .map(|state| state.heads().into_iter().collect())
            .unwrap_or_default()
    }

    fn validate_signed(&self, signed: &IslandSignedOperation) -> Result<(), IslandAuthzError> {
        if signed.operation.author() != signed.signer.auth_id() {
            return Err(IslandAuthzError::SignerMismatch {
                signer: signed.signer,
                author: IslandMemberId(signed.operation.author()),
            });
        }
        let Some(current_binding) = self.bindings.get(&signed.signer) else {
            return Err(IslandAuthzError::MissingBinding(signed.signer));
        };
        let payload = membership_operation_payload(
            &signed.operation,
            signed.signer,
            signed.introduced_binding.as_ref(),
        );
        if !current_binding
            .author_key()
            .public_key()
            .verify(&payload.0, &signed.signature)
        {
            return Err(IslandAuthzError::InvalidSignature(signed.operation_id()));
        }
        if signed.operation.group_id() != self.group_id.auth_id() {
            return Err(IslandAuthzError::WrongGroup {
                expected: self.group_id,
                actual: IslandGroupId(signed.operation.group_id()),
            });
        }
        validate_supported_action_shape(&signed.operation.action)?;
        match &signed.operation.action {
            GroupAction::Add { member, .. } => {
                let Some(binding) = signed.introduced_binding.as_ref() else {
                    return Err(IslandAuthzError::MissingIntroducedBinding(IslandMemberId(
                        member.id(),
                    )));
                };
                if binding.island() != &self.island {
                    return Err(IslandAuthzError::WrongIsland {
                        expected: self.island.clone(),
                        actual: binding.island().clone(),
                    });
                }
                if binding.member_id().auth_id() != member.id() {
                    return Err(IslandAuthzError::IntroducedBindingMismatch {
                        expected: IslandMemberId(member.id()),
                        actual: binding.member_id(),
                    });
                }
            }
            GroupAction::Promote { member, .. } | GroupAction::Demote { member, .. } => {
                if signed.introduced_binding.is_some() {
                    return Err(IslandAuthzError::UnexpectedIntroducedBinding(
                        signed.operation_id(),
                    ));
                }
                let member_id = IslandMemberId(member.id());
                if !self.bindings.contains_key(&member_id) {
                    return Err(IslandAuthzError::MissingBinding(member_id));
                }
            }
            GroupAction::Create { .. } | GroupAction::Remove { .. } => {
                if signed.introduced_binding.is_some() {
                    return Err(IslandAuthzError::UnexpectedIntroducedBinding(
                        signed.operation_id(),
                    ));
                }
            }
        }
        Ok(())
    }
}

fn validate_supported_action_shape(
    action: &GroupAction<AuthId, IslandMemberCondition>,
) -> Result<(), IslandAuthzError> {
    match action {
        GroupAction::Create { initial_members } => {
            for (member, _) in initial_members {
                reject_nested_group_member(*member)?;
            }
        }
        GroupAction::Add { member, .. }
        | GroupAction::Remove { member }
        | GroupAction::Promote { member, .. }
        | GroupAction::Demote { member, .. } => reject_nested_group_member(*member)?,
    }
    Ok(())
}

fn reject_nested_group_member(member: GroupMember<AuthId>) -> Result<(), IslandAuthzError> {
    match member {
        GroupMember::Individual(_) => Ok(()),
        GroupMember::Group(id) => Err(IslandAuthzError::NestedGroupsUnsupported(IslandMemberId(
            id,
        ))),
    }
}

fn membership_operation_payload(
    operation: &IslandAuthOperation,
    signer: IslandMemberId,
    introduced_binding: Option<&IslandMemberKeyBinding>,
) -> IslandMembershipPayload {
    let mut hasher = blake3::Hasher::new();
    hash_bytes(&mut hasher, b"ployz:p2panda-authz-membership-signature-v1");
    hash_auth_id(&mut hasher, signer.auth_id());
    hash_operation_id(&mut hasher, operation.id());
    hash_auth_id(&mut hasher, operation.author());
    let dependencies = operation.dependencies();
    hash_u64(&mut hasher, dependencies.len() as u64);
    for dependency in dependencies {
        hash_operation_id(&mut hasher, dependency);
    }
    hash_payload(&mut hasher, operation.group_id(), &operation.action);
    match introduced_binding {
        Some(binding) => {
            hasher.update(&[1]);
            hash_str(&mut hasher, binding.island().as_str());
            hash_str(&mut hasher, binding.principal().as_str());
            hash_u64(&mut hasher, binding.epoch().get());
            hash_bytes(&mut hasher, &binding.author_key().as_bytes());
            hash_auth_id(&mut hasher, binding.member_id().auth_id());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    IslandMembershipPayload(*hasher.finalize().as_bytes())
}

fn hash_payload(
    hasher: &mut blake3::Hasher,
    group_id: AuthId,
    action: &GroupAction<AuthId, IslandMemberCondition>,
) {
    hash_auth_id(hasher, group_id);
    match action {
        GroupAction::Create { initial_members } => {
            hasher.update(&[0]);
            hash_u64(hasher, initial_members.len() as u64);
            for (member, access) in initial_members {
                hash_member(hasher, *member);
                hash_access(hasher, access);
            }
        }
        GroupAction::Add { member, access } => {
            hasher.update(&[1]);
            hash_member(hasher, *member);
            hash_access(hasher, access);
        }
        GroupAction::Remove { member } => {
            hasher.update(&[2]);
            hash_member(hasher, *member);
        }
        GroupAction::Promote { member, access } => {
            hasher.update(&[3]);
            hash_member(hasher, *member);
            hash_access(hasher, access);
        }
        GroupAction::Demote { member, access } => {
            hasher.update(&[4]);
            hash_member(hasher, *member);
            hash_access(hasher, access);
        }
    }
}

fn hash_member(hasher: &mut blake3::Hasher, member: GroupMember<AuthId>) {
    match member {
        GroupMember::Individual(id) => {
            hasher.update(&[0]);
            hasher.update(&id.0);
        }
        GroupMember::Group(id) => {
            hasher.update(&[1]);
            hasher.update(&id.0);
        }
    }
}

fn hash_access(hasher: &mut blake3::Hasher, access: &Access<IslandMemberCondition>) {
    let level = match access.level {
        AccessLevel::Pull => 0,
        AccessLevel::Read => 1,
        AccessLevel::Write => 2,
        AccessLevel::Manage => 3,
    };
    hasher.update(&[level]);
    match access.conditions {
        Some(IslandMemberCondition::ReplicaImporter) => hasher.update(&[1]),
        None => hasher.update(&[0]),
    };
}

#[derive(Debug, Error)]
pub enum IslandAuthzError {
    #[error("member {actor} has {access:?}, but manager access is required for group {group}")]
    InsufficientAccess {
        actor: IslandMemberId,
        access: IslandAccess,
        group: IslandGroupId,
    },
    #[error("member {member} already belongs to group {group}")]
    AlreadyMember {
        member: IslandMemberId,
        group: IslandGroupId,
    },
    #[error("member {member} does not belong to group {group}")]
    NotMember {
        member: IslandMemberId,
        group: IslandGroupId,
    },
    #[error("member {member} already has access {access:?} in group {group}")]
    SameAccess {
        member: IslandMemberId,
        access: IslandAccess,
        group: IslandGroupId,
    },
    #[error("p2panda-auth rejected group graph update: {reason}")]
    GroupGraphRejected { reason: String },
    #[error("island operation sequence overflow after {0}")]
    OperationSequenceOverflow(NonZeroU64),
    #[error("membership state was unavailable during mutation")]
    StateUnavailable,
    #[error("member binding is for island {actual}, expected {expected}")]
    WrongIsland {
        expected: IslandId,
        actual: IslandId,
    },
    #[error("membership operation signer {signer} does not match author {author}")]
    SignerMismatch {
        signer: IslandMemberId,
        author: IslandMemberId,
    },
    #[error("member {0} has no durable key binding")]
    MissingBinding(IslandMemberId),
    #[error("invalid membership operation signature for operation {0}")]
    InvalidSignature(IslandOperationId),
    #[error("membership operation is for group {actual}, expected {expected}")]
    WrongGroup {
        expected: IslandGroupId,
        actual: IslandGroupId,
    },
    #[error("membership operation uses unsupported nested group member {0}")]
    NestedGroupsUnsupported(IslandMemberId),
    #[error("membership operation did not carry a key binding for introduced member {0}")]
    MissingIntroducedBinding(IslandMemberId),
    #[error("introduced key binding points at {actual}, expected {expected}")]
    IntroducedBindingMismatch {
        expected: IslandMemberId,
        actual: IslandMemberId,
    },
    #[error("membership operation {0} unexpectedly carried an introduced key binding")]
    UnexpectedIntroducedBinding(IslandOperationId),
    #[error("p2panda membership operation failed validation: {message}")]
    InvalidPandaOperation { message: String },
    #[error("membership payload encoding failed: {message}")]
    Encode { message: String },
    #[error("island {island} has no durable membership operations")]
    EmptyMembershipLog { island: IslandId },
    #[error("root create is not anchored by the configured root member {0}")]
    UnanchoredRootCreate(IslandMemberId),
    #[error("root authority does not match signer/member {0}")]
    RootAuthorityMismatch(IslandMemberId),
    #[error("member private key does not match durable binding for {0}")]
    MemberKeyMismatch(IslandMemberId),
}

impl IslandAuthzError {
    fn from_crdt(value: AuthCrdtError) -> Self {
        Self::GroupGraphRejected {
            reason: value.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IslandAccess {
    level: IslandAccessLevel,
    condition: Option<IslandMemberCondition>,
}

impl IslandAccess {
    #[must_use]
    pub fn level(&self) -> IslandAccessLevel {
        self.level
    }

    #[must_use]
    pub fn condition(&self) -> Option<IslandMemberCondition> {
        self.condition
    }
}

impl From<&Access<IslandMemberCondition>> for IslandAccess {
    fn from(value: &Access<IslandMemberCondition>) -> Self {
        let level = match value.level {
            AccessLevel::Pull => IslandAccessLevel::Pull,
            AccessLevel::Read => IslandAccessLevel::Read,
            AccessLevel::Write => IslandAccessLevel::Write,
            AccessLevel::Manage => IslandAccessLevel::Manage,
        };
        Self {
            level,
            condition: value.conditions,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IslandAccessLevel {
    Pull,
    Read,
    Write,
    Manage,
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2panda_auth::processor::{GroupsArgs, GroupsOperation, GroupsProcessor};
    use p2panda_core::test_utils::TestLog;
    use p2panda_core::{Extension, Hash, Header, Topic, VerifyingKey};
    use p2panda_store::groups::GroupsStore;
    use p2panda_store::{SqliteStore, Transaction};

    fn island() -> IslandId {
        IslandId::new("default")
    }

    fn member(seed: &str, island: &IslandId, epoch: u64) -> IslandMemberKeyBinding {
        member_with_private_key(seed, island, epoch).0
    }

    fn member_with_private_key(
        seed: &str,
        island: &IslandId,
        epoch: u64,
    ) -> (IslandMemberKeyBinding, SigningKey) {
        let epoch = NonZeroU64::new(epoch).expect("test epochs are non-zero");
        let private_key = member_private_key(seed, epoch.get());
        let binding = IslandMemberKeyBinding::new(
            island.clone(),
            PrincipalId::new(seed),
            IslandMemberEpoch::new(epoch),
            IslandMemberAuthorKey::from_private_key(&private_key),
        );
        (binding, private_key)
    }

    fn member_private_key(seed: &str, epoch: u64) -> SigningKey {
        let bytes = blake3::hash(format!("{seed}:{epoch}").as_bytes());
        SigningKey::from_bytes(bytes.as_bytes())
    }

    fn new_authz() -> (IslandAuthz, IslandMemberId) {
        let island = island();
        let root = member("root", &island, 1);
        let root_id = root.member_id();
        let authz = IslandAuthz::create(island, root).expect("root group should be created");
        (authz, root_id)
    }

    fn operation_id(id: u64) -> IslandOperationId {
        IslandOperationId::from_parts(b"ployz:test-island-operation", &[&id.to_be_bytes()])
    }

    fn group_operation(
        author: IslandMemberId,
        id: u64,
        group: IslandGroupId,
        dependencies: &BTreeSet<IslandOperationId>,
        action: GroupAction<AuthId, IslandMemberCondition>,
    ) -> IslandAuthOperation {
        IslandAuthOperation {
            id: operation_id(id),
            author: author.auth_id(),
            dependencies: dependencies.iter().copied().collect(),
            group_id: group.auth_id(),
            action,
        }
    }

    fn add_operation(
        author: IslandMemberId,
        id: u64,
        group: IslandGroupId,
        added: IslandMemberId,
        role: IslandMemberRole,
        dependencies: &BTreeSet<IslandOperationId>,
    ) -> IslandAuthOperation {
        group_operation(
            author,
            id,
            group,
            dependencies,
            GroupAction::Add {
                member: GroupMember::Individual(added.auth_id()),
                access: role.into_access(),
            },
        )
    }

    fn remove_operation(
        author: IslandMemberId,
        id: u64,
        group: IslandGroupId,
        removed: IslandMemberId,
        dependencies: &BTreeSet<IslandOperationId>,
    ) -> IslandAuthOperation {
        group_operation(
            author,
            id,
            group,
            dependencies,
            GroupAction::Remove {
                member: GroupMember::Individual(removed.auth_id()),
            },
        )
    }

    fn demote_operation(
        author: IslandMemberId,
        id: u64,
        group: IslandGroupId,
        demoted: IslandMemberId,
        role: IslandMemberRole,
        dependencies: &BTreeSet<IslandOperationId>,
    ) -> IslandAuthOperation {
        group_operation(
            author,
            id,
            group,
            dependencies,
            GroupAction::Demote {
                member: GroupMember::Individual(demoted.auth_id()),
                access: role.into_access(),
            },
        )
    }

    fn signed_add_operation(
        authz: &IslandAuthz,
        signer: &IslandMemberKeyBinding,
        signer_private_key: &SigningKey,
        id: u64,
        member: IslandMemberKeyBinding,
        role: IslandMemberRole,
    ) -> IslandSignedOperation {
        let operation = add_operation(
            signer.member_id(),
            id,
            authz.group_id(),
            member.member_id(),
            role,
            &authz.current_heads(),
        );
        IslandSignedOperation::sign(
            operation,
            signer.member_id(),
            signer_private_key,
            Some(member),
        )
    }

    type ProcessorLogId = u64;
    type ProcessorState = GroupCrdtState<
        VerifyingKey,
        Hash,
        GroupsOperation<IslandMemberCondition>,
        IslandMemberCondition,
    >;
    type AuthzProcessor =
        GroupsProcessor<Topic, ProcessorFitExtensions, ProcessorLogId, IslandMemberCondition>;

    const PROCESSOR_LOG_ID: ProcessorLogId = 7;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct ProcessorFitExtensions {
        log_id: ProcessorLogId,
        groups: Option<GroupsArgs<IslandMemberCondition>>,
    }

    impl Extension<GroupsArgs<IslandMemberCondition>> for ProcessorFitExtensions {
        fn extract(header: &Header<Self>) -> Option<GroupsArgs<IslandMemberCondition>> {
            header.extensions.groups.clone()
        }
    }

    impl Extension<ProcessorLogId> for ProcessorFitExtensions {
        fn extract(header: &Header<Self>) -> Option<ProcessorLogId> {
            Some(header.extensions.log_id)
        }
    }

    impl From<GroupsArgs<IslandMemberCondition>> for ProcessorFitExtensions {
        fn from(groups: GroupsArgs<IslandMemberCondition>) -> Self {
            Self {
                log_id: PROCESSOR_LOG_ID,
                groups: Some(groups),
            }
        }
    }

    #[test]
    fn root_creates_island_group_as_manager() {
        let (authz, root) = new_authz();
        assert!(authz.is_active_member(root));
        assert!(authz.can_write_member(root));
        assert!(!authz.can_import_replica(root));
    }

    #[test]
    fn root_binding_must_match_group_island() {
        let root = member("root", &IslandId::new("wrong"), 1);
        assert!(matches!(
            IslandAuthz::create(island(), root),
            Err(IslandAuthzError::WrongIsland { .. })
        ));
    }

    #[test]
    fn signed_membership_operation_adds_writer() {
        let island = island();
        let (root, root_key) = member_with_private_key("root", &island, 1);
        let mut authz =
            IslandAuthz::create(island.clone(), root.clone()).expect("root group should create");
        let writer = member("writer", &island, 1);
        let writer_id = writer.member_id();
        let signed = signed_add_operation(
            &authz,
            &root,
            &root_key,
            100,
            writer,
            IslandMemberRole::Writer,
        );
        authz
            .apply_signed(signed)
            .expect("signed add should be accepted");
        assert!(authz.can_write_member(writer_id));
    }

    #[tokio::test]
    async fn groups_processor_fit_check_uses_verifying_key_hash_identity_model() {
        let store = SqliteStore::temporary().await;
        let processor = AuthzProcessor::new(store.clone());
        let topic = Topic::random();
        let state_id = 41_u64;
        let manager_log = TestLog::new();
        let importer_log = TestLog::new();
        let group_key = SigningKey::generate().verifying_key();

        let create = manager_log.operation(
            &[],
            ProcessorFitExtensions::from(GroupsArgs {
                group_id: group_key,
                action: GroupAction::Create {
                    initial_members: vec![(
                        GroupMember::Individual(manager_log.author()),
                        Access::manage(),
                    )],
                },
                dependencies: Vec::new(),
            }),
        );
        processor
            .process(&state_id, &topic, &create)
            .await
            .expect("processor should store create group operation");

        let add_importer = manager_log.operation(
            &[],
            ProcessorFitExtensions::from(GroupsArgs {
                group_id: group_key,
                action: GroupAction::Add {
                    member: GroupMember::Individual(importer_log.author()),
                    access: ReplicaImportAccess::Read.into_access(),
                },
                dependencies: vec![create.hash],
            }),
        );
        processor
            .process(&state_id, &topic, &add_importer)
            .await
            .expect("processor should store add-member operation");

        let permit = store.begin().await.expect("begin processor store read");
        let state: ProcessorState = store
            .get_groups_state(&state_id)
            .await
            .expect("read processor group state")
            .expect("processor group state should exist");
        store.commit(permit).await.expect("commit processor read");

        let members = state.members(group_key);
        assert_eq!(members.len(), 2);
        assert!(members.contains(&(manager_log.author(), Access::manage())));
        assert!(members.contains(&(
            importer_log.author(),
            ReplicaImportAccess::Read.into_access()
        )));

        // This processor stores group and member identities as p2panda
        // VerifyingKey values, with operation identity fixed to the signed
        // p2panda operation hash. Slice 041 still needs Ployz-owned
        // island/principal/epoch/key bindings and root anchoring above it.
        assert_ne!(
            group_key.as_bytes(),
            &IslandGroupId::from_island(&island()).auth_id().0
        );
    }

    #[tokio::test]
    async fn durable_membership_log_replays_root_and_writer_from_memory_store() {
        let island = island();
        let (root, root_key) = member_with_private_key("root", &island, 1);
        let root_authority = IslandRootAuthority::new(root.clone());
        let mut log = IslandAuthzMemoryLog::new(island.clone());
        let mut authz = log
            .create_root(root.clone(), &root_key)
            .await
            .expect("root create should be stored");

        let writer = member("writer", &island, 1);
        let writer_id = writer.member_id();
        let signed = signed_add_operation(
            &authz,
            &root,
            &root_key,
            100,
            writer,
            IslandMemberRole::Writer,
        );
        log.apply_signed(&mut authz, signed, &root_key)
            .await
            .expect("signed writer add should be stored");

        let reopened_log = IslandAuthzMemoryLog::from_store(island, log.store());
        let reopened = reopened_log
            .replay(&root_authority)
            .await
            .expect("stored membership operations should replay");

        assert!(reopened.can_write_member(root.member_id()));
        assert!(reopened.can_write_member(writer_id));
    }

    #[tokio::test]
    async fn durable_membership_log_rejects_wrong_root_authority_on_replay() {
        let island = island();
        let (root, root_key) = member_with_private_key("root", &island, 1);
        let mut log = IslandAuthzMemoryLog::new(island.clone());
        log.create_root(root, &root_key)
            .await
            .expect("root create should be stored");

        let wrong_root = member("wrong-root", &island, 1);
        let reopened_log = IslandAuthzMemoryLog::from_store(island, log.store());
        let error = match reopened_log
            .replay(&IslandRootAuthority::new(wrong_root))
            .await
        {
            Ok(_) => panic!("wrong root authority should fail closed"),
            Err(error) => error,
        };

        assert!(matches!(error, IslandAuthzError::UnanchoredRootCreate(_)));
    }

    #[tokio::test]
    async fn durable_membership_log_rejects_empty_replay() {
        let island = island();
        let root = member("root", &island, 1);
        let log = IslandAuthzMemoryLog::new(island.clone());
        let error = match log.replay(&IslandRootAuthority::new(root)).await {
            Ok(_) => panic!("empty membership log should fail closed"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            IslandAuthzError::EmptyMembershipLog { island: failed_island }
                if failed_island == island
        ));
    }

    #[tokio::test]
    async fn durable_membership_log_apply_failure_does_not_mutate_authz() {
        let island = island();
        let (root, root_key) = member_with_private_key("root", &island, 1);
        let mut log = IslandAuthzMemoryLog::new(island.clone());
        let mut authz = log
            .create_root(root.clone(), &root_key)
            .await
            .expect("root create should be stored");

        let writer = member("writer", &island, 1);
        let writer_id = writer.member_id();
        let signed = signed_add_operation(
            &authz,
            &root,
            &root_key,
            100,
            writer,
            IslandMemberRole::Writer,
        );
        let wrong_private_key = member_private_key("wrong-root-key", 1);
        let error = match log
            .apply_signed(&mut authz, signed, &wrong_private_key)
            .await
        {
            Ok(_) => panic!("wrong persistence signer should fail"),
            Err(error) => error,
        };

        assert!(matches!(error, IslandAuthzError::InvalidSignature(_)));
        assert!(!authz.can_write_member(writer_id));
    }

    #[tokio::test]
    async fn durable_membership_log_rejects_wrong_island_added_binding_before_persisting() {
        let island = island();
        let (root, root_key) = member_with_private_key("root", &island, 1);
        let mut log = IslandAuthzMemoryLog::new(island.clone());
        let mut authz = log
            .create_root(root.clone(), &root_key)
            .await
            .expect("root create should be stored");
        let wrong_island_writer = member("writer", &IslandId::new("wrong"), 1);
        let wrong_writer_id = wrong_island_writer.member_id();

        let error = log
            .add_writer(&mut authz, &root, &root_key, wrong_island_writer)
            .await
            .expect_err("wrong-island binding should fail before persistence");

        assert!(matches!(error, IslandAuthzError::WrongIsland { .. }));
        assert!(!authz.can_write_member(wrong_writer_id));
        let reopened = IslandAuthzMemoryLog::from_store(island, log.store())
            .replay(&IslandRootAuthority::new(root.clone()))
            .await
            .expect("root-only log should still replay");
        assert!(!reopened.can_write_member(wrong_writer_id));
    }

    #[tokio::test]
    async fn durable_membership_log_rejects_wrong_manager_private_key_before_persisting() {
        let island = island();
        let (root, root_key) = member_with_private_key("root", &island, 1);
        let mut log = IslandAuthzMemoryLog::new(island.clone());
        let mut authz = log
            .create_root(root.clone(), &root_key)
            .await
            .expect("root create should be stored");

        let writer = member("writer", &island, 1);
        let writer_id = writer.member_id();
        let wrong_private_key = member_private_key("wrong-root-key", 1);
        let error = log
            .add_writer(&mut authz, &root, &wrong_private_key, writer)
            .await
            .expect_err("manager private key must match durable manager binding");

        assert!(matches!(error, IslandAuthzError::MemberKeyMismatch(id) if id == root.member_id()));
        assert!(!authz.can_write_member(writer_id));
        let reopened = IslandAuthzMemoryLog::from_store(island, log.store())
            .replay(&IslandRootAuthority::new(root))
            .await
            .expect("root-only log should still replay");
        assert!(!reopened.can_write_member(writer_id));
    }

    #[test]
    fn signed_membership_operation_rejects_substituted_signer_key() {
        let island = island();
        let (root, root_key) = member_with_private_key("root", &island, 1);
        let mut authz =
            IslandAuthz::create(island.clone(), root.clone()).expect("root group should create");
        let writer = member("writer", &island, 1);
        let signed = signed_add_operation(
            &authz,
            &root,
            &member_private_key("substituted-root-key", 1),
            100,
            writer,
            IslandMemberRole::Writer,
        );
        assert_ne!(
            root_key.verifying_key(),
            member_private_key("substituted-root-key", 1).verifying_key()
        );
        assert!(matches!(
            authz.apply_signed(signed),
            Err(IslandAuthzError::InvalidSignature(_))
        ));
    }

    #[test]
    fn signed_membership_operation_rejects_tampered_signature() {
        let island = island();
        let (root, root_key) = member_with_private_key("root", &island, 1);
        let mut authz =
            IslandAuthz::create(island.clone(), root.clone()).expect("root group should create");
        let writer = member("writer", &island, 1);
        let mut signed = signed_add_operation(
            &authz,
            &root,
            &root_key,
            100,
            writer,
            IslandMemberRole::Writer,
        );
        signed.signature = Signature::from_bytes(&[7; 64]);
        assert!(matches!(
            authz.apply_signed(signed),
            Err(IslandAuthzError::InvalidSignature(_))
        ));
    }

    #[test]
    fn signed_membership_operation_rejects_wrong_group() {
        let island = island();
        let (root, root_key) = member_with_private_key("root", &island, 1);
        let mut authz =
            IslandAuthz::create(island.clone(), root.clone()).expect("root group should create");
        let writer = member("writer", &island, 1);
        let operation = add_operation(
            root.member_id(),
            100,
            IslandGroupId::from_island(&IslandId::new("other")),
            writer.member_id(),
            IslandMemberRole::Writer,
            &authz.current_heads(),
        );
        let signed =
            IslandSignedOperation::sign(operation, root.member_id(), &root_key, Some(writer));
        assert!(matches!(
            authz.apply_signed(signed),
            Err(IslandAuthzError::WrongGroup { .. })
        ));
    }

    #[test]
    fn signed_membership_operation_rejects_nested_group_member() {
        let island = island();
        let (root, root_key) = member_with_private_key("root", &island, 1);
        let mut authz =
            IslandAuthz::create(island.clone(), root.clone()).expect("root group should create");
        let nested = member("nested", &island, 1);
        let operation = group_operation(
            root.member_id(),
            100,
            authz.group_id(),
            &authz.current_heads(),
            GroupAction::Add {
                member: GroupMember::Group(nested.member_id().auth_id()),
                access: IslandMemberRole::Writer.into_access(),
            },
        );
        let signed =
            IslandSignedOperation::sign(operation, root.member_id(), &root_key, Some(nested));
        assert!(matches!(
            authz.apply_signed(signed),
            Err(IslandAuthzError::NestedGroupsUnsupported(_))
        ));
    }

    #[test]
    fn signed_membership_operation_requires_added_member_binding() {
        let island = island();
        let (root, root_key) = member_with_private_key("root", &island, 1);
        let mut authz =
            IslandAuthz::create(island.clone(), root.clone()).expect("root group should create");
        let writer = member("writer", &island, 1);
        let operation = add_operation(
            root.member_id(),
            100,
            authz.group_id(),
            writer.member_id(),
            IslandMemberRole::Writer,
            &authz.current_heads(),
        );
        let signed = IslandSignedOperation::sign(operation, root.member_id(), &root_key, None);
        assert!(matches!(
            authz.apply_signed(signed),
            Err(IslandAuthzError::MissingIntroducedBinding(_))
        ));
    }

    #[test]
    fn signed_membership_operation_rejects_remove_with_introduced_binding() {
        let island = island();
        let (root, root_key) = member_with_private_key("root", &island, 1);
        let mut authz =
            IslandAuthz::create(island.clone(), root.clone()).expect("root group should create");
        let writer = member("writer", &island, 1);
        let writer_id = writer.member_id();
        authz
            .add_writer(root.member_id(), writer.clone())
            .expect("root manager should add writer");
        let operation = remove_operation(
            root.member_id(),
            100,
            authz.group_id(),
            writer_id,
            &authz.current_heads(),
        );
        let signed =
            IslandSignedOperation::sign(operation, root.member_id(), &root_key, Some(writer));
        assert!(matches!(
            authz.apply_signed(signed),
            Err(IslandAuthzError::UnexpectedIntroducedBinding(_))
        ));
    }

    #[test]
    fn signed_membership_operation_demotes_without_introduced_binding() {
        let island = island();
        let (root, root_key) = member_with_private_key("root", &island, 1);
        let mut authz =
            IslandAuthz::create(island.clone(), root.clone()).expect("root group should create");
        let writer = member("writer", &island, 1);
        let writer_id = writer.member_id();
        authz
            .add_writer(root.member_id(), writer)
            .expect("root manager should add writer");
        let operation = demote_operation(
            root.member_id(),
            100,
            authz.group_id(),
            writer_id,
            IslandMemberRole::ReplicaImporter(ReplicaImportAccess::Read),
            &authz.current_heads(),
        );
        let signed = IslandSignedOperation::sign(operation, root.member_id(), &root_key, None);
        authz
            .apply_signed(signed)
            .expect("signed demote should be accepted");
        assert!(!authz.can_write_member(writer_id));
        assert!(authz.can_import_replica(writer_id));
    }

    #[test]
    fn rejected_import_keeps_previous_state_available() {
        let island = island();
        let (root, root_key) = member_with_private_key("root", &island, 1);
        let mut authz =
            IslandAuthz::create(island.clone(), root.clone()).expect("root group should create");
        let writer = member("writer", &island, 1);
        let signed = signed_add_operation(
            &authz,
            &root,
            &root_key,
            100,
            writer.clone(),
            IslandMemberRole::Writer,
        );
        authz
            .apply_signed(signed.clone())
            .expect("first signed add should be accepted");
        assert!(matches!(
            authz.apply_signed(signed),
            Err(IslandAuthzError::GroupGraphRejected { .. })
        ));
        assert!(authz.can_write_member(root.member_id()));
        let next_writer = member("next-writer", &island, 1);
        authz
            .add_writer(root.member_id(), next_writer.clone())
            .expect("state should remain usable after rejected import");
        assert!(authz.can_write_member(next_writer.member_id()));
    }

    #[test]
    fn manager_adds_writer() {
        let (mut authz, root) = new_authz();
        let writer = member("writer", authz.island(), 1);
        let writer_id = writer.member_id();
        let change = authz
            .add_writer(root, writer)
            .expect("root manager should add writer");
        assert_eq!(change.actor(), root);
        assert!(authz.is_active_member(writer_id));
        assert!(authz.can_write_member(writer_id));
        assert!(!authz.can_import_replica(writer_id));
    }

    #[test]
    fn manager_demotes_writer_to_replica_importer() {
        let (mut authz, root) = new_authz();
        let writer = member("writer", authz.island(), 1);
        let writer_id = writer.member_id();
        authz
            .add_writer(root, writer)
            .expect("root manager should add writer");
        authz
            .demote_member(
                root,
                writer_id,
                IslandMemberRole::ReplicaImporter(ReplicaImportAccess::Read),
            )
            .expect("root manager should demote writer");
        assert!(!authz.can_write_member(writer_id));
        assert!(authz.can_import_replica(writer_id));
    }

    #[test]
    fn non_manager_cannot_add_or_remove_members() {
        let (mut authz, root) = new_authz();
        let writer = member("writer", authz.island(), 1);
        let writer_id = writer.member_id();
        authz
            .add_writer(root, writer)
            .expect("root manager should add writer");
        let target = member("target", authz.island(), 1);
        assert!(matches!(
            authz.add_writer(writer_id, target),
            Err(IslandAuthzError::InsufficientAccess { .. })
        ));
        assert!(matches!(
            authz.remove_member(writer_id, root),
            Err(IslandAuthzError::InsufficientAccess { .. })
        ));
    }

    #[test]
    fn removed_writer_loses_write_access() {
        let (mut authz, root) = new_authz();
        let writer = member("writer", authz.island(), 1);
        let writer_id = writer.member_id();
        authz
            .add_writer(root, writer)
            .expect("root manager should add writer");
        authz
            .remove_member(root, writer_id)
            .expect("root manager should remove writer");
        assert!(!authz.is_active_member(writer_id));
        assert!(!authz.can_write_member(writer_id));
    }

    #[test]
    fn removed_replica_loses_import_access() {
        let (mut authz, root) = new_authz();
        let replica = member("replica", authz.island(), 1);
        let replica_id = replica.member_id();
        authz
            .add_replica_importer(root, replica, ReplicaImportAccess::Read)
            .expect("root manager should add replica importer");
        authz
            .remove_member(root, replica_id)
            .expect("root manager should remove replica importer");
        assert!(!authz.is_active_member(replica_id));
        assert!(!authz.can_import_replica(replica_id));
    }

    #[test]
    fn replica_importer_is_not_a_writer() {
        let (mut authz, root) = new_authz();
        let replica = member("replica", authz.island(), 1);
        let replica_id = replica.member_id();
        authz
            .add_replica_importer(root, replica, ReplicaImportAccess::Pull)
            .expect("root manager should add replica importer");
        assert!(authz.is_active_member(replica_id));
        assert!(authz.can_import_replica(replica_id));
        assert!(!authz.can_write_member(replica_id));
    }

    #[test]
    fn readd_with_new_epoch_and_key_replaces_old_key_binding() {
        let (mut authz, root) = new_authz();
        let island = authz.island().clone();
        let writer_v1 = member("writer", &island, 1);
        let writer_v1_id = writer_v1.member_id();
        authz
            .add_writer(root, writer_v1)
            .expect("root manager should add writer v1");
        authz
            .remove_member(root, writer_v1_id)
            .expect("root manager should remove writer v1");
        let writer_v2 = member("writer", &island, 2);
        let writer_v2_id = writer_v2.member_id();
        authz
            .add_writer(root, writer_v2)
            .expect("root manager should add writer v2");
        assert!(!authz.is_active_member(writer_v1_id));
        assert!(!authz.can_write_member(writer_v1_id));
        assert!(authz.is_active_member(writer_v2_id));
        assert!(authz.can_write_member(writer_v2_id));
        assert_ne!(writer_v1_id, writer_v2_id);
    }

    #[test]
    fn concurrent_remove_filters_removed_manager_operation() {
        let (mut authz, root) = new_authz();
        let island = authz.island().clone();
        let manager = member("manager", &island, 1);
        let manager_id = manager.member_id();
        authz
            .add_manager(root, manager)
            .expect("root manager should add second manager");
        let target = member("target", &island, 1);
        let target_id = target.member_id();
        let group = authz.group_id();
        let dependencies = authz.current_heads();
        let remove_manager = remove_operation(root, 100, group, manager_id, &dependencies);
        let manager_adds_target = add_operation(
            manager_id,
            101,
            group,
            target_id,
            IslandMemberRole::Writer,
            &dependencies,
        );
        authz
            .process_imported(remove_manager)
            .expect("concurrent remove should process");
        authz
            .process_imported(manager_adds_target)
            .expect("concurrent add should be accepted into the graph");
        assert!(!authz.is_active_member(manager_id));
        assert!(!authz.is_active_member(target_id));
        assert!(!authz.can_write_member(target_id));
    }

    #[test]
    fn concurrent_manager_removals_remove_both_managers() {
        let (mut authz, root) = new_authz();
        let island = authz.island().clone();
        let alice = member("alice", &island, 1);
        let alice_id = alice.member_id();
        let bob = member("bob", &island, 1);
        let bob_id = bob.member_id();
        authz
            .add_manager(root, alice)
            .expect("root manager should add alice");
        authz
            .add_manager(root, bob)
            .expect("root manager should add bob");
        let group = authz.group_id();
        let dependencies = authz.current_heads();
        let alice_removes_bob = remove_operation(alice_id, 100, group, bob_id, &dependencies);
        let bob_removes_alice = remove_operation(bob_id, 101, group, alice_id, &dependencies);
        authz
            .process_imported(alice_removes_bob)
            .expect("alice remove should process");
        authz
            .process_imported(bob_removes_alice)
            .expect("bob remove should process");
        assert!(!authz.is_active_member(alice_id));
        assert!(!authz.is_active_member(bob_id));
        assert!(authz.is_active_member(root));
    }

    #[test]
    fn concurrent_manager_removal_and_demotion_cycle_removes_both_managers() {
        let (mut authz, root) = new_authz();
        let island = authz.island().clone();
        let alice = member("alice", &island, 1);
        let alice_id = alice.member_id();
        let bob = member("bob", &island, 1);
        let bob_id = bob.member_id();
        authz
            .add_manager(root, alice)
            .expect("root manager should add alice");
        authz
            .add_manager(root, bob)
            .expect("root manager should add bob");
        let group = authz.group_id();
        let dependencies = authz.current_heads();
        let alice_removes_bob = remove_operation(alice_id, 100, group, bob_id, &dependencies);
        let bob_demotes_alice = demote_operation(
            bob_id,
            101,
            group,
            alice_id,
            IslandMemberRole::Writer,
            &dependencies,
        );
        authz
            .process_imported(alice_removes_bob)
            .expect("alice remove should process");
        authz
            .process_imported(bob_demotes_alice)
            .expect("bob demote should process");
        assert!(!authz.is_active_member(alice_id));
        assert!(!authz.is_active_member(bob_id));
        assert!(authz.is_active_member(root));
    }
}
