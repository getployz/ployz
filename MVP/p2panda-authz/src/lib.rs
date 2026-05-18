use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Debug, Display, Formatter};
use std::num::NonZeroU64;

use mvp_bus::{IslandId, PrincipalId};
use p2panda_auth::group::resolver::StrongRemove;
#[cfg(test)]
use p2panda_auth::group::{GroupAction, GroupMember};
use p2panda_auth::group::{
    GroupControlMessage, GroupCrdt, GroupCrdtError, GroupCrdtState, Groups, GroupsError,
};
use p2panda_auth::traits::{
    Conditions, Groups as GroupsTrait, IdentityHandle, Operation, OperationId, Orderer,
};
use p2panda_auth::{Access, AccessLevel};
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

#[cfg(test)]
fn hash_str(hasher: &mut blake3::Hasher, value: &str) {
    hash_bytes(hasher, value.as_bytes());
}

fn hash_u64(hasher: &mut blake3::Hasher, value: u64) {
    hasher.update(&value.to_be_bytes());
}

#[cfg(test)]
fn hash_auth_id(hasher: &mut blake3::Hasher, value: AuthId) {
    hasher.update(&value.0);
}

#[cfg(test)]
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
    #[must_use]
    pub fn from_parts(tag: &'static [u8], parts: &[&[u8]]) -> Self {
        Self(AuthId::derive(tag, parts))
    }

    #[cfg(test)]
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
pub struct IslandMemberKey([u8; 32]);

impl IslandMemberKey {
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn from_seed(seed: impl AsRef<[u8]>) -> Self {
        Self(*blake3::hash(seed.as_ref()).as_bytes())
    }

