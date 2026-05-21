use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Instant;

use mvp_bus::{FactPayload, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_p2panda_authz::{
    IslandAuthzStore, IslandMemberAuthorKey, IslandMemberEpoch, IslandMemberKeyBinding,
    IslandRootAuthority, ReplicaImportAccess,
};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactAuthoritySource, PandaFactError, PandaFactOperation, PandaFactStore,
    PandaFactWriteOutcome, PandaSqliteOpenConfig,
};
use p2panda_core::SigningKey;
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::{fact_key, fact_pattern};
use crate::metrics::{reset_dir, scenario_dir, write_json};

#[derive(Debug, Serialize)]
struct P2pandaAuthMembershipReport {
    scenario: &'static str,
    accepted_writes: usize,
    accepted_imports: usize,
    rejected_writes: usize,
    rejected_imports: usize,
    unauthorized_write_rejected: bool,
    granted_non_member_write_rejected: bool,
    active_member_without_fact_grant_import_rejected: bool,
    demoted_writer_write_rejected: bool,
    demoted_writer_import_accepted: bool,
    removed_writer_write_rejected: bool,
    removed_writer_import_rejected: bool,
    reinvited_new_key_accepted: bool,
    reinvited_old_key_write_rejected: bool,
    reinvited_old_key_import_rejected: bool,
    old_binding_replay_did_not_resurrect: bool,
    cross_island_rejected: bool,
    restart_preserved_decisions: bool,
    unauthorized_imports_accepted: usize,
    elapsed_ms: u128,
}

#[derive(Debug)]
struct Counters {
    accepted_writes: usize,
    accepted_imports: usize,
    rejected_writes: usize,
    rejected_imports: usize,
    unauthorized_imports_accepted: usize,
}

