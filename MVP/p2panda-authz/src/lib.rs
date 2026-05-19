use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Debug, Display};
use std::num::NonZeroU64;
use std::path::Path;

use mvp_bus::IslandId;
#[cfg(test)]
use mvp_bus::PrincipalId;
use p2panda_auth::group::resolver::StrongRemove;
use p2panda_auth::group::{
    GroupAction, GroupCrdt, GroupCrdtError, GroupCrdtInnerError, GroupCrdtState, GroupMember,
    GroupMembershipError,
};
use p2panda_auth::traits::Operation;
use p2panda_auth::{Access, AccessLevel};
use p2panda_core::cbor::{decode_cbor, encode_cbor};
use p2panda_core::{Body, Hash, Header, Operation as PandaOperation, SigningKey};
use p2panda_core::{Signature, validate_operation};
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod authority_view;
mod identity;
mod store;

pub use authority_view::{
    IslandAccess, IslandAccessLevel, IslandAuthorityMember, IslandAuthoritySnapshot,
};
use identity::*;
pub use identity::{
    IslandGroupId, IslandMemberAuthorKey, IslandMemberEpoch, IslandMemberId,
    IslandMemberKeyBinding, IslandMemberRole, IslandOperationId, ReplicaImportAccess,
};
pub use store::{IslandAuthzMemoryLog, IslandAuthzStore};

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

pub(crate) type IslandMembershipOperation = PandaOperation<IslandMembershipExtensions>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IslandMembershipExtensions {
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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct IslandMembershipLog(String);

impl IslandMembershipLog {
    pub(crate) fn from_island(island: &IslandId) -> Self {
        Self(island.as_str().to_owned())
    }
}

pub(crate) fn build_membership_panda_operation(
    island: &IslandId,
    signed: &IslandSignedOperation,
    signer_private_key: &SigningKey,
    latest: Option<&Header<IslandMembershipExtensions>>,
) -> Result<PandaOperation<IslandMembershipExtensions>, IslandAuthzError> {
    verify_private_key_signed(signed, signer_private_key)?;
    let public_key = signer_private_key.verifying_key();
    let (seq_num, backlink) = latest
        .map(|header| (header.seq_num + 1, Some(header.hash())))
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
        extensions: IslandMembershipExtensions::new(island, signed),
    };
    header.sign(signer_private_key);
    let operation = PandaOperation {
        hash: header.hash(),
        header,
        body: Some(body),
    };
    validate_operation(&operation).map_err(|error| IslandAuthzError::InvalidPandaOperation {
        message: error.to_string(),
    })?;
    Ok(operation)
}

pub(crate) fn signed_from_membership_operation(
    island: &IslandId,
    operation: PandaOperation<IslandMembershipExtensions>,
) -> Result<(Header<IslandMembershipExtensions>, IslandSignedOperation), IslandAuthzError> {
    validate_operation(&operation).map_err(|error| IslandAuthzError::InvalidPandaOperation {
        message: error.to_string(),
    })?;
    if operation.header.extensions.island != island.as_str() {
        return Err(IslandAuthzError::WrongIsland {
            expected: island.clone(),
            actual: IslandId::new(operation.header.extensions.island.clone()),
        });
    }
    let expected_group = IslandGroupId::from_island(island);
    if operation.header.extensions.group != expected_group.auth_id() {
        return Err(IslandAuthzError::WrongGroup {
            expected: expected_group,
            actual: IslandGroupId(operation.header.extensions.group),
        });
    }
    let body = operation
        .body
        .ok_or(IslandAuthzError::MissingMembershipPayload {
            operation: operation.hash,
        })?;
    let body_bytes = body.to_bytes();
    let signed: IslandSignedOperation =
        decode_cbor(&body_bytes[..]).map_err(|error| IslandAuthzError::Decode {
            message: error.to_string(),
        })?;
    if signed.signer().auth_id() != operation.header.extensions.actor {
        return Err(IslandAuthzError::SignerMismatch {
            signer: signed.signer(),
            author: IslandMemberId(operation.header.extensions.actor),
        });
    }
    Ok((operation.header, signed))
}