    #[must_use]
    pub fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IslandMemberKeyBinding {
    island: IslandId,
    principal: PrincipalId,
    epoch: IslandMemberEpoch,
    key: IslandMemberKey,
    member_id: IslandMemberId,
}

impl IslandMemberKeyBinding {
    #[must_use]
    pub fn new(
        island: IslandId,
        principal: PrincipalId,
        epoch: IslandMemberEpoch,
        key: IslandMemberKey,
    ) -> Self {
        let member_id = IslandMemberId(AuthId::derive(
            b"ployz:island-member",
            &[
                island.as_str().as_bytes(),
                principal.as_str().as_bytes(),
                &epoch.get().to_be_bytes(),
                &key.as_bytes(),
            ],
        ));
        Self {
            island,
            principal,
            epoch,
            key,
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
    pub fn key(&self) -> IslandMemberKey {
        self.key
    }

    #[must_use]
    pub fn member_id(&self) -> IslandMemberId {
        self.member_id
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
    payload: GroupControlMessage<AuthId, IslandMemberCondition>,
}

impl Operation<AuthId, IslandOperationId, GroupControlMessage<AuthId, IslandMemberCondition>>
    for IslandAuthOperation
{
    fn id(&self) -> IslandOperationId {
        self.id
    }

    fn author(&self) -> AuthId {
        self.author
    }

    fn dependencies(&self) -> Vec<IslandOperationId> {
        self.dependencies.clone()
    }

    fn payload(&self) -> GroupControlMessage<AuthId, IslandMemberCondition> {
        self.payload.clone()
    }
}

#[derive(Clone, Debug)]
struct IslandOrdererState {
    actor: AuthId,
    next_sequence: NonZeroU64,
    heads: BTreeSet<IslandOperationId>,
}

impl IslandOrdererState {
    fn new(actor: AuthId) -> Self {
        Self {
            actor,
            next_sequence: NonZeroU64::MIN,
            heads: BTreeSet::new(),
        }
    }

    fn set_heads(&mut self, heads: BTreeSet<IslandOperationId>) {
        self.heads = heads;
    }

    fn observe_operation(
        &mut self,
        _operation_id: IslandOperationId,
    ) -> Result<(), IslandAuthzError> {
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct IslandOrderer;

#[derive(Debug, Error)]
enum IslandOrdererError {
    #[error("island operation sequence overflow after {0}")]
    OperationSequenceOverflow(NonZeroU64),
}

impl Orderer<AuthId, IslandOperationId, GroupControlMessage<AuthId, IslandMemberCondition>>
    for IslandOrderer
{
    type State = IslandOrdererState;
    type Operation = IslandAuthOperation;
    type Error = IslandOrdererError;

    fn next_message(
        mut y: Self::State,
        payload: &GroupControlMessage<AuthId, IslandMemberCondition>,
    ) -> Result<(Self::State, Self::Operation), Self::Error> {
        let id = IslandOperationId::from_parts(
            b"ployz:island-operation",
            &[&y.actor.0, &y.next_sequence.get().to_be_bytes()],
        );
        y.next_sequence = y
            .next_sequence
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or(IslandOrdererError::OperationSequenceOverflow(
                y.next_sequence,
            ))?;
        let dependencies = y.heads.iter().copied().collect();
        let operation = IslandAuthOperation {
            id,
            author: y.actor,
            dependencies,
            payload: payload.clone(),
        };
        y.heads.clear();
        y.heads.insert(id);
        Ok((y, operation))
    }

    fn queue(mut y: Self::State, message: &Self::Operation) -> Result<Self::State, Self::Error> {
        for dependency in message.dependencies() {
            y.heads.remove(&dependency);
        }
        y.heads.insert(message.id());
        Ok(y)
    }

    fn next_ready_message(
        y: Self::State,
    ) -> Result<(Self::State, Option<Self::Operation>), Self::Error> {
        Ok((y, None))
    }
}

type AuthResolver =
    StrongRemove<AuthId, IslandOperationId, IslandMemberCondition, IslandAuthOperation>;
type AuthCrdt =
    GroupCrdt<AuthId, IslandOperationId, IslandMemberCondition, AuthResolver, IslandOrderer>;
type AuthState = GroupCrdtState<AuthId, IslandOperationId, IslandMemberCondition, IslandOrderer>;
type AuthGroups =
    Groups<AuthId, IslandOperationId, IslandMemberCondition, AuthResolver, IslandOrderer>;
type AuthGroupsError =
    GroupsError<AuthId, IslandOperationId, IslandMemberCondition, AuthResolver, IslandOrderer>;
type AuthCrdtError =
    GroupCrdtError<AuthId, IslandOperationId, IslandMemberCondition, AuthResolver, IslandOrderer>;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IslandMembershipSignature([u8; 32]);

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IslandSignedOperation {
    operation: IslandAuthOperation,
    signer: IslandMemberId,
    signer_key: IslandMemberKey,
    introduced_binding: Option<IslandMemberKeyBinding>,
    signature: IslandMembershipSignature,
}

#[cfg(test)]
impl IslandSignedOperation {
    fn sign(
        operation: IslandAuthOperation,
        signer: IslandMemberId,
        signer_key: IslandMemberKey,
        introduced_binding: Option<IslandMemberKeyBinding>,
    ) -> Self {
        let signature =
            sign_membership_operation(&operation, signer, signer_key, introduced_binding.as_ref());
        Self {
            operation,
            signer,
            signer_key,
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

pub struct IslandAuthz {
    island: IslandId,
    group_id: IslandGroupId,
    state: Option<AuthState>,
    bindings: BTreeMap<IslandMemberId, IslandMemberKeyBinding>,
    #[cfg(test)]
    operations: BTreeMap<IslandOperationId, IslandSignedOperation>,
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
        let group_id = IslandGroupId::from_island(&island);
        let actor = root.member_id();
        let mut authz = Self {
            island,
            group_id,
            state: Some(AuthCrdt::init(IslandOrdererState::new(actor.auth_id()))),
            bindings: BTreeMap::from([(actor, root)]),
            #[cfg(test)]
            operations: BTreeMap::new(),
        };
        let _change = authz.create_group(actor)?;
        Ok(authz)
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

    pub fn add_manager(
        &mut self,
        manager: IslandMemberId,
        member: IslandMemberKeyBinding,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        self.add_member(manager, member, IslandMemberRole::Manager)
    }

    pub fn add_writer(
        &mut self,
        manager: IslandMemberId,
        member: IslandMemberKeyBinding,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        self.add_member(manager, member, IslandMemberRole::Writer)
    }

    pub fn add_replica_importer(
        &mut self,
        manager: IslandMemberId,
        member: IslandMemberKeyBinding,
        access: ReplicaImportAccess,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        self.add_member(manager, member, IslandMemberRole::ReplicaImporter(access))
    }

    #[cfg(test)]
    pub fn apply_signed(
        &mut self,
        signed: IslandSignedOperation,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        self.validate_signed(&signed)?;
        let introduced_binding = signed.introduced_binding.clone();
        let operation_id = signed.operation_id();
        let change = self.process_imported(signed.operation.clone())?;
        if let Some(binding) = introduced_binding {
            self.bindings.insert(binding.member_id(), binding);
        }
        self.operations.insert(operation_id, signed);
        Ok(change)
    }

    pub fn remove_member(
        &mut self,
        manager: IslandMemberId,
        member: IslandMemberId,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        let group_id = self.group_id.auth_id();
        self.mutate(manager, |groups| {
            groups.remove(group_id, manager.auth_id(), member.auth_id())
        })
    }

    pub fn demote_member(
        &mut self,
        manager: IslandMemberId,
        member: IslandMemberId,
        role: IslandMemberRole,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        let group_id = self.group_id.auth_id();
        self.mutate(manager, |groups| {
            groups.demote(
                group_id,
                manager.auth_id(),
                member.auth_id(),
                role.into_access(),
            )
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

    fn create_group(
        &mut self,
        actor: IslandMemberId,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        let group_id = self.group_id.auth_id();
        self.mutate(actor, |groups| groups.create(group_id, Vec::new()))
    }

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
        let group_id = self.group_id.auth_id();
        let member_id = member.member_id();
        let change = self.mutate(manager, |groups| {
            groups.add(
                group_id,
                manager.auth_id(),
                member_id.auth_id(),
                role.into_access(),
            )
        })?;
        self.bindings.insert(member_id, member);
        Ok(change)
    }

    fn mutate<F>(
        &mut self,
        actor: IslandMemberId,
        operation: F,
    ) -> Result<IslandAuthChange, IslandAuthzError>
    where
        F: FnOnce(&mut AuthGroups) -> Result<IslandAuthOperation, AuthGroupsError>,
    {
        self.with_groups(actor, operation)
    }

    fn with_groups<F>(
        &mut self,
        actor: IslandMemberId,
        operation: F,
    ) -> Result<IslandAuthChange, IslandAuthzError>
    where
        F: FnOnce(&mut AuthGroups) -> Result<IslandAuthOperation, AuthGroupsError>,
    {
        let Some(mut state) = self.state.take() else {
            return Err(IslandAuthzError::StateUnavailable);
        };
        state.orderer_y.actor = actor.auth_id();
        state
            .orderer_y
            .set_heads(state.inner.heads().into_iter().collect());

        let mut groups = AuthGroups::new(actor.auth_id(), state);
        let result = operation(&mut groups);
        self.state = Some(groups.take_state());
        let operation = result?;
        self.observe_operation(operation.id)?;
        Ok(IslandAuthChange {
            operation_id: operation.id,
            actor,
        })
    }

    #[cfg(test)]
    fn process_imported(
        &mut self,
        operation: IslandAuthOperation,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        let Some(state) = self.state.take() else {
            return Err(IslandAuthzError::StateUnavailable);
        };
        let actor = IslandMemberId(operation.author());
        let operation_id = operation.id();
        let state = AuthCrdt::process(state, &operation).map_err(IslandAuthzError::from_crdt)?;
        self.state = Some(state);
        self.observe_operation(operation_id)?;
        Ok(IslandAuthChange {
            operation_id,
            actor,
        })
    }

    fn observe_operation(
        &mut self,
        operation_id: IslandOperationId,
    ) -> Result<(), IslandAuthzError> {
        let Some(state) = self.state.as_mut() else {
            return Err(IslandAuthzError::StateUnavailable);
        };
        state.orderer_y.observe_operation(operation_id)?;
        state
            .orderer_y
            .set_heads(state.inner.heads().into_iter().collect());
        Ok(())
    }

    fn access(&self, member: IslandMemberId) -> Option<Access<IslandMemberCondition>> {
        let state = self.state.as_ref()?;
        state
            .members(self.group_id.auth_id())
            .into_iter()
            .find_map(|(member_id, access)| (member_id == member.auth_id()).then_some(access))
    }

    #[cfg(test)]
    fn current_heads(&self) -> BTreeSet<IslandOperationId> {
        self.state
            .as_ref()
            .map(|state| state.inner.heads().into_iter().collect())
            .unwrap_or_default()
    }

    #[cfg(test)]
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
        if current_binding.key() != signed.signer_key {
            return Err(IslandAuthzError::StaleSignerKey(signed.signer));
        }
        let expected_signature = sign_membership_operation(
            &signed.operation,
            signed.signer,
            signed.signer_key,
            signed.introduced_binding.as_ref(),
        );
        if expected_signature != signed.signature {
            return Err(IslandAuthzError::InvalidSignature(signed.operation_id()));
        }
        let payload = signed.operation.payload();
        if payload.group_id() != self.group_id.auth_id() {
            return Err(IslandAuthzError::WrongGroup {
                expected: self.group_id,
                actual: IslandGroupId(payload.group_id()),
            });
        }
        validate_supported_action_shape(&payload.action)?;
        match &payload.action {
            GroupAction::Add { member, .. }
            | GroupAction::Promote { member, .. }
            | GroupAction::Demote { member, .. } => {
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

#[cfg(test)]
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

#[cfg(test)]
fn reject_nested_group_member(member: GroupMember<AuthId>) -> Result<(), IslandAuthzError> {
    match member {
        GroupMember::Individual(_) => Ok(()),
        GroupMember::Group(id) => Err(IslandAuthzError::NestedGroupsUnsupported(IslandMemberId(
            id,
        ))),
    }
}

#[cfg(test)]
fn sign_membership_operation(
    operation: &IslandAuthOperation,
    signer: IslandMemberId,
    signer_key: IslandMemberKey,
    introduced_binding: Option<&IslandMemberKeyBinding>,
) -> IslandMembershipSignature {
    let mut hasher = blake3::Hasher::new();
    hash_bytes(&mut hasher, b"ployz:p2panda-authz-membership-signature-v1");
    hash_auth_id(&mut hasher, signer.auth_id());
    hash_bytes(&mut hasher, &signer_key.as_bytes());
    hash_operation_id(&mut hasher, operation.id());
    hash_auth_id(&mut hasher, operation.author());
    hash_u64(&mut hasher, operation.dependencies().len() as u64);
    for dependency in operation.dependencies() {
        hash_operation_id(&mut hasher, dependency);
    }
    hash_payload(&mut hasher, &operation.payload());
    match introduced_binding {
        Some(binding) => {
            hasher.update(&[1]);
            hash_str(&mut hasher, binding.island().as_str());
            hash_str(&mut hasher, binding.principal().as_str());
            hash_u64(&mut hasher, binding.epoch().get());
            hash_bytes(&mut hasher, &binding.key().as_bytes());
            hash_auth_id(&mut hasher, binding.member_id().auth_id());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    IslandMembershipSignature(*hasher.finalize().as_bytes())
}

#[cfg(test)]
fn hash_payload(
    hasher: &mut blake3::Hasher,
    payload: &GroupControlMessage<AuthId, IslandMemberCondition>,
) {
    hash_auth_id(hasher, payload.group_id());
    match &payload.action {
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

#[cfg(test)]
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

#[cfg(test)]
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
    #[error("member {member} state was not found in group {group}")]
    MemberNotFound {
        member: IslandMemberId,
        group: IslandGroupId,
    },
    #[error("p2panda-auth rejected group mutation: {reason}")]
    GroupRejected { reason: String },
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
    #[error("member {0} signed with a stale or substituted key")]
    StaleSignerKey(IslandMemberId),
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
}

impl From<AuthGroupsError> for IslandAuthzError {
    fn from(value: AuthGroupsError) -> Self {
        match value {
            GroupsError::Group(error) => Self::from_crdt(error),
            GroupsError::EmptyGroup => Self::GroupRejected {
                reason: "group must be created with at least one initial member".to_string(),
            },
            GroupsError::GroupMember(member, group) => Self::AlreadyMember {
                member: IslandMemberId(member),
                group: IslandGroupId(group),
            },
            GroupsError::NotGroupMember(member, group) => Self::NotMember {
                member: IslandMemberId(member),
                group: IslandGroupId(group),
            },
            GroupsError::InsufficientAccess(actor, access, group) => Self::InsufficientAccess {
                actor: IslandMemberId(actor),
                access: IslandAccess::from(&access),
                group: IslandGroupId(group),
            },
            GroupsError::SameAccessLevel(member, access, group) => Self::SameAccess {
                member: IslandMemberId(member),
                access: IslandAccess::from(&access),
                group: IslandGroupId(group),
            },
            GroupsError::MemberNotFound(member, group) => Self::MemberNotFound {
                member: IslandMemberId(member),
                group: IslandGroupId(group),
            },
        }
    }
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

    fn island() -> IslandId {
        IslandId::new("default")
    }

    fn member(seed: &str, island: &IslandId, epoch: u64) -> IslandMemberKeyBinding {
        let epoch = NonZeroU64::new(epoch).expect("test epochs are non-zero");
        IslandMemberKeyBinding::new(
            island.clone(),
            PrincipalId::new(seed),
            IslandMemberEpoch::new(epoch),
            IslandMemberKey::from_seed(format!("{seed}:{epoch}")),
        )
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
            payload: GroupControlMessage {
                group_id: group.auth_id(),
                action,
            },
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
        IslandSignedOperation::sign(operation, signer.member_id(), signer.key(), Some(member))
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
        let root = member("root", &island, 1);
        let mut authz =
            IslandAuthz::create(island.clone(), root.clone()).expect("root group should create");
        let writer = member("writer", &island, 1);
        let writer_id = writer.member_id();
        let signed = signed_add_operation(&authz, &root, 100, writer, IslandMemberRole::Writer);
        authz
            .apply_signed(signed)
            .expect("signed add should be accepted");
        assert!(authz.can_write_member(writer_id));
    }

    #[test]
    fn signed_membership_operation_rejects_substituted_signer_key() {
        let island = island();
        let root = member("root", &island, 1);
        let mut authz =
            IslandAuthz::create(island.clone(), root.clone()).expect("root group should create");
        let writer = member("writer", &island, 1);
        let mut signed = signed_add_operation(&authz, &root, 100, writer, IslandMemberRole::Writer);
        signed.signer_key = IslandMemberKey::from_seed("substituted-root-key");
        signed.signature = sign_membership_operation(
            &signed.operation,
            signed.signer,
            signed.signer_key,
            signed.introduced_binding.as_ref(),
        );
        assert!(matches!(
            authz.apply_signed(signed),
            Err(IslandAuthzError::StaleSignerKey(_))
        ));
    }

    #[test]
    fn signed_membership_operation_rejects_tampered_signature() {
        let island = island();
        let root = member("root", &island, 1);
        let mut authz =
            IslandAuthz::create(island.clone(), root.clone()).expect("root group should create");
        let writer = member("writer", &island, 1);
        let mut signed = signed_add_operation(&authz, &root, 100, writer, IslandMemberRole::Writer);
        signed.signature = IslandMembershipSignature([7; 32]);
        assert!(matches!(
            authz.apply_signed(signed),
            Err(IslandAuthzError::InvalidSignature(_))
        ));
    }

    #[test]
    fn signed_membership_operation_rejects_wrong_group() {
        let island = island();
        let root = member("root", &island, 1);
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
            IslandSignedOperation::sign(operation, root.member_id(), root.key(), Some(writer));
        assert!(matches!(
            authz.apply_signed(signed),
            Err(IslandAuthzError::WrongGroup { .. })
        ));
    }

    #[test]
    fn signed_membership_operation_rejects_nested_group_member() {
        let island = island();
        let root = member("root", &island, 1);
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
            IslandSignedOperation::sign(operation, root.member_id(), root.key(), Some(nested));
        assert!(matches!(
            authz.apply_signed(signed),
            Err(IslandAuthzError::NestedGroupsUnsupported(_))
        ));
    }

    #[test]
    fn signed_membership_operation_requires_added_member_binding() {
        let island = island();
        let root = member("root", &island, 1);
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
        let signed = IslandSignedOperation::sign(operation, root.member_id(), root.key(), None);
        assert!(matches!(
            authz.apply_signed(signed),
            Err(IslandAuthzError::MissingIntroducedBinding(_))
        ));
    }

    #[test]
    fn signed_membership_operation_rejects_remove_with_introduced_binding() {
        let island = island();
        let root = member("root", &island, 1);
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
            IslandSignedOperation::sign(operation, root.member_id(), root.key(), Some(writer));
        assert!(matches!(
            authz.apply_signed(signed),
            Err(IslandAuthzError::UnexpectedIntroducedBinding(_))
        ));
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
