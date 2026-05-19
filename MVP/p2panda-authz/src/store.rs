use std::path::Path;

use mvp_bus::IslandId;
#[cfg(test)]
use p2panda_auth::group::GroupAction;
use p2panda_auth::traits::Operation;
use p2panda_core::{Hash, Header, Operation as PandaOperation, SigningKey, VerifyingKey};
use p2panda_store::logs::LogStore;
use p2panda_store::topics::TopicStore;
use p2panda_store::{SqliteStore, SqliteStoreBuilder};
use p2panda_stream::ingest::ingest_operation;

use crate::{
    IslandAuthChange, IslandAuthoritySnapshot, IslandAuthz, IslandAuthzError, IslandMemberId,
    IslandMemberKeyBinding, IslandMemberRole, IslandMembershipExtensions, IslandMembershipLog,
    IslandMembershipOperation, IslandRootAuthority, IslandSignedOperation, ReplicaImportAccess,
    build_membership_panda_operation, prepare_sqlite_parent, replay_signed_membership_operations,
    signed_from_membership_operation, sqlite_url, store_error,
    validate_membership_mutation_for_island,
};

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
    pub(crate) fn from_store(
        island: IslandId,
        operations: Vec<(Header<IslandMembershipExtensions>, IslandSignedOperation)>,
    ) -> Self {
        Self { island, operations }
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn store(&self) -> Vec<(Header<IslandMembershipExtensions>, IslandSignedOperation)> {
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
        validate_membership_mutation_for_island(&self.island, authz, manager, manager_private_key)?;
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
        validate_membership_mutation_for_island(&self.island, authz, manager, manager_private_key)?;
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
        validate_membership_mutation_for_island(&self.island, authz, manager, manager_private_key)?;
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

    pub async fn replay(
        &self,
        root_authority: &IslandRootAuthority,
    ) -> Result<IslandAuthz, IslandAuthzError> {
        replay_signed_membership_operations(&self.island, root_authority, self.signed_operations())
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
        let latest = self
            .operations
            .iter()
            .rev()
            .find(|(header, _)| header.verifying_key == public_key);
        let operation = build_membership_panda_operation(
            &self.island,
            signed,
            signer_private_key,
            latest.map(|(header, _)| header),
        )?;
        self.operations.push((operation.header, signed.clone()));
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct IslandAuthzStore {
    island: IslandId,
    root_authority: IslandRootAuthority,
    store: SqliteStore,
}

impl IslandAuthzStore {
    pub async fn open(
        path: impl AsRef<Path>,
        island: IslandId,
        root_authority: IslandRootAuthority,
    ) -> Result<Self, IslandAuthzError> {
        if root_authority.binding().island() != &island {
            return Err(IslandAuthzError::WrongIsland {
                expected: island,
                actual: root_authority.binding().island().clone(),
            });
        }
        prepare_sqlite_parent(path.as_ref())?;
        let url = sqlite_url(path.as_ref());
        let store = SqliteStoreBuilder::new()
            .database_url(&url)
            .build()
            .await
            .map_err(store_error)?;
        let authz_store = Self {
            island,
            root_authority,
            store,
        };
        let signed_operations = authz_store.signed_operations().await?;
        if !signed_operations.is_empty() {
            replay_signed_membership_operations(
                &authz_store.island,
                &authz_store.root_authority,
                signed_operations,
            )?;
        }
        Ok(authz_store)
    }

    pub async fn create_root(
        &self,
        root: IslandMemberKeyBinding,
        root_key: &SigningKey,
    ) -> Result<IslandAuthz, IslandAuthzError> {
        if self.has_membership_log_association().await? {
            return Err(IslandAuthzError::RootAlreadyPinned {
                island: self.island.clone(),
            });
        }
        if root.member_id() != self.root_authority.binding().member_id() {
            return Err(IslandAuthzError::RootAuthorityMismatch(root.member_id()));
        }
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
        &self,
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
        &self,
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
        &self,
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

    pub async fn demote_to_replica_importer(
        &self,
        authz: &mut IslandAuthz,
        manager: &IslandMemberKeyBinding,
        manager_private_key: &SigningKey,
        member: IslandMemberId,
        access: ReplicaImportAccess,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        validate_membership_mutation_for_island(&self.island, authz, manager, manager_private_key)?;
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

    pub async fn remove_member(
        &self,
        authz: &mut IslandAuthz,
        manager: &IslandMemberKeyBinding,
        manager_private_key: &SigningKey,
        member: IslandMemberId,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        validate_membership_mutation_for_island(&self.island, authz, manager, manager_private_key)?;
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

    pub async fn replay(&self) -> Result<IslandAuthz, IslandAuthzError> {
        replay_signed_membership_operations(
            &self.island,
            &self.root_authority,
            self.signed_operations().await?,
        )
    }

    pub async fn authority_snapshot(&self) -> Result<IslandAuthoritySnapshot, IslandAuthzError> {
        Ok(self.replay().await?.authority_snapshot())
    }

    #[cfg(test)]
    pub(crate) async fn export_operations(
        &self,
    ) -> Result<Vec<IslandMembershipOperation>, IslandAuthzError> {
        self.panda_operations().await
    }

    #[cfg(test)]
    pub(crate) async fn import_operation(
        &self,
        operation: IslandMembershipOperation,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        let candidate = signed_from_membership_operation(&self.island, operation.clone())?;
        let existing = self.signed_operations().await?;
        if let Some((_, existing_signed)) = existing
            .iter()
            .find(|(_, signed)| signed.operation_id() == candidate.1.operation_id())
        {
            if existing_signed == &candidate.1 {
                replay_signed_membership_operations(
                    &self.island,
                    &self.root_authority,
                    existing.clone(),
                )?
                .validate_panda_envelope_author(&candidate.0, &candidate.1)?;
                return Ok(IslandAuthChange {
                    operation_id: existing_signed.operation_id(),
                    actor: existing_signed.signer(),
                });
            }
            return Err(IslandAuthzError::DuplicateMembershipOperation(
                candidate.1.operation_id(),
            ));
        }
        if !existing.is_empty()
            && matches!(candidate.1.operation.action, GroupAction::Create { .. })
        {
            return Err(IslandAuthzError::RootAlreadyPinned {
                island: self.island.clone(),
            });
        }

        let mut candidate_replay = existing;
        candidate_replay.push(candidate.clone());
        replay_signed_membership_operations(&self.island, &self.root_authority, candidate_replay)?;
        self.insert_operation(&operation).await?;
        Ok(IslandAuthChange {
            operation_id: candidate.1.operation_id(),
            actor: candidate.1.signer(),
        })
    }

    async fn add_member(
        &self,
        authz: &mut IslandAuthz,
        manager: &IslandMemberKeyBinding,
        manager_private_key: &SigningKey,
        member: IslandMemberKeyBinding,
        role: IslandMemberRole,
    ) -> Result<IslandAuthChange, IslandAuthzError> {
        validate_membership_mutation_for_island(&self.island, authz, manager, manager_private_key)?;
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

    async fn insert_signed(
        &self,
        signed: &IslandSignedOperation,
        signer_private_key: &SigningKey,
    ) -> Result<(), IslandAuthzError> {
        let public_key = signer_private_key.verifying_key();
        let log_id = IslandMembershipLog::from_island(&self.island);
        let latest = self
            .store
            .get_latest_entry(&public_key, &log_id)
            .await
            .map_err(store_error)?;
        let operation = build_membership_panda_operation(
            &self.island,
            signed,
            signer_private_key,
            latest.as_ref().map(|operation| &operation.header),
        )?;
        self.insert_operation(&operation).await?;
        Ok(())
    }

    async fn insert_operation(
        &self,
        operation: &IslandMembershipOperation,
    ) -> Result<bool, IslandAuthzError> {
        let log_id = IslandMembershipLog::from_island(&self.island);
        ingest_operation(&self.store, operation, &log_id, &log_id, false)
            .await
            .map_err(|error| IslandAuthzError::InvalidPandaOperation {
                message: error.to_string(),
            })
    }

    async fn panda_operations(&self) -> Result<Vec<IslandMembershipOperation>, IslandAuthzError> {
        let log_id = IslandMembershipLog::from_island(&self.island);
        let associations = <SqliteStore as TopicStore<
            IslandMembershipLog,
            VerifyingKey,
            IslandMembershipLog,
        >>::resolve(&self.store, &log_id)
        .await
        .map_err(store_error)?;
        let mut operations = Vec::new();
        for (author, logs) in associations {
            for log in logs {
                let Some(entries) =
                    <SqliteStore as LogStore<
                        PandaOperation<IslandMembershipExtensions>,
                        VerifyingKey,
                        IslandMembershipLog,
                        u64,
                        Hash,
                    >>::get_log_entries(&self.store, &author, &log, None, None)
                    .await
                    .map_err(store_error)?
                else {
                    continue;
                };
                operations.extend(entries.into_iter().map(|(operation, _)| operation));
            }
        }
        operations.sort_by_key(|operation| (operation.header.seq_num, operation.hash));
        Ok(operations)
    }

    async fn signed_operations(
        &self,
    ) -> Result<Vec<(Header<IslandMembershipExtensions>, IslandSignedOperation)>, IslandAuthzError>
    {
        self.panda_operations()
            .await?
            .into_iter()
            .map(|operation| signed_from_membership_operation(&self.island, operation))
            .collect()
    }

    async fn has_membership_log_association(&self) -> Result<bool, IslandAuthzError> {
        let log_id = IslandMembershipLog::from_island(&self.island);
        let associations = <SqliteStore as TopicStore<
            IslandMembershipLog,
            VerifyingKey,
            IslandMembershipLog,
        >>::resolve(&self.store, &log_id)
        .await
        .map_err(store_error)?;
        Ok(!associations.is_empty())
    }
}
