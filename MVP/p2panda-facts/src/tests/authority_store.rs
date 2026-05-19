use std::num::NonZeroU64;
use std::sync::Arc;

use mvp_bus::{BusAuthority, Grant, harness::InMemoryBus};
use mvp_p2panda_authz::{
    IslandAuthz, IslandAuthzMemoryLog, IslandAuthzStore, IslandMemberAuthorKey, IslandMemberEpoch,
    IslandMemberKeyBinding, IslandRootAuthority, ReplicaImportAccess,
};
use tempfile::tempdir;

use super::*;

fn key(value: &str) -> FactKey {
    FactKey::parse(value).expect("fact key parses")
}

fn pattern(value: &str) -> FactKeyPattern {
    FactKeyPattern::parse(value).expect("fact pattern parses")
}

fn principal(value: &str) -> PrincipalId {
    PrincipalId::new(value)
}

fn island(value: &str) -> IslandId {
    IslandId::new(value)
}

fn grant_prod(authority: &BusAuthority, principal: &str, grant: Grant) -> BusSession {
    authority.grant_in(island("prod"), PrincipalId::new(principal), grant)
}

fn store_with_authority() -> (PandaFactStore, BusAuthority) {
    let (bus, authority) = InMemoryBus::new_with_authority();
    (PandaFactStore::new(Arc::new(bus)), authority)
}

#[test]
fn author_private_key_hex_round_trips_for_process_roles() {
    let seed = "12".repeat(32);
    let author = PandaFactAuthor::from_private_key_hex(principal("writer"), &seed)
        .expect("private key parses");
    let same_author =
        PandaFactAuthor::from_private_key_hex(principal("writer"), &author.private_key_hex())
            .expect("private key round trips");

    assert_eq!(author.private_key_hex(), seed);
    assert_eq!(same_author.author_key(), author.author_key());
    assert!(matches!(
        PandaFactAuthor::from_private_key_hex(principal("writer"), "123"),
        Err(PandaFactError::InvalidAuthorPrivateKey { .. })
    ));
    assert!(matches!(
        PandaFactAuthor::from_private_key_hex(principal("writer"), &"GG".repeat(32)),
        Err(PandaFactError::InvalidAuthorPrivateKey { .. })
    ));
}

fn store_from_bus(bus: InMemoryBus) -> PandaFactStore {
    PandaFactStore::new(Arc::new(bus))
}

fn authz_binding(
    island: &IslandId,
    author: &PandaFactAuthor,
    epoch: u64,
) -> IslandMemberKeyBinding {
    IslandMemberKeyBinding::new(
        island.clone(),
        author.principal().clone(),
        IslandMemberEpoch::new(NonZeroU64::new(epoch).expect("test epoch is non-zero")),
        author.author_key().into(),
    )
}

struct AuthorityFixture {
    log: IslandAuthzMemoryLog,
    authz: IslandAuthz,
    root: IslandMemberKeyBinding,
    root_private_key: SigningKey,
}

impl AuthorityFixture {
    fn snapshot(&self) -> IslandAuthoritySnapshot {
        self.authz.authority_snapshot()
    }
}

async fn authority_fixture_for_writer_and_replica(
    island: &IslandId,
    writer: &PandaFactAuthor,
    replica: &PandaFactAuthor,
) -> AuthorityFixture {
    let root_private_key = SigningKey::from_bytes(&[9; 32]);
    let root = IslandMemberKeyBinding::new(
        island.clone(),
        PrincipalId::new("root"),
        IslandMemberEpoch::new(NonZeroU64::new(1).expect("test epoch is non-zero")),
        IslandMemberAuthorKey::from_public_key(root_private_key.verifying_key()),
    );
    let mut log = IslandAuthzMemoryLog::new(island.clone());
    let mut authz = log
        .create_root(root.clone(), &root_private_key)
        .await
        .expect("root membership operation persists");
    log.add_writer(
        &mut authz,
        &root,
        &root_private_key,
        authz_binding(island, writer, 1),
    )
    .await
    .expect("writer membership operation persists");
    log.add_replica_importer(
        &mut authz,
        &root,
        &root_private_key,
        authz_binding(island, replica, 1),
        ReplicaImportAccess::Read,
    )
    .await
    .expect("replica importer membership operation persists");
    AuthorityFixture {
        log,
        authz,
        root,
        root_private_key,
    }
}