impl Counters {
    fn new() -> Self {
        Self {
            accepted_writes: 0,
            accepted_imports: 0,
            rejected_writes: 0,
            rejected_imports: 0,
            unauthorized_imports_accepted: 0,
        }
    }
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|error| {
            format!("create tokio runtime for p2panda auth membership contract: {error}")
        })?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("p2panda-auth-membership-contract");
    reset_dir(&root)?;

    let prod = IslandId::new("prod");
    let laptop = IslandId::new("laptop");
    let root_author = PandaFactAuthor::from_private_key_bytes(PrincipalId::new("root"), [9; 32]);
    let root_key = SigningKey::from_bytes(&[9; 32]);
    let root_binding = member_binding(&prod, &root_author, 1)?;
    let root_authority = IslandRootAuthority::new(root_binding.clone());
    let membership_path = root.join("membership.sqlite");
    let membership = IslandAuthzStore::open(&membership_path, prod.clone(), root_authority.clone())
        .await
        .map_err(|error| format!("open prod membership store: {error}"))?;
    let mut authz = membership
        .create_root(root_binding.clone(), &root_key)
        .await
        .map_err(|error| format!("create prod membership root: {error}"))?;

    let writer_v1 = PandaFactAuthor::from_private_key_bytes(PrincipalId::new("writer"), [1; 32]);
    let writer_v2 = PandaFactAuthor::from_private_key_bytes(PrincipalId::new("writer"), [2; 32]);
    let producer = PandaFactAuthor::from_private_key_bytes(PrincipalId::new("producer"), [3; 32]);
    let replica = PandaFactAuthor::from_private_key_bytes(PrincipalId::new("replica"), [4; 32]);
    let no_grant_writer =
        PandaFactAuthor::from_private_key_bytes(PrincipalId::new("no-grant-writer"), [5; 32]);
    let outsider = PandaFactAuthor::from_private_key_bytes(PrincipalId::new("outsider"), [6; 32]);
    add_writer(
        &membership,
        &mut authz,
        &root_binding,
        &root_key,
        &prod,
        &writer_v1,
        1,
    )
    .await?;
    add_writer(
        &membership,
        &mut authz,
        &root_binding,
        &root_key,
        &prod,
        &producer,
        1,
    )
    .await?;
    add_writer(
        &membership,
        &mut authz,
        &root_binding,
        &root_key,
        &prod,
        &no_grant_writer,
        1,
    )
    .await?;
    membership
        .add_replica_importer(
            &mut authz,
            &root_binding,
            &root_key,
            member_binding(&prod, &replica, 1)?,
            ReplicaImportAccess::Read,
        )
        .await
        .map_err(|error| format!("add replica importer membership: {error}"))?;

    let (bus, authority) = InMemoryBus::new_with_authority();
    let bus = Arc::new(bus);
    let writer_session = authority.grant_in(
        prod.clone(),
        PrincipalId::new("writer"),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/>")?)
            .with_fact_read(fact_pattern("/facts/>")?),
    );
    let producer_session = authority.grant_in(
        prod.clone(),
        PrincipalId::new("producer"),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/>")?)
            .with_fact_read(fact_pattern("/facts/>")?),
    );
    let replica_session =
        authority.grant_in(prod.clone(), PrincipalId::new("replica"), Grant::empty());
    let no_grant_session = authority.grant_in(
        prod.clone(),
        PrincipalId::new("no-grant-writer"),
        Grant::empty(),
    );
    let laptop_session = authority.grant_in(laptop, PrincipalId::new("replica"), Grant::empty());
    let outsider_session = authority.grant_in(
        prod.clone(),
        PrincipalId::new("outsider"),
        Grant::empty().with_fact_write(fact_pattern("/facts/>")?),
    );

    let mut counters = Counters::new();
    let mut source = open_fact_store(
        Arc::clone(&bus),
        root.join("source.sqlite"),
        &prod,
        &membership,
    )
    .await?;
    let mut target = open_fact_store(
        Arc::clone(&bus),
        root.join("target.sqlite"),
        &prod,
        &membership,
    )
    .await?;
    let mut stale_writer_source = open_fact_store(
        Arc::clone(&bus),
        root.join("stale-writer-source.sqlite"),
        &prod,
        &membership,
    )
    .await?;
    let initial_snapshot = membership
        .authority_snapshot()
        .await
        .map_err(|error| format!("load initial membership authority: {error}"))?;
    require(
        initial_snapshot
            .active_writer(no_grant_writer.principal())
            .is_some(),
        "no-grant writer is active in membership before fact-grant denial",
    )?;

    expect_inserted(
        source
            .write_fact_payload(
                &writer_session,
                &writer_v1,
                fact_key("/facts/authz/writer-v1-before-demotion")?,
                FactPayload::from_static(b"writer-v1-before-demotion"),
            )
            .await,
        "writer v1 initial write",
    )?;
    counters.accepted_writes += 1;
    let writer_v1_operation = last_operation(&source, "writer v1 operation")?;

    expect_inserted(
        source
            .write_fact_payload(
                &producer_session,
                &producer,
                fact_key("/facts/authz/producer-before-demotion")?,
                FactPayload::from_static(b"producer-before-demotion"),
            )
            .await,
        "producer write",
    )?;
    counters.accepted_writes += 1;
    let producer_operation = last_operation(&source, "producer operation")?;

    let unauthorized_write_rejected = matches!(
        source
            .write_fact_payload(
                &no_grant_session,
                &no_grant_writer,
                fact_key("/facts/authz/no-grant")?,
                FactPayload::from_static(b"no-grant"),
            )
            .await,
        Err(PandaFactError::UnauthorizedWrite { .. })
    );
    require(
        unauthorized_write_rejected,
        "unauthorized writer without fact grant was rejected",
    )?;
    counters.rejected_writes += 1;

    let granted_non_member_write_rejected = matches!(
        source
            .write_fact_payload(
                &outsider_session,
                &outsider,
                fact_key("/facts/authz/granted-non-member")?,
                FactPayload::from_static(b"granted-non-member"),
            )
            .await,
        Err(PandaFactError::UntrustedAuthorKey { .. })
    );
    require(
        granted_non_member_write_rejected,
        "fact grant did not authorize non-member writer",
    )?;
    counters.rejected_writes += 1;

    let no_grant_operation = operation_written_with_separate_authorizer(
        &prod,
        &root,
        &membership,
        &no_grant_writer,
        "/facts/authz/no-grant-import",
        b"no-grant-import",
    )
    .await?;
    let active_member_without_fact_grant_import = target
        .import_replica_operation(&replica_session, &no_grant_operation)
        .await;
    let active_member_without_fact_grant_import_rejected = matches!(
        &active_member_without_fact_grant_import,
        Err(PandaFactError::UnauthorizedWrite { .. })
    );
    if active_member_without_fact_grant_import.is_ok() {
        counters.unauthorized_imports_accepted += 1;
    }
    require(
        active_member_without_fact_grant_import_rejected,
        "active member import still requires original author fact-write grant",
    )?;
    counters.rejected_imports += 1;

    expect_inserted(
        target
            .import_replica_operation(&replica_session, &producer_operation)
            .await,
        "replica import before demotion",
    )?;
    counters.accepted_imports += 1;

    membership
        .demote_to_replica_importer(
            &mut authz,
            &root_binding,
            &root_key,
            member_binding(&prod, &writer_v1, 1)?.member_id(),
            ReplicaImportAccess::Read,
        )
        .await
        .map_err(|error| format!("demote writer to replica importer: {error}"))?;
    refresh_store_authority(&mut source, &membership).await?;
    let demoted_writer_write_rejected = matches!(
        source
            .write_fact_payload(
                &writer_session,
                &writer_v1,
                fact_key("/facts/authz/demoted-writer")?,
                FactPayload::from_static(b"demoted-writer"),
            )
            .await,
        Err(PandaFactError::UntrustedAuthorKey { .. })
    );
    require(
        demoted_writer_write_rejected,
        "demoted writer lost write authority",
    )?;
    counters.rejected_writes += 1;

    let mut demoted_target = open_fact_store(
        Arc::clone(&bus),
        root.join("demoted-target.sqlite"),
        &prod,
        &membership,
    )
    .await?;
    expect_inserted(
        demoted_target
            .import_replica_operation(&writer_session, &producer_operation)
            .await,
        "demoted writer replica import",
    )?;
    let demoted_writer_import_accepted = true;
    counters.accepted_imports += 1;

    membership
        .remove_member(
            &mut authz,
            &root_binding,
            &root_key,
            member_binding(&prod, &writer_v1, 1)?.member_id(),
        )
        .await
        .map_err(|error| format!("remove writer v1 membership: {error}"))?;
    refresh_store_authority(&mut source, &membership).await?;
    let removed_writer_write_rejected = matches!(
        source
            .write_fact_payload(
                &writer_session,
                &writer_v1,
                fact_key("/facts/authz/removed-writer")?,
                FactPayload::from_static(b"removed-writer"),
            )
            .await,
        Err(PandaFactError::UntrustedAuthorKey { .. })
    );
    require(
        removed_writer_write_rejected,
        "removed writer lost write authority",
    )?;
    counters.rejected_writes += 1;

    expect_inserted(
        stale_writer_source
            .write_fact_payload(
                &writer_session,
                &writer_v1,
                fact_key("/facts/authz/stale-writer-after-removal")?,
                FactPayload::from_static(b"stale-writer-after-removal"),
            )
            .await,
        "stale writer operation created from pre-removal authority",
    )?;
    let stale_writer_operation =
        last_operation(&stale_writer_source, "stale writer after removal operation")?;

    let mut removed_target = open_fact_store(
        Arc::clone(&bus),
        root.join("removed-target.sqlite"),
        &prod,
        &membership,
    )
    .await?;
    let removed_writer_import = removed_target
        .import_replica_operation(&writer_session, &producer_operation)
        .await;
    let removed_writer_import_rejected = matches!(
        &removed_writer_import,
        Err(PandaFactError::UnauthorizedReplicaImport { .. })
    );
    if removed_writer_import.is_ok() {
        counters.unauthorized_imports_accepted += 1;
    }
    require(
        removed_writer_import_rejected,
        "removed writer lost replica import authority",
    )?;
    counters.rejected_imports += 1;

    let mut stale_rejection_target = open_fact_store(
        Arc::clone(&bus),
        root.join("stale-rejection-target.sqlite"),
        &prod,
        &membership,
    )
    .await?;
    let stale_writer_import = stale_rejection_target
        .import_replica_operation(&replica_session, &stale_writer_operation)
        .await;
    let stale_writer_import_rejected = matches!(
        &stale_writer_import,
        Err(PandaFactError::UntrustedAuthorKey { .. })
    );
    if stale_writer_import.is_ok() {
        counters.unauthorized_imports_accepted += 1;
    }
    require(
        stale_writer_import_rejected,
        "updated authority rejects fresh operation from removed writer key",
    )?;
    counters.rejected_imports += 1;

    add_writer(
        &membership,
        &mut authz,
        &root_binding,
        &root_key,
        &prod,
        &writer_v2,
        2,
    )
    .await?;
    refresh_store_authority(&mut source, &membership).await?;
    expect_inserted(
        source
            .write_fact_payload(
                &writer_session,
                &writer_v2,
                fact_key("/facts/authz/writer-v2-after-reinvite")?,
                FactPayload::from_static(b"writer-v2-after-reinvite"),
            )
            .await,
        "writer v2 write",
    )?;
    let reinvited_new_key_accepted = true;
    counters.accepted_writes += 1;
    let reinvited_old_key_write_rejected = matches!(
        source
            .write_fact_payload(
                &writer_session,
                &writer_v1,
                fact_key("/facts/authz/writer-v1-after-reinvite")?,
                FactPayload::from_static(b"writer-v1-after-reinvite"),
            )
            .await,
        Err(PandaFactError::AuthorKeyMismatch { .. })
    );
    require(
        reinvited_old_key_write_rejected,
        "old writer key cannot write after reinvite",
    )?;
    counters.rejected_writes += 1;

    let mut old_key_target = open_fact_store(
        Arc::clone(&bus),
        root.join("old-key-target.sqlite"),
        &prod,
        &membership,
    )
    .await?;
    let reinvited_old_key_import = old_key_target
        .import_replica_operation(&replica_session, &writer_v1_operation)
        .await;
    let reinvited_old_key_import_rejected = matches!(
        &reinvited_old_key_import,
        Err(PandaFactError::AuthorKeyMismatch { .. })
    );
    if reinvited_old_key_import.is_ok() {
        counters.unauthorized_imports_accepted += 1;
    }
    require(
        reinvited_old_key_import_rejected,
        "old writer key cannot import after reinvite",
    )?;
    counters.rejected_imports += 1;

    let reopened_membership =
        IslandAuthzStore::open(&membership_path, prod.clone(), root_authority)
            .await
            .map_err(|error| format!("reopen membership store: {error}"))?;
    let mut reopened_source = open_fact_store(
        Arc::clone(&bus),
        root.join("reopened-source.sqlite"),
        &prod,
        &reopened_membership,
    )
    .await?;
    let old_binding_replay_did_not_resurrect = matches!(
        reopened_source
            .write_fact_payload(
                &writer_session,
                &writer_v1,
                fact_key("/facts/authz/reopened-old-writer")?,
                FactPayload::from_static(b"reopened-old-writer"),
            )
            .await,
        Err(PandaFactError::AuthorKeyMismatch { .. })
    );
    require(
        old_binding_replay_did_not_resurrect,
        "membership replay did not resurrect old writer key",
    )?;
    counters.rejected_writes += 1;
    expect_inserted(
        reopened_source
            .write_fact_payload(
                &writer_session,
                &writer_v2,
                fact_key("/facts/authz/reopened-new-writer")?,
                FactPayload::from_static(b"reopened-new-writer"),
            )
            .await,
        "reopened writer v2 write",
    )?;
    let restart_preserved_decisions = true;
    counters.accepted_writes += 1;

    let mut laptop_target = PandaFactStore::new(bus.clone());
    laptop_target.install_authority_snapshot(
        reopened_membership
            .authority_snapshot()
            .await
            .map_err(|error| format!("load prod authority snapshot: {error}"))?,
    );
    let cross_island_import = laptop_target
        .import_operation(&laptop_session, &producer_operation)
        .await;
    let cross_island_rejected = matches!(
        &cross_island_import,
        Err(PandaFactError::ImportIslandMismatch { .. })
    );
    if cross_island_import.is_ok() {
        counters.unauthorized_imports_accepted += 1;
    }
    require(
        cross_island_rejected,
        "cross-island import hit island-mismatch rejection",
    )?;
    counters.rejected_imports += 1;
    assert_eq_named(
        "p2panda unauthorized imports accepted",
        counters.unauthorized_imports_accepted,
        0,
    )?;

    let report = P2pandaAuthMembershipReport {
        scenario: "p2panda-auth-membership-contract",
        accepted_writes: counters.accepted_writes,
        accepted_imports: counters.accepted_imports,
        rejected_writes: counters.rejected_writes,
        rejected_imports: counters.rejected_imports,
        unauthorized_write_rejected,
        granted_non_member_write_rejected,
        active_member_without_fact_grant_import_rejected,
        demoted_writer_write_rejected,
        demoted_writer_import_accepted,
        removed_writer_write_rejected,
        removed_writer_import_rejected,
        reinvited_new_key_accepted,
        reinvited_old_key_write_rejected,
        reinvited_old_key_import_rejected,
        old_binding_replay_did_not_resurrect,
        cross_island_rejected,
        restart_preserved_decisions,
        unauthorized_imports_accepted: counters.unauthorized_imports_accepted,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let json = write_json(
        &root.join("p2panda-auth-membership-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS p2panda-auth-membership-contract");
    Ok(())
}

async fn add_writer(
    membership: &IslandAuthzStore,
    authz: &mut mvp_p2panda_authz::IslandAuthz,
    root: &IslandMemberKeyBinding,
    root_key: &SigningKey,
    island: &IslandId,
    author: &PandaFactAuthor,
    epoch: u64,
) -> Result<(), String> {
    membership
        .add_writer(
            authz,
            root,
            root_key,
            member_binding(island, author, epoch)?,
        )
        .await
        .map(|_| ())
        .map_err(|error| format!("add writer membership for {}: {error}", author.principal()))
}

async fn open_fact_store(
    bus: Arc<InMemoryBus>,
    path: std::path::PathBuf,
    island: &IslandId,
    membership: &IslandAuthzStore,
) -> Result<PandaFactStore, String> {
    let authority_source = PandaFactAuthoritySource::from_membership_store(membership)
        .await
        .map_err(|error| format!("load p2panda membership authority: {error}"))?;
    PandaFactStore::open_sqlite(
        bus,
        PandaSqliteOpenConfig::new(path, vec![island.clone()])
            .with_authority_source(authority_source),
    )
    .await
    .map_err(|error| format!("open membership-backed p2panda fact store: {error}"))
}

async fn operation_written_with_separate_authorizer(
    island: &IslandId,
    root: &std::path::Path,
    membership: &IslandAuthzStore,
    author: &PandaFactAuthor,
    key: &str,
    payload: &'static [u8],
) -> Result<PandaFactOperation, String> {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let session = authority.grant_in(
        island.clone(),
        author.principal().clone(),
        Grant::empty().with_fact_write(fact_pattern("/facts/>")?),
    );
    let mut store = open_fact_store(
        Arc::new(bus),
        root.join(format!(
            "{}-source.sqlite",
            author.principal().as_str().replace('-', "_")
        )),
        island,
        membership,
    )
    .await?;
    expect_inserted(
        store
            .write_fact_payload(
                &session,
                author,
                fact_key(key)?,
                FactPayload::from_static(payload),
            )
            .await,
        "separate-authorizer source operation",
    )?;
    last_operation(&store, "separate-authorizer source operation")
}

async fn refresh_store_authority(
    store: &mut PandaFactStore,
    membership: &IslandAuthzStore,
) -> Result<(), String> {
    let snapshot = membership
        .authority_snapshot()
        .await
        .map_err(|error| format!("reload membership authority snapshot: {error}"))?;
    store.install_authority_snapshot(snapshot);
    Ok(())
}

fn member_binding(
    island: &IslandId,
    author: &PandaFactAuthor,
    epoch: u64,
) -> Result<IslandMemberKeyBinding, String> {
    let epoch = NonZeroU64::new(epoch)
        .map(IslandMemberEpoch::new)
        .ok_or_else(|| "membership epoch must be non-zero".to_string())?;
    Ok(IslandMemberKeyBinding::new(
        island.clone(),
        author.principal().clone(),
        epoch,
        IslandMemberAuthorKey::from(author.author_key()),
    ))
}

fn expect_inserted(
    result: Result<PandaFactWriteOutcome, PandaFactError>,
    label: &str,
) -> Result<(), String> {
    match result {
        Ok(PandaFactWriteOutcome::Inserted(_)) => Ok(()),
        Ok(other) => Err(format!("expected {label} to insert, got {other:?}")),
        Err(error) => Err(format!("expected {label} to insert, got error: {error}")),
    }
}

fn require(condition: bool, label: &str) -> Result<(), String> {
    if condition {
        return Ok(());
    }
    Err(format!("p2panda auth membership check failed: {label}"))
}

fn last_operation(store: &PandaFactStore, label: &str) -> Result<PandaFactOperation, String> {
    store
        .export_operations()
        .last()
        .cloned()
        .ok_or_else(|| format!("{label} was not exported"))
}