pub(crate) fn replay_signed_membership_operations(
    island: &IslandId,
    root_authority: &IslandRootAuthority,
    mut signed_operations: Vec<(Header<IslandMembershipExtensions>, IslandSignedOperation)>,
) -> Result<IslandAuthz, IslandAuthzError> {
    if root_authority.binding().island() != island {
        return Err(IslandAuthzError::WrongIsland {
            expected: island.clone(),
            actual: root_authority.binding().island().clone(),
        });
    }
    if signed_operations.is_empty() {
        return Err(IslandAuthzError::EmptyMembershipLog {
            island: island.clone(),
        });
    }
    let all_operation_ids = signed_operations
        .iter()
        .map(|(_, signed)| signed.operation_id())
        .collect::<BTreeSet<_>>();
    let root_index = signed_operations
        .iter()
        .position(|(_, signed)| {
            signed.signer == root_authority.binding().member_id()
                && matches!(signed.operation.action, GroupAction::Create { .. })
        })
        .ok_or_else(|| {
            IslandAuthzError::UnanchoredRootCreate(root_authority.binding().member_id())
        })?;
    let (root_header, root_signed) = signed_operations.remove(root_index);
    validate_root_anchor(island, root_authority.binding(), &root_header, &root_signed)?;
    let mut authz = IslandAuthz::empty_with_root(island.clone(), root_authority.binding().clone())?;
    authz.apply_signed_from_panda_header(&root_header, root_signed.clone())?;
    let mut applied_operation_ids = BTreeSet::from([root_signed.operation_id()]);

    while !signed_operations.is_empty() {
        let starting_len = signed_operations.len();
        let mut waiting = Vec::new();
        for (header, signed) in signed_operations {
            let dependencies = signed.operation.dependencies();
            if dependencies
                .iter()
                .all(|dependency| applied_operation_ids.contains(dependency))
            {
                let operation_id = signed.operation_id();
                authz.apply_signed_from_panda_header(&header, signed)?;
                applied_operation_ids.insert(operation_id);
            } else {
                waiting.push((header, signed));
            }
        }
        if waiting.len() == starting_len {
            let (_, signed) = waiting
                .first()
                .expect("non-empty waiting set follows non-empty signed operation set");
            if let Some(missing) = signed
                .operation
                .dependencies()
                .into_iter()
                .find(|dependency| !all_operation_ids.contains(dependency))
            {
                return Err(IslandAuthzError::DependencyMissing {
                    operation: signed.operation_id(),
                    dependency: missing,
                });
            }
            return Err(IslandAuthzError::DependencyCycle {
                operation: signed.operation_id(),
            });
        }
        signed_operations = waiting;
    }
    Ok(authz)
}

fn validate_root_anchor(
    island: &IslandId,
    root: &IslandMemberKeyBinding,
    header: &Header<IslandMembershipExtensions>,
    signed: &IslandSignedOperation,
) -> Result<(), IslandAuthzError> {
    if root.island() != island {
        return Err(IslandAuthzError::WrongIsland {
            expected: island.clone(),
            actual: root.island().clone(),
        });
    }
    if signed.signer != root.member_id() || signed.operation.author() != root.member_id().auth_id()
    {
        return Err(IslandAuthzError::UnanchoredRootCreate(root.member_id()));
    }
    if !matches!(signed.operation.action, GroupAction::Create { .. }) {
        return Err(IslandAuthzError::UnanchoredRootCreate(root.member_id()));
    }
    if !signed.operation.dependencies().is_empty() {
        return Err(IslandAuthzError::UnanchoredRootCreate(root.member_id()));
    }
    validate_panda_envelope_author(root, header)?;
    let signature_payload = membership_operation_payload(&signed.operation, signed.signer, None);
    if !root
        .author_key()
        .public_key()
        .verify(&signature_payload.0, &signed.signature)
    {
        return Err(IslandAuthzError::InvalidSignature(signed.operation_id()));
    }
    Ok(())
}