async fn authority_for_writer_and_replica(
    island: &IslandId,
    writer: &PandaFactAuthor,
    replica: &PandaFactAuthor,
) -> IslandAuthoritySnapshot {
    authority_fixture_for_writer_and_replica(island, writer, replica)
        .await
        .snapshot()
}

async fn durable_authority_store_for_writer_and_replica(
    root: &Path,
    island: &IslandId,
    writer: &PandaFactAuthor,
    replica: &PandaFactAuthor,
) -> IslandAuthzStore {
    let root_private_key = SigningKey::from_bytes(&[9; 32]);
    let root_binding = IslandMemberKeyBinding::new(
        island.clone(),
        PrincipalId::new("root"),
        IslandMemberEpoch::new(NonZeroU64::new(1).expect("test epoch is non-zero")),
        IslandMemberAuthorKey::from_public_key(root_private_key.verifying_key()),
    );
    let store = IslandAuthzStore::open(
        root.join("membership.sqlite"),
        island.clone(),
        IslandRootAuthority::new(root_binding.clone()),
    )
    .await
    .expect("durable authority store opens");
    let mut authz = store
        .create_root(root_binding.clone(), &root_private_key)
        .await
        .expect("root membership persists");
    store
        .add_writer(
            &mut authz,
            &root_binding,
            &root_private_key,
            authz_binding(island, writer, 1),
        )
        .await
        .expect("writer membership persists");
    store
        .add_replica_importer(
            &mut authz,
            &root_binding,
            &root_private_key,
            authz_binding(island, replica, 1),
            ReplicaImportAccess::Read,
        )
        .await
        .expect("replica membership persists");
    store
}

