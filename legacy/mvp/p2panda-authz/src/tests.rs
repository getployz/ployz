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

fn rewrap_membership_operation(
    mut operation: IslandMembershipOperation,
    signer_private_key: &SigningKey,
) -> IslandMembershipOperation {
    operation.header.verifying_key = signer_private_key.verifying_key();
    operation.header.sign(signer_private_key);
    operation.hash = operation.header.hash();
    operation
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
async fn sqlite_membership_store_reopens_root_and_writer() {
    let island = island();
    let (root, root_key) = member_with_private_key("root", &island, 1);
    let root_authority = IslandRootAuthority::new(root.clone());
    let tempdir = tempfile::tempdir().expect("tempdir");
    let path = tempdir.path().join("membership.sqlite");
    let store = IslandAuthzStore::open(&path, island.clone(), root_authority.clone())
        .await
        .expect("open membership store");
    let mut authz = store
        .create_root(root.clone(), &root_key)
        .await
        .expect("root create should persist");

    let writer = member("writer", &island, 1);
    let writer_id = writer.member_id();
    store
        .add_writer(&mut authz, &root, &root_key, writer)
        .await
        .expect("writer add should persist");

    let reopened = IslandAuthzStore::open(&path, island, root_authority)
        .await
        .expect("reopen membership store")
        .replay()
        .await
        .expect("stored operations should replay");
    assert!(reopened.can_write_member(root.member_id()));
    assert!(reopened.can_write_member(writer_id));
}

#[tokio::test]
async fn sqlite_membership_store_rejects_second_root() {
    let island = island();
    let (root, root_key) = member_with_private_key("root", &island, 1);
    let root_authority = IslandRootAuthority::new(root.clone());
    let tempdir = tempfile::tempdir().expect("tempdir");
    let path = tempdir.path().join("membership.sqlite");
    let store = IslandAuthzStore::open(&path, island.clone(), root_authority)
        .await
        .expect("open membership store");
    store
        .create_root(root.clone(), &root_key)
        .await
        .expect("root create should persist");
    let error = match store.create_root(root, &root_key).await {
        Ok(_) => panic!("second root create should fail"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        IslandAuthzError::RootAlreadyPinned { island: failed_island }
            if failed_island == island
    ));
}

#[tokio::test]
async fn sqlite_membership_store_imports_duplicate_operation_idempotently() {
    let island = island();
    let (root, root_key) = member_with_private_key("root", &island, 1);
    let root_authority = IslandRootAuthority::new(root.clone());
    let writer = member("writer", &island, 1);
    let writer_id = writer.member_id();
    let tempdir = tempfile::tempdir().expect("tempdir");

    let source = IslandAuthzStore::open(
        tempdir.path().join("source.sqlite"),
        island.clone(),
        root_authority.clone(),
    )
    .await
    .expect("open source membership store");
    let mut source_authz = source
        .create_root(root.clone(), &root_key)
        .await
        .expect("source root create should persist");
    source
        .add_writer(&mut source_authz, &root, &root_key, writer)
        .await
        .expect("source writer add should persist");
    let operations = source
        .export_operations()
        .await
        .expect("source operations should export");
    assert_eq!(operations.len(), 2);

    let target = IslandAuthzStore::open(
        tempdir.path().join("target.sqlite"),
        island.clone(),
        root_authority,
    )
    .await
    .expect("open target membership store");
    target
        .import_operation(operations[0].clone())
        .await
        .expect("root import should persist");
    let first_import = target
        .import_operation(operations[1].clone())
        .await
        .expect("writer import should persist");
    let duplicate_import = target
        .import_operation(operations[1].clone())
        .await
        .expect("duplicate writer import should be idempotent");

    assert_eq!(first_import, duplicate_import);
    assert!(
        target
            .replay()
            .await
            .expect("target membership should replay")
            .can_write_member(writer_id)
    );
}

#[tokio::test]
async fn sqlite_membership_store_rejects_rewrapped_membership_envelope() {
    let island = island();
    let (root, root_key) = member_with_private_key("root", &island, 1);
    let root_authority = IslandRootAuthority::new(root.clone());
    let writer = member("writer", &island, 1);
    let tempdir = tempfile::tempdir().expect("tempdir");

    let source = IslandAuthzStore::open(
        tempdir.path().join("source.sqlite"),
        island.clone(),
        root_authority.clone(),
    )
    .await
    .expect("open source membership store");
    let mut source_authz = source
        .create_root(root.clone(), &root_key)
        .await
        .expect("source root create should persist");
    source
        .add_writer(&mut source_authz, &root, &root_key, writer)
        .await
        .expect("source writer add should persist");
    let operations = source
        .export_operations()
        .await
        .expect("source operations should export");

    let target = IslandAuthzStore::open(
        tempdir.path().join("target.sqlite"),
        island.clone(),
        root_authority,
    )
    .await
    .expect("open target membership store");
    target
        .import_operation(operations[0].clone())
        .await
        .expect("root import should persist");

    let attacker_key = member_private_key("attacker-envelope-key", 1);
    let rewrapped = rewrap_membership_operation(operations[1].clone(), &attacker_key);
    let error = match target.import_operation(rewrapped).await {
        Ok(_) => panic!("rewrapped membership envelope should fail"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        IslandAuthzError::PandaEnvelopeAuthorMismatch { signer }
            if signer == root.member_id()
    ));
    target
        .import_operation(operations[1].clone())
        .await
        .expect("canonical operation should still import after rejected envelope");
}

#[tokio::test]
async fn sqlite_membership_store_rejects_missing_dependency_with_structured_error() {
    let island = island();
    let (root, root_key) = member_with_private_key("root", &island, 1);
    let root_authority = IslandRootAuthority::new(root.clone());
    let writer = member("writer", &island, 1);
    let replica = member("replica", &island, 1);
    let tempdir = tempfile::tempdir().expect("tempdir");

    let source = IslandAuthzStore::open(
        tempdir.path().join("source.sqlite"),
        island.clone(),
        root_authority.clone(),
    )
    .await
    .expect("open source membership store");
    let mut source_authz = source
        .create_root(root.clone(), &root_key)
        .await
        .expect("source root create should persist");
    source
        .add_writer(&mut source_authz, &root, &root_key, writer)
        .await
        .expect("source writer add should persist");
    source
        .add_replica_importer(
            &mut source_authz,
            &root,
            &root_key,
            replica,
            ReplicaImportAccess::Read,
        )
        .await
        .expect("source replica add should persist");
    let operations = source
        .export_operations()
        .await
        .expect("source operations should export");
    let missing_dependency = signed_from_membership_operation(&island, operations[1].clone())
        .expect("writer operation should decode")
        .1
        .operation_id();

    let target =
        IslandAuthzStore::open(tempdir.path().join("target.sqlite"), island, root_authority)
            .await
            .expect("open target membership store");
    target
        .import_operation(operations[0].clone())
        .await
        .expect("root import should persist");
    let error = match target.import_operation(operations[2].clone()).await {
        Ok(_) => panic!("membership op with missing dependency should fail"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        IslandAuthzError::DependencyMissing { dependency, .. }
            if dependency == missing_dependency
    ));
}

#[tokio::test]
async fn sqlite_membership_store_rejects_shadow_root_import() {
    let island = island();
    let (root, root_key) = member_with_private_key("root", &island, 1);
    let root_authority = IslandRootAuthority::new(root.clone());
    let (shadow_root, shadow_root_key) = member_with_private_key("shadow-root", &island, 1);
    let tempdir = tempfile::tempdir().expect("tempdir");

    let target = IslandAuthzStore::open(
        tempdir.path().join("target.sqlite"),
        island.clone(),
        root_authority,
    )
    .await
    .expect("open target membership store");
    target
        .create_root(root.clone(), &root_key)
        .await
        .expect("target root create should persist");

    let shadow = IslandAuthzStore::open(
        tempdir.path().join("shadow.sqlite"),
        island.clone(),
        IslandRootAuthority::new(shadow_root.clone()),
    )
    .await
    .expect("open shadow membership store");
    shadow
        .create_root(shadow_root, &shadow_root_key)
        .await
        .expect("shadow root create should persist");
    let shadow_root_operation = shadow
        .export_operations()
        .await
        .expect("shadow root should export")
        .remove(0);

    let error = match target.import_operation(shadow_root_operation).await {
        Ok(_) => panic!("shadow root import should fail"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        IslandAuthzError::RootAlreadyPinned { island: failed_island }
            if failed_island == island
    ));
    assert!(
        target
            .replay()
            .await
            .expect("target membership should remain valid")
            .can_write_member(root.member_id())
    );
}

#[tokio::test]
async fn sqlite_membership_store_rejects_wrong_root_on_open() {
    let island = island();
    let (root, root_key) = member_with_private_key("root", &island, 1);
    let root_authority = IslandRootAuthority::new(root.clone());
    let tempdir = tempfile::tempdir().expect("tempdir");
    let path = tempdir.path().join("membership.sqlite");
    let store = IslandAuthzStore::open(&path, island.clone(), root_authority)
        .await
        .expect("open membership store");
    store
        .create_root(root, &root_key)
        .await
        .expect("root create should persist");

    let wrong_root = member("wrong-root", &island, 1);
    let error = IslandAuthzStore::open(&path, island, IslandRootAuthority::new(wrong_root))
        .await
        .expect_err("wrong root authority should fail on open");

    assert!(matches!(error, IslandAuthzError::UnanchoredRootCreate(_)));
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
    let signed = IslandSignedOperation::sign(operation, root.member_id(), &root_key, Some(writer));
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
    let signed = IslandSignedOperation::sign(operation, root.member_id(), &root_key, Some(nested));
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
    let signed = IslandSignedOperation::sign(operation, root.member_id(), &root_key, Some(writer));
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
        Err(IslandAuthzError::DuplicateMembershipOperation(_))
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