fn validate_panda_envelope_author(
    binding: &IslandMemberKeyBinding,
    header: &Header<IslandMembershipExtensions>,
) -> Result<(), IslandAuthzError> {
    if header.verifying_key == binding.author_key().public_key() {
        Ok(())
    } else {
        Err(IslandAuthzError::PandaEnvelopeAuthorMismatch {
            signer: binding.member_id(),
        })
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

pub(crate) fn validate_membership_mutation_for_island(
    island: &IslandId,
    authz: &IslandAuthz,
    manager: &IslandMemberKeyBinding,
    manager_private_key: &SigningKey,
) -> Result<(), IslandAuthzError> {
    if authz.island() != island {
        return Err(IslandAuthzError::WrongIsland {
            expected: island.clone(),
            actual: authz.island().clone(),
        });
    }
    if manager.island() != island {
        return Err(IslandAuthzError::WrongIsland {
            expected: island.clone(),
            actual: manager.island().clone(),
        });
    }
    if manager.author_key().public_key() != manager_private_key.verifying_key() {
        return Err(IslandAuthzError::MemberKeyMismatch(manager.member_id()));
    }
    Ok(())
}

pub(crate) fn prepare_sqlite_parent(path: &Path) -> Result<(), IslandAuthzError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|error| IslandAuthzError::Store {
            message: error.to_string(),
        })?;
    }
    Ok(())
}

pub(crate) fn sqlite_url(path: &Path) -> String {
    format!("sqlite://{}", path.display())
}

pub(crate) fn store_error(error: impl Display) -> IslandAuthzError {
    IslandAuthzError::Store {
        message: error.to_string(),
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

    fn apply_signed_from_panda_header(
        &mut self,
        header: &Header<IslandMembershipExtensions>,
        signed: IslandSignedOperation,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        self.validate_panda_envelope_author(header, &signed)?;
        self.apply_signed(signed)
    }

    fn validate_panda_envelope_author(
        &self,
        header: &Header<IslandMembershipExtensions>,
        signed: &IslandSignedOperation,
    ) -> Result<(), IslandAuthzError> {
        let Some(binding) = self.bindings.get(&signed.signer) else {
            return Err(IslandAuthzError::MissingBinding(signed.signer));
        };
        validate_panda_envelope_author(binding, header)
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
    #[error("p2panda envelope author key does not match durable key binding for {signer}")]
    PandaEnvelopeAuthorMismatch { signer: IslandMemberId },
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
    #[error("membership payload decoding failed: {message}")]
    Decode { message: String },
    #[error("membership store failed: {message}")]
    Store { message: String },
    #[error("membership operation {operation} has no payload")]
    MissingMembershipPayload { operation: Hash },
    #[error("membership operation {0} already exists with different payload")]
    DuplicateMembershipOperation(IslandOperationId),
    #[error("membership operation {operation} depends on missing operation {dependency}")]
    DependencyMissing {
        operation: IslandOperationId,
        dependency: IslandOperationId,
    },
    #[error("membership operation {operation} is stuck behind a dependency cycle")]
    DependencyCycle { operation: IslandOperationId },
    #[error("membership graph references missing states {dependencies:?}")]
    DependencyGraphMissing {
        dependencies: Vec<IslandOperationId>,
    },
    #[error("membership operation {operation} was authored by unauthorized member {actor}")]
    UnauthorizedMembershipMutation {
        operation: IslandOperationId,
        actor: IslandMemberId,
    },
    #[error("island {island} has no durable membership operations")]
    EmptyMembershipLog { island: IslandId },
    #[error("island {island} already has a pinned root membership operation")]
    RootAlreadyPinned { island: IslandId },
    #[error("root create is not anchored by the configured root member {0}")]
    UnanchoredRootCreate(IslandMemberId),
    #[error("root authority does not match signer/member {0}")]
    RootAuthorityMismatch(IslandMemberId),
    #[error("member private key does not match durable binding for {0}")]
    MemberKeyMismatch(IslandMemberId),
}

impl IslandAuthzError {
    fn from_crdt(value: AuthCrdtError) -> Self {
        match value {
            GroupCrdtError::Inner(GroupCrdtInnerError::StatesNotFound(dependencies)) => {
                Self::DependencyGraphMissing { dependencies }
            }
            GroupCrdtError::DuplicateOperation(operation, _) => {
                Self::DuplicateMembershipOperation(operation)
            }
            GroupCrdtError::StateChangeError(
                operation,
                GroupMembershipError::InsufficientAccess(actor)
                | GroupMembershipError::InactiveActor(actor)
                | GroupMembershipError::UnrecognisedActor(actor),
            ) => Self::UnauthorizedMembershipMutation {
                operation,
                actor: IslandMemberId(actor.id()),
            },
            error => Self::GroupGraphRejected {
                reason: error.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests;