#[tokio::test]
async fn sqlite_open_config_installs_durable_membership_authority_source() {
    let tempdir = tempdir().expect("tempdir");
    let island = island("prod");
    let writer = PandaFactAuthor::from_private_key_bytes(principal("writer"), [1; 32]);
    let replica = PandaFactAuthor::from_private_key_bytes(principal("replica"), [2; 32]);
    let authority_store =
        durable_authority_store_for_writer_and_replica(tempdir.path(), &island, &writer, &replica)
            .await;
    let authority_source = PandaFactAuthoritySource::from_membership_store(&authority_store)
        .await
        .expect("authority source rebuilds from membership store");
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer_session = authority.grant_in(
        island.clone(),
        PrincipalId::new("writer"),
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    let store = PandaFactStore::open_sqlite(
        Arc::new(bus),
        PandaSqliteOpenConfig::new(tempdir.path().join("facts.sqlite"), vec![island.clone()])
            .with_authority_source(authority_source),
    )
    .await
    .expect("fact store opens with durable authority source");

    assert!(
        store.can_write_fact(&writer_session, &key("/facts/routes/commit-1")),
        "durable membership-derived authority and Ployz fact grant should both pass"
    );
}

#[tokio::test]
async fn authority_snapshot_authorizes_local_write_without_manual_trust() {
    let (mut store, authority) = store_with_authority();
    let writer_session = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let writer = PandaFactAuthor::from_private_key_bytes(principal("writer"), [1; 32]);
    let replica = PandaFactAuthor::from_private_key_bytes(principal("replica"), [2; 32]);
    let snapshot =
        authority_for_writer_and_replica(writer_session.island(), &writer, &replica).await;
    store.install_authority_snapshot(snapshot);

    let outcome = store
        .write_fact_payload(
            &writer_session,
            &writer,
            key("/facts/authz/writer"),
            FactPayload::from_static(b"authorized"),
        )
        .await
        .expect("authz writer should write without manual trust");

    assert!(matches!(outcome, PandaFactWriteOutcome::Inserted(_)));
}

#[tokio::test]
async fn authority_snapshot_authorizes_replica_import_without_manual_trust() {
    let (mut source, source_authority) = store_with_authority();
    let writer_session = grant_prod(
        &source_authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let writer = PandaFactAuthor::from_private_key_bytes(principal("writer"), [1; 32]);
    let replica = PandaFactAuthor::from_private_key_bytes(principal("replica"), [2; 32]);
    let snapshot =
        authority_for_writer_and_replica(writer_session.island(), &writer, &replica).await;
    source.install_authority_snapshot(snapshot.clone());
    source
        .write_fact_payload(
            &writer_session,
            &writer,
            key("/facts/authz/import"),
            FactPayload::from_static(b"authorized"),
        )
        .await
        .expect("authz writer should write");
    let operation = source
        .export_operations()
        .next()
        .cloned()
        .expect("write exports one operation");

    let (mut target, target_authority) = store_with_authority();
    let _writer_grant = grant_prod(
        &target_authority,
        "writer",
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    let replica_session = grant_prod(&target_authority, "replica", Grant::empty());
    target.install_authority_snapshot(snapshot);

    let outcome = target
        .import_replica_operation(&replica_session, &operation)
        .await
        .expect("authz replica importer should import without manual trust");

    assert!(matches!(outcome, PandaFactWriteOutcome::Inserted(_)));
}

#[tokio::test]
async fn authority_snapshot_rejects_read_only_replica_import() {
    let (mut source, source_authority) = store_with_authority();
    let writer_session = grant_prod(
        &source_authority,
        "writer",
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    let writer = PandaFactAuthor::from_private_key_bytes(principal("writer"), [1; 32]);
    let replica = PandaFactAuthor::from_private_key_bytes(principal("replica"), [2; 32]);
    let snapshot =
        authority_for_writer_and_replica(writer_session.island(), &writer, &replica).await;
    source.install_authority_snapshot(snapshot.clone());
    source
        .write_fact_payload(
            &writer_session,
            &writer,
            key("/facts/authz/reject-import"),
            FactPayload::from_static(b"authorized"),
        )
        .await
        .expect("authz writer should write");
    let operation = source
        .export_operations()
        .next()
        .cloned()
        .expect("write exports one operation");

    let (mut target, target_authority) = store_with_authority();
    let _writer_grant = grant_prod(
        &target_authority,
        "writer",
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    let readonly_session = grant_prod(&target_authority, "readonly", Grant::empty());
    target.install_authority_snapshot(snapshot);

    let error = target
        .import_replica_operation(&readonly_session, &operation)
        .await
        .expect_err("non-replica member cannot import");

    assert!(matches!(
        error,
        PandaFactError::UnauthorizedReplicaImport { .. }
    ));
}

#[tokio::test]
async fn authority_snapshot_rejects_removed_writer_imports_and_future_writes() {
    let (mut source, source_authority) = store_with_authority();
    let writer_session = grant_prod(
        &source_authority,
        "writer",
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    let writer = PandaFactAuthor::from_private_key_bytes(principal("writer"), [1; 32]);
    let replica = PandaFactAuthor::from_private_key_bytes(principal("replica"), [2; 32]);
    let mut fixture =
        authority_fixture_for_writer_and_replica(writer_session.island(), &writer, &replica).await;
    source.install_authority_snapshot(fixture.snapshot());
    source
        .write_fact_payload(
            &writer_session,
            &writer,
            key("/facts/authz/pre-remove"),
            FactPayload::from_static(b"before-remove"),
        )
        .await
        .expect("writer should write before removal");
    let operation = source
        .export_operations()
        .next()
        .cloned()
        .expect("write exports one operation");

    fixture
        .log
        .remove_member(
            &mut fixture.authz,
            &fixture.root,
            &fixture.root_private_key,
            authz_binding(writer_session.island(), &writer, 1).member_id(),
        )
        .await
        .expect("writer removal persists");
    let removed_snapshot = fixture.snapshot();

    let forged_after_remove = source
        .write_fact_payload(
            &writer_session,
            &writer,
            key("/facts/authz/stale-post-remove"),
            FactPayload::from_static(b"stale-after-remove"),
        )
        .await
        .expect("stale local authority can still sign a partitioned operation");
    assert!(matches!(
        forged_after_remove,
        PandaFactWriteOutcome::Inserted(_)
    ));
    let forged_operation = source
        .export_operations()
        .last()
        .cloned()
        .expect("forged write exports an operation");

    let (mut target, target_authority) = store_with_authority();
    let _writer_grant = grant_prod(
        &target_authority,
        "writer",
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    let replica_session = grant_prod(&target_authority, "replica", Grant::empty());
    target.install_authority_snapshot(removed_snapshot.clone());
    let import_error = target
        .import_replica_operation(&replica_session, &operation)
        .await
        .expect_err("removed writer imports are not accepted without fact-log frontier proof");
    assert!(matches!(
        import_error,
        PandaFactError::UntrustedAuthorKey { .. }
    ));
    let forged_error = target
        .import_replica_operation(&replica_session, &forged_operation)
        .await
        .expect_err("removed writer cannot forge a fresh operation with the old epoch");
    assert!(matches!(
        forged_error,
        PandaFactError::UntrustedAuthorKey { .. }
    ));

    source.install_authority_snapshot(removed_snapshot.clone());
    assert_eq!(
        source
            .list_candidates(
                writer_session.island(),
                &pattern("/facts/authz/pre-remove"),
                &writer_session,
            )
            .expect("local pre-remove fact remains queryable")
            .len(),
        1
    );
    let error = source
        .write_fact_payload(
            &writer_session,
            &writer,
            key("/facts/authz/post-remove"),
            FactPayload::from_static(b"after-remove"),
        )
        .await
        .expect_err("removed writer cannot write future facts");
    assert!(matches!(error, PandaFactError::UntrustedAuthorKey { .. }));

    let scope = PandaFactSyncScope::from_authority(&removed_snapshot);
    assert!(!scope.trusted_authors.contains_key(writer.principal()));
}

#[tokio::test]
async fn demoted_writer_becomes_replica_importer_without_write_authority() {
    let (mut source, source_authority) = store_with_authority();
    let writer_session = grant_prod(
        &source_authority,
        "writer",
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    let root_session = grant_prod(&source_authority, "root", Grant::allow_all());
    let writer = PandaFactAuthor::from_private_key_bytes(principal("writer"), [1; 32]);
    let root_author = PandaFactAuthor::from_private_key_bytes(principal("root"), [9; 32]);
    let replica = PandaFactAuthor::from_private_key_bytes(principal("replica"), [2; 32]);
    let mut fixture =
        authority_fixture_for_writer_and_replica(writer_session.island(), &writer, &replica).await;
    source.install_authority_snapshot(fixture.snapshot());
    source
        .write_fact_payload(
            &writer_session,
            &writer,
            key("/facts/authz/pre-demote"),
            FactPayload::from_static(b"before-demote"),
        )
        .await
        .expect("writer should write before demotion");
    let operation = source
        .export_operations()
        .next()
        .cloned()
        .expect("write exports one operation");

    fixture
        .log
        .demote_to_replica_importer(
            &mut fixture.authz,
            &fixture.root,
            &fixture.root_private_key,
            authz_binding(writer_session.island(), &writer, 1).member_id(),
            ReplicaImportAccess::Read,
        )
        .await
        .expect("writer demotion persists");
    let demoted_snapshot = fixture.snapshot();

    source.install_authority_snapshot(demoted_snapshot.clone());
    let write_error = source
        .write_fact_payload(
            &writer_session,
            &writer,
            key("/facts/authz/post-demote"),
            FactPayload::from_static(b"after-demote"),
        )
        .await
        .expect_err("demoted writer cannot write future facts");
    assert!(matches!(
        write_error,
        PandaFactError::UntrustedAuthorKey { .. }
    ));
    source
        .write_fact_payload(
            &root_session,
            &root_author,
            key("/facts/authz/root-after-demote"),
            FactPayload::from_static(b"root-after-demote"),
        )
        .await
        .expect("root remains an active writer");
    let root_operation = source
        .export_operations()
        .last()
        .cloned()
        .expect("root write exports one operation");

    let (mut target, target_authority) = store_with_authority();
    let _target_root_grant = grant_prod(
        &target_authority,
        "root",
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    let demoted_replica_session = grant_prod(
        &target_authority,
        "writer",
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    target.install_authority_snapshot(demoted_snapshot.clone());
    let old_writer_error = target
        .import_replica_operation(&demoted_replica_session, &operation)
        .await
        .expect_err("demoted writer's own old operation needs fact-log frontier proof");
    assert!(matches!(
        old_writer_error,
        PandaFactError::UntrustedAuthorKey { .. }
    ));
    let imported = target
        .import_replica_operation(&demoted_replica_session, &root_operation)
        .await
        .expect("demoted writer can import active writers as replica");
    assert!(matches!(imported, PandaFactWriteOutcome::Inserted(_)));

    let scope = PandaFactSyncScope::from_authority(&demoted_snapshot);
    assert!(!scope.trusted_authors.contains_key(writer.principal()));
}

fn sqlite_config(
    path: impl Into<PathBuf>,
    island: &IslandId,
    author: &PandaFactAuthor,
) -> PandaSqliteOpenConfig {
    PandaSqliteOpenConfig::new(path, vec![island.clone()]).with_trusted_author_key(
        PandaTrustedAuthorKey::new(
            island.clone(),
            author.principal().clone(),
            author.author_key(),
        ),
    )
}

fn authority_sqlite_config(
    path: impl Into<PathBuf>,
    island: &IslandId,
    authority: IslandAuthoritySnapshot,
) -> PandaSqliteOpenConfig {
    PandaSqliteOpenConfig::new(path, vec![island.clone()]).with_authority_snapshot(authority)
}

fn trust_author(store: &mut PandaFactStore, session: &BusSession, author: &PandaFactAuthor) {
    store
        .trust_author_key(
            session.island(),
            author.principal().clone(),
            author.author_key(),
        )
        .expect("trust p2panda author key");
}

fn trust_replica(store: &mut PandaFactStore, session: &BusSession) {
    store.trust_replica_peer(session.island(), session.principal().clone());
}

fn sync_scope(session: &BusSession, authors: &[&PandaFactAuthor]) -> PandaFactSyncScope {
    PandaFactSyncScope::from_trusted_authors(
        session.island().clone(),
        authors
            .iter()
            .map(|author| (author.principal().clone(), author.author_key())),
    )
}

#[derive(Clone, Copy)]
enum TestSyncBackend {
    Memory,
    Sqlite,
}

impl TestSyncBackend {
    fn name(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Sqlite => "sqlite",
        }
    }
}

async fn test_sync_store(
    backend: TestSyncBackend,
    bus: InMemoryBus,
    path: PathBuf,
    island: &IslandId,
    authors: &[&PandaFactAuthor],
) -> PandaFactStore {
    let mut store = match backend {
        TestSyncBackend::Memory => store_from_bus(bus),
        TestSyncBackend::Sqlite => PandaFactStore::open_sqlite(
            Arc::new(bus),
            PandaSqliteOpenConfig::new(path, vec![island.clone()]),
        )
        .await
        .expect("open sqlite sync test store"),
    };
    for author in authors {
        store
            .trust_author_key(island, author.principal().clone(), author.author_key())
            .expect("trust sync test author");
    }
    store
}

#[tokio::test]
async fn authorized_write_returns_insert_duplicate_and_conflict_outcomes() {
    let (mut store, authority) = store_with_authority();
    let session = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
    let fact_key = key("/facts/node/node-1/joined/1");

    let inserted = store
        .write_fact_payload(
            &session,
            &author,
            fact_key.clone(),
            FactPayload::from_static(b"one"),
        )
        .await
        .expect("write first fact");
    assert!(matches!(inserted, PandaFactWriteOutcome::Inserted(_)));

    let duplicate = store
        .write_fact_payload(
            &session,
            &author,
            fact_key.clone(),
            FactPayload::from_static(b"one"),
        )
        .await
        .expect("write duplicate fact");
    assert!(matches!(
        duplicate,
        PandaFactWriteOutcome::AlreadyPresent(_)
    ));

    let conflict = store
        .write_fact_payload(
            &session,
            &author,
            fact_key.clone(),
            FactPayload::from_static(b"two"),
        )
        .await
        .expect("write conflicting fact");
    assert!(matches!(conflict, PandaFactWriteOutcome::Conflict(_)));

    let candidates = store
        .list_candidates(session.island(), &pattern("/facts/node/>"), &session)
        .expect("list candidates");
    assert_eq!(candidates.len(), 2);
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.status() == CandidateStatus::Conflict)
    );
}

#[tokio::test]
async fn unauthorized_write_is_rejected_before_operation_ingest() {
    let (mut store, authority) = store_with_authority();
    let session = grant_prod(&authority, "intruder", Grant::empty());
    let author = PandaFactAuthor::new(principal("intruder"));
    let fact_key = key("/facts/node/evil/joined/1");

    let error = store
        .write_fact_payload(
            &session,
            &author,
            fact_key.clone(),
            FactPayload::from_static(b"payload"),
        )
        .await
        .expect_err("unauthorized write fails");

    assert!(matches!(
        error,
        PandaFactError::UnauthorizedWrite { key, .. } if key == fact_key
    ));
    assert!(
        store
            .list_candidates(session.island(), &pattern("/facts/>"), &session)
            .expect("list candidates")
            .is_empty()
    );
}

#[tokio::test]
async fn author_principal_must_match_session_principal() {
    let (mut store, authority) = store_with_authority();
    let session = grant_prod(
        &authority,
        "writer",
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    let forged_author = PandaFactAuthor::new(principal("admin"));
    let fact_key = key("/facts/node/node-1/joined/1");

    let error = store
        .write_fact_payload(
            &session,
            &forged_author,
            fact_key,
            FactPayload::from_static(b"payload"),
        )
        .await
        .expect_err("principal mismatch fails before authorization by name");

    assert!(matches!(
        error,
        PandaFactError::PrincipalMismatch { session, author }
            if session == principal("writer") && author == principal("admin")
    ));
}

#[tokio::test]
async fn read_payloads_respects_session_read_grants() {
    let (mut store, authority) = store_with_authority();
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/public/>")),
    );
    let reader = grant_prod(
        &authority,
        "reader",
        Grant::empty().with_fact_read(pattern("/facts/public/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
    store
        .write_fact_payload(
            &writer,
            &author,
            key("/facts/public/node-1"),
            FactPayload::from_static(b"public"),
        )
        .await
        .expect("write public fact");
    store
        .write_fact_payload(
            &writer,
            &author,
            key("/facts/private/node-1"),
            FactPayload::from_static(b"private"),
        )
        .await
        .expect("write private fact");

    let candidates = store
        .list_candidates(writer.island(), &pattern("/facts/>"), &reader)
        .expect("list candidates");
    assert_eq!(
        candidates
            .iter()
            .filter(|candidate| candidate.status() == CandidateStatus::Unauthorized)
            .count(),
        1
    );
    let payloads = store
        .read_payloads(writer.island(), &candidates, &reader)
        .expect("read payloads");
    assert_eq!(payloads.len(), 1);
    assert!(
        payloads
            .values()
            .any(|payload| payload.as_bytes() == b"public")
    );
}

#[tokio::test]
async fn read_payloads_rejects_forged_candidate_metadata() {
    let (mut store, authority) = store_with_authority();
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/public/>")),
    );
    let reader = grant_prod(
        &authority,
        "reader",
        Grant::empty().with_fact_read(pattern("/facts/public/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
    store
        .write_fact_payload(
            &writer,
            &author,
            key("/facts/private/node-1"),
            FactPayload::from_static(b"private"),
        )
        .await
        .expect("write private fact");
    let private_candidate = store
        .list_candidates(writer.island(), &pattern("/facts/>"), &writer)
        .expect("list private candidate")
        .into_iter()
        .find(|candidate| candidate.key() == &key("/facts/private/node-1"))
        .expect("private candidate exists");
    let forged = FactCandidate::new(
        writer.island().clone(),
        key("/facts/public/forged"),
        principal("writer"),
        private_candidate.content_hash().clone(),
        mvp_projection::FactKind::Unsupported,
        0,
        CandidateStatus::Verified,
    );

    let payloads = store
        .read_payloads(reader.island(), &[forged], &reader)
        .expect("read payloads");
    assert!(payloads.is_empty());
}

#[tokio::test]
async fn fact_source_does_not_authorize_cross_island_reads_with_session_grants() {
    let (mut store, authority) = store_with_authority();
    let laptop_writer = authority.grant_in(
        island("laptop"),
        principal("writer"),
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let prod_reader = grant_prod(
        &authority,
        "reader",
        Grant::empty().with_fact_read(pattern("/facts/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
    store
        .write_fact_payload(
            &laptop_writer,
            &author,
            key("/facts/node/laptop-only/joined/1"),
            FactPayload::from_static(b"laptop"),
        )
        .await
        .expect("write laptop fact");

    let laptop_candidates = store
        .list_candidates(
            laptop_writer.island(),
            &pattern("/facts/node/>"),
            &laptop_writer,
        )
        .expect("list laptop candidates through laptop session");
    assert_eq!(laptop_candidates.len(), 1);

    let prod_candidates = store
        .list_candidates(
            laptop_writer.island(),
            &pattern("/facts/node/>"),
            &prod_reader,
        )
        .expect("list laptop candidates through prod session");
    assert!(prod_candidates.is_empty());
    let payloads = store
        .read_payloads(laptop_writer.island(), &laptop_candidates, &prod_reader)
        .expect("read laptop payloads through prod session");
    assert!(payloads.is_empty());
}

#[tokio::test]
async fn exported_operations_import_into_an_empty_store() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let mut source = store_from_bus(bus.clone());
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let projection = grant_prod(
        &authority,
        "projection",
        Grant::empty().with_fact_read(pattern("/facts/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
    let fact_key = key("/facts/node/node-1/joined/1");
    source
        .write_fact_payload(
            &writer,
            &author,
            fact_key,
            FactPayload::from_static(b"payload"),
        )
        .await
        .expect("write source fact");
    let exported = source.export_operations().cloned().collect::<Vec<_>>();

    let mut imported = store_from_bus(bus);
    trust_author(&mut imported, &writer, &author);
    let outcome = imported
        .import_operation(&projection, &exported[0])
        .await
        .expect("import exported operation");
    assert!(matches!(outcome, PandaFactWriteOutcome::Inserted(_)));

    let candidates = imported
        .list_candidates(projection.island(), &pattern("/facts/node/>"), &projection)
        .expect("list imported candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].status(), CandidateStatus::Verified);
    let payloads = imported
        .read_payloads(projection.island(), &candidates, &projection)
        .expect("read imported payloads");
    assert!(
        payloads
            .values()
            .any(|payload| payload.as_bytes() == b"payload")
    );
}

#[tokio::test]
async fn sqlite_store_reopens_and_rebuilds_fact_indexes() {
    let directory = tempdir().expect("create tempdir");
    let path = directory.path().join("facts.sqlite");
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
    let fact_key = key("/facts/node/node-1/joined/1");
    let mut store = PandaFactStore::open_sqlite(
        Arc::new(bus.clone()),
        PandaSqliteOpenConfig::new(&path, vec![writer.island().clone()]),
    )
    .await
    .expect("open sqlite store");
    store
        .write_fact_payload(
            &writer,
            &author,
            fact_key.clone(),
            FactPayload::from_static(b"payload"),
        )
        .await
        .expect("write persistent fact");
    drop(store);

    let mut reopened = PandaFactStore::open_sqlite(
        Arc::new(bus),
        sqlite_config(&path, writer.island(), &author),
    )
    .await
    .expect("reopen sqlite store");
    let candidates = reopened
        .list_candidates(writer.island(), &pattern("/facts/node/>"), &writer)
        .expect("list reopened candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].status(), CandidateStatus::Verified);
    let payloads = reopened
        .read_payloads(writer.island(), &candidates, &writer)
        .expect("read reopened payload");
    assert!(
        payloads
            .values()
            .any(|payload| payload.as_bytes() == b"payload")
    );

    let duplicate = reopened
        .write_fact_payload(
            &writer,
            &author,
            fact_key.clone(),
            FactPayload::from_static(b"payload"),
        )
        .await
        .expect("write duplicate after reopen");
    assert!(matches!(
        duplicate,
        PandaFactWriteOutcome::AlreadyPresent(_)
    ));

    let conflict = reopened
        .write_fact_payload(
            &writer,
            &author,
            fact_key,
            FactPayload::from_static(b"conflict"),
        )
        .await
        .expect("write conflict after reopen");
    assert!(matches!(conflict, PandaFactWriteOutcome::Conflict(_)));
}

