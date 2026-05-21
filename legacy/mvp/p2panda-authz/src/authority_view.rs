use std::collections::BTreeMap;

use mvp_bus::{IslandId, PrincipalId};
use p2panda_auth::{Access, AccessLevel};

use crate::{
    IslandAuthz, IslandMemberAuthorKey, IslandMemberCondition, IslandMemberEpoch, IslandMemberId,
    IslandMemberKeyBinding,
};

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
    pub(crate) fn from_authz(authz: &IslandAuthz) -> Self {
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
