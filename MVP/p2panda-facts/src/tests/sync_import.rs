#[tokio::test]
async fn shared_store_writes_reads_and_checks_preflight() {
    let (store, authority) = store_with_authority();
    let shared = SharedPandaFactStore::new(store);
    let session = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let readonly = grant_prod(
        &authority,
        "reader",
        Grant::empty().with_fact_read(pattern("/facts/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
    let fact_key = key("/facts/node/node-1/joined/1");

    assert!(
        shared
            .try_can_write_fact(&session, &fact_key)
            .expect("preflight")
    );
    assert!(
        !shared
            .try_can_write_fact(&readonly, &fact_key)
            .expect("readonly preflight")
    );

    let inserted = shared
        .write_fact_payload(
            &session,
            &author,
            fact_key.clone(),
            FactPayload::from_static(b"joined"),
        )
        .await
        .expect("write shared fact");
    let repeated = shared
        .write_fact_payload(
            &session,
            &author,
            fact_key,
            FactPayload::from_static(b"joined"),
        )
        .await
        .expect("repeat shared fact");
    assert!(matches!(inserted, PandaFactWriteOutcome::Inserted(_)));
    assert!(matches!(repeated, PandaFactWriteOutcome::AlreadyPresent(_)));

    let candidates = shared
        .list_candidates(session.island(), &pattern("/facts/node/>"), &session)
        .expect("list candidates");
    assert_eq!(candidates.len(), 1);
    let payloads = shared
        .read_payloads(session.island(), &candidates, &session)
        .expect("read payloads");
    assert_eq!(payloads.len(), 1);
}

#[tokio::test]
async fn shared_store_preserves_author_and_replica_import_modes() {
    let (source_store, authority) = store_with_authority();
    let source = SharedPandaFactStore::new(source_store);
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
    source
        .write_fact_payload(
            &writer,
            &author,
            key("/facts/node/node-1/joined/1"),
            FactPayload::from_static(b"joined"),
        )
        .await
        .expect("write source fact");
    let operations = source.export_operations().await;

    let (author_import_bus, author_import_authority) = InMemoryBus::new_with_authority();
    let author_import_writer = grant_prod(
        &author_import_authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let author_import = SharedPandaFactStore::new(store_from_bus(author_import_bus));
    author_import
        .trust_author_key(
            author_import_writer.island(),
            author_import_writer.principal().clone(),
            author.author_key(),
        )
        .await
        .expect("trust author");
    let imported = author_import
        .import_operation(&author_import_writer, &operations[0])
        .await
        .expect("direct author import");
    assert!(matches!(imported, PandaFactWriteOutcome::Inserted(_)));

    let (replica_import_bus, replica_import_authority) = InMemoryBus::new_with_authority();
    let replica_import_writer = grant_prod(
        &replica_import_authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let replica_importer = grant_prod(
        &replica_import_authority,
        "replica",
        Grant::empty().with_fact_read(pattern("/facts/>")),
    );
    let replica_import = SharedPandaFactStore::new(store_from_bus(replica_import_bus));
    replica_import
        .trust_author_key(
            replica_import_writer.island(),
            replica_import_writer.principal().clone(),
            author.author_key(),
        )
        .await
        .expect("trust replica author");
    replica_import
        .trust_replica_peer(
            replica_importer.island(),
            replica_importer.principal().clone(),
        )
        .await;
    let imported = replica_import
        .import_replica_operation(&replica_importer, &operations[0])
        .await
        .expect("trusted replica import");
    assert!(matches!(imported, PandaFactWriteOutcome::Inserted(_)));
}

#[tokio::test]
async fn shared_store_keeps_original_p2panda_write_errors() {
    let (store, authority) = store_with_authority();
    let shared = SharedPandaFactStore::new(store);
    let session = grant_prod(
        &authority,
        "writer",
        Grant::empty().with_fact_read(pattern("/facts/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
    let error = shared
        .write_fact_payload(
            &session,
            &author,
            key("/facts/node/node-1/joined/1"),
            FactPayload::from_static(b"joined"),
        )
        .await
        .expect_err("unauthorized write");
    assert!(matches!(error, PandaFactError::UnauthorizedWrite { .. }));
}

#[tokio::test]
async fn shared_store_fact_source_reports_unavailable_while_write_locked() {
    let (store, authority) = store_with_authority();
    let shared = SharedPandaFactStore::new(store);
    let session = grant_prod(
        &authority,
        "reader",
        Grant::empty().with_fact_read(pattern("/facts/>")),
    );
    let _guard = shared.store.lock().await;

    let error = shared
        .list_candidates(session.island(), &pattern("/facts/>"), &session)
        .expect_err("locked store");
    assert!(matches!(
        error,
        FactSourceError::Unavailable { name } if name == "p2panda fact store"
    ));
    let error = shared
        .try_can_write_fact(&session, &key("/facts/node/node-1/joined/1"))
        .expect_err("locked preflight");
    assert!(matches!(
        error,
        FactSourceError::Unavailable { name } if name == "p2panda fact store"
    ));
}

#[tokio::test]
async fn sqlite_reopen_requires_trusted_author_keys_for_stored_operations() {
    let directory = tempdir().expect("create tempdir");
    let path = directory.path().join("facts.sqlite");
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
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
            key("/facts/node/node-1/joined/1"),
            FactPayload::from_static(b"payload"),
        )
        .await
        .expect("write persistent fact");
    drop(store);

    let error = match PandaFactStore::open_sqlite(
        Arc::new(bus),
        PandaSqliteOpenConfig::new(&path, vec![writer.island().clone()]),
    )
    .await
    {
        Ok(_) => panic!("reopen without trusted author key fails"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        PandaFactError::UntrustedAuthorKey { principal, .. } if principal == PrincipalId::new("writer")
    ));
}

#[tokio::test]
async fn sqlite_reopen_with_new_authority_fails_closed_for_removed_writer_history() {
    let directory = tempdir().expect("create tempdir");
    let path = directory.path().join("facts.sqlite");
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer_session = grant_prod(
        &authority,
        "writer",
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    let writer = PandaFactAuthor::from_private_key_bytes(principal("writer"), [1; 32]);
    let replica = PandaFactAuthor::from_private_key_bytes(principal("replica"), [2; 32]);
    let mut fixture =
        authority_fixture_for_writer_and_replica(writer_session.island(), &writer, &replica).await;

    let mut store = PandaFactStore::open_sqlite(
        Arc::new(bus.clone()),
        authority_sqlite_config(&path, writer_session.island(), fixture.snapshot()),
    )
    .await
    .expect("open sqlite store with initial authority");
    store
        .write_fact_payload(
            &writer_session,
            &writer,
            key("/facts/authz/stale-before-removal"),
            FactPayload::from_static(b"stale-before-removal"),
        )
        .await
        .expect("writer can write before removal");
    drop(store);

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

    let error = match PandaFactStore::open_sqlite(
        Arc::new(bus),
        authority_sqlite_config(&path, writer_session.island(), fixture.snapshot()),
    )
    .await
    {
        Ok(_) => panic!("reopen with removed writer authority should fail closed"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        PandaFactError::UntrustedAuthorKey { principal, .. } if principal == PrincipalId::new("writer")
    ));
}

#[tokio::test]
async fn import_rejects_cross_island_untrusted_and_revoked_authors() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let mut source = store_from_bus(bus.clone());
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let laptop = authority.grant_in(
        island("laptop"),
        principal("projection"),
        Grant::empty().with_fact_read(pattern("/facts/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
    let fact_key = key("/facts/node/node-1/joined/1");
    source
        .write_fact_payload(
            &writer,
            &author,
            fact_key.clone(),
            FactPayload::from_static(b"payload"),
        )
        .await
        .expect("write source fact");
    let exported = source.export_operations().cloned().collect::<Vec<_>>();
    let [operation] = exported.as_slice() else {
        panic!("expected one exported operation");
    };

    let mut imported = store_from_bus(bus.clone());
    trust_author(&mut imported, &writer, &author);
    let cross_island = imported
        .import_operation(&laptop, operation)
        .await
        .expect_err("cross-island import fails");
    assert!(matches!(
        cross_island,
        PandaFactError::ImportIslandMismatch { .. }
    ));

    let mut untrusted = store_from_bus(bus.clone());
    let missing_key = untrusted
        .import_operation(&writer, operation)
        .await
        .expect_err("untrusted import fails");
    assert!(matches!(
        missing_key,
        PandaFactError::UntrustedAuthorKey { .. }
    ));

    authority.revoke(&writer);
    let mut revoked = store_from_bus(bus);
    trust_author(&mut revoked, &writer, &author);
    let revoked_author = revoked
        .import_operation(&writer, operation)
        .await
        .expect_err("revoked author import fails");
    assert!(matches!(
        revoked_author,
        PandaFactError::UnauthorizedWrite { key, .. } if key == fact_key
    ));
}

#[tokio::test]
async fn import_rejects_operation_signed_by_untrusted_key_for_claimed_author() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let trusted_author = PandaFactAuthor::new(principal("writer"));
    let forged_author = PandaFactAuthor::new(principal("writer"));
    let mut source = store_from_bus(bus.clone());
    source
        .write_fact_payload(
            &writer,
            &forged_author,
            key("/facts/node/node-1/joined/1"),
            FactPayload::from_static(b"payload"),
        )
        .await
        .expect("write forged-key source fact");
    let exported = source.export_operations().cloned().collect::<Vec<_>>();

    let mut imported = store_from_bus(bus);
    trust_author(&mut imported, &writer, &trusted_author);
    let error = imported
        .import_operation(&writer, &exported[0])
        .await
        .expect_err("mismatched author key fails");
    assert!(matches!(error, PandaFactError::AuthorKeyMismatch { .. }));
}

#[tokio::test]
async fn import_reports_out_of_order_operations_without_calling_them_invalid() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let mut source = store_from_bus(bus.clone());
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
    source
        .write_fact_payload(
            &writer,
            &author,
            key("/facts/node/node-1/joined/1"),
            FactPayload::from_static(b"one"),
        )
        .await
        .expect("write first operation");
    source
        .write_fact_payload(
            &writer,
            &author,
            key("/facts/node/node-2/joined/1"),
            FactPayload::from_static(b"two"),
        )
        .await
        .expect("write second operation");
    let exported = source.export_operations().cloned().collect::<Vec<_>>();
    let [first, second] = exported.as_slice() else {
        panic!("expected two exported operations");
    };

    let mut imported = store_from_bus(bus);
    trust_author(&mut imported, &writer, &author);
    let retry = imported
        .import_operation(&writer, second)
        .await
        .expect_err("second operation is out of order");
    assert!(matches!(
        retry,
        PandaFactError::OutOfOrderOperation {
            missing_operations: 1,
            ..
        }
    ));
    assert!(matches!(
        imported
            .import_operation(&writer, first)
            .await
            .expect("import first operation"),
        PandaFactWriteOutcome::Inserted(_)
    ));
    assert!(matches!(
        imported
            .import_operation(&writer, second)
            .await
            .expect("retry second operation after predecessor"),
        PandaFactWriteOutcome::Inserted(_)
    ));
}

#[tokio::test]
async fn sqlite_import_reports_out_of_order_operations_as_deferred() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let mut source = store_from_bus(bus.clone());
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
    source
        .write_fact_payload(
            &writer,
            &author,
            key("/facts/node/sqlite-1/joined/1"),
            FactPayload::from_static(b"one"),
        )
        .await
        .expect("write first operation");
    source
        .write_fact_payload(
            &writer,
            &author,
            key("/facts/node/sqlite-2/joined/1"),
            FactPayload::from_static(b"two"),
        )
        .await
        .expect("write second operation");
    let exported = source.export_operations().cloned().collect::<Vec<_>>();
    let [_first, second] = exported.as_slice() else {
        panic!("expected two exported operations");
    };

    let directory = tempdir().expect("create tempdir");
    let mut imported = PandaFactStore::open_sqlite(
        Arc::new(bus),
        PandaSqliteOpenConfig::new(directory.path().join("facts.sqlite"), vec![island("prod")]),
    )
    .await
    .expect("open sqlite fact store");
    trust_author(&mut imported, &writer, &author);

    let retry = imported
        .import_operation(&writer, second)
        .await
        .expect_err("second operation is out of order");
    assert!(matches!(
        retry,
        PandaFactError::OutOfOrderOperation {
            missing_operations: 1,
            ..
        }
    ));
}

#[tokio::test]
async fn memory_import_rejects_non_incremental_sequence_numbers() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let mut source = store_from_bus(bus.clone());
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
    source
        .write_fact_payload(
            &writer,
            &author,
            key("/facts/node/seq-1/joined/1"),
            FactPayload::from_static(b"one"),
        )
        .await
        .expect("write first operation");
    source
        .write_fact_payload(
            &writer,
            &author,
            key("/facts/node/seq-2/joined/1"),
            FactPayload::from_static(b"two"),
        )
        .await
        .expect("write second operation");
    let exported = source.export_operations().cloned().collect::<Vec<_>>();
    let [first, second] = exported.as_slice() else {
        panic!("expected two exported operations");
    };
    let mut non_incremental = second.to_p2panda_operation().expect("operation decodes");
    non_incremental.header.seq_num = 99;
    non_incremental.header.signature = None;
    non_incremental.header.sign(&author.key);
    non_incremental.hash = non_incremental.header.hash();
    let non_incremental =
        PandaFactOperation::from_p2panda_operation(non_incremental).expect("operation re-encodes");

    let mut imported = store_from_bus(bus);
    trust_author(&mut imported, &writer, &author);
    imported
        .import_operation(&writer, first)
        .await
        .expect("import first operation");
    let error = imported
        .import_operation(&writer, &non_incremental)
        .await
        .expect_err("non-incremental sequence fails");
    assert!(matches!(error, PandaFactError::OutOfOrderOperation { .. }));
}

#[tokio::test]
async fn imported_operations_still_enforce_reader_permissions() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let mut source = store_from_bus(bus.clone());
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let blind_reader = grant_prod(&authority, "blind-reader", Grant::empty());
    let author = PandaFactAuthor::new(principal("writer"));
    source
        .write_fact_payload(
            &writer,
            &author,
            key("/facts/node/node-1/joined/1"),
            FactPayload::from_static(b"payload"),
        )
        .await
        .expect("write source fact");
    let exported = source.export_operations().cloned().collect::<Vec<_>>();
    let [exported] = exported.as_slice() else {
        panic!("expected one exported operation");
    };

    let mut imported = store_from_bus(bus);
    trust_author(&mut imported, &writer, &author);
    let outcome = imported
        .import_operation(&blind_reader, exported)
        .await
        .expect("import operation through same-island session");
    assert!(matches!(outcome, PandaFactWriteOutcome::Inserted(_)));

    let candidates = imported
        .list_candidates(
            blind_reader.island(),
            &pattern("/facts/node/>"),
            &blind_reader,
        )
        .expect("list imported candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].status(), CandidateStatus::Unauthorized);
    let payloads = imported
        .read_payloads(blind_reader.island(), &candidates, &blind_reader)
        .expect("read imported payloads");
    assert!(payloads.is_empty());
}

#[tokio::test]
async fn importing_duplicates_and_conflicts_preserves_immutable_fact_semantics() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let mut source = store_from_bus(bus.clone());
    let session = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
    let fact_key = key("/facts/node/node-1/joined/1");
    source
        .write_fact_payload(
            &session,
            &author,
            fact_key.clone(),
            FactPayload::from_static(b"one"),
        )
        .await
        .expect("write first source fact");
    source
        .write_fact_payload(
            &session,
            &author,
            fact_key,
            FactPayload::from_static(b"two"),
        )
        .await
        .expect("write conflicting source fact");
    let exported = source.export_operations().cloned().collect::<Vec<_>>();

    let mut imported = store_from_bus(bus);
    trust_author(&mut imported, &session, &author);
    assert!(matches!(
        imported
            .import_operation(&session, &exported[0])
            .await
            .expect("import first operation"),
        PandaFactWriteOutcome::Inserted(_)
    ));
    assert!(matches!(
        imported
            .import_operation(&session, &exported[0])
            .await
            .expect("import duplicate operation"),
        PandaFactWriteOutcome::AlreadyPresent(_)
    ));
    assert!(matches!(
        imported
            .import_operation(&session, &exported[1])
            .await
            .expect("import conflict operation"),
        PandaFactWriteOutcome::Conflict(_)
    ));

    let candidates = imported
        .list_candidates(session.island(), &pattern("/facts/node/>"), &session)
        .expect("list imported conflict candidates");
    assert_eq!(candidates.len(), 2);
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.status() == CandidateStatus::Conflict)
    );
}

#[tokio::test]
async fn p2panda_sync_imports_missing_operations_and_repeated_sync_is_noop() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let left_replica = grant_prod(&authority, "left-replica", Grant::empty());
    let right_replica = grant_prod(&authority, "right-replica", Grant::empty());
    let author = PandaFactAuthor::new(principal("writer"));
    let mut left = store_from_bus(bus.clone());
    let mut right = store_from_bus(bus);
    trust_author(&mut left, &writer, &author);
    trust_author(&mut right, &writer, &author);
    trust_replica(&mut left, &left_replica);
    trust_replica(&mut right, &right_replica);

    left.write_fact_payload(
        &writer,
        &author,
        key("/facts/node/node-1/joined/1"),
        FactPayload::from_static(b"joined"),
    )
    .await
    .expect("write source fact");
    let scope = sync_scope(&writer, &[&author]);

    let report =
        sync_panda_fact_stores(&mut left, &left_replica, &mut right, &right_replica, &scope)
            .await
            .expect("sync stores");
    assert_eq!(report.right.received, 1);
    assert_eq!(report.right.imported, 1);
    let candidates = right
        .list_candidates(writer.island(), &pattern("/facts/node/>"), &writer)
        .expect("list synced candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].status(), CandidateStatus::Verified);

    let no_op =
        sync_panda_fact_stores(&mut left, &left_replica, &mut right, &right_replica, &scope)
            .await
            .expect("repeat sync stores");
    assert_eq!(no_op.left.received + no_op.right.received, 0);
}

#[tokio::test]
async fn p2panda_sync_supports_mixed_memory_and_sqlite_backends() {
    for (left_backend, right_backend) in [
        (TestSyncBackend::Memory, TestSyncBackend::Sqlite),
        (TestSyncBackend::Sqlite, TestSyncBackend::Memory),
    ] {
        run_mixed_backend_sync_case(left_backend, right_backend).await;
    }
}

async fn run_mixed_backend_sync_case(
    left_backend: TestSyncBackend,
    right_backend: TestSyncBackend,
) {
    let directory = tempdir().expect("create tempdir");
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let left_replica = grant_prod(&authority, "left-replica", Grant::empty());
    let right_replica = grant_prod(&authority, "right-replica", Grant::empty());
    let author = PandaFactAuthor::new(principal("writer"));
    let mut left = test_sync_store(
        left_backend,
        bus.clone(),
        directory
            .path()
            .join(format!("left-{}.sqlite", left_backend.name())),
        writer.island(),
        &[&author],
    )
    .await;
    let mut right = test_sync_store(
        right_backend,
        bus,
        directory
            .path()
            .join(format!("right-{}.sqlite", right_backend.name())),
        writer.island(),
        &[&author],
    )
    .await;
    trust_replica(&mut left, &left_replica);
    trust_replica(&mut right, &right_replica);

    left.write_fact_payload(
        &writer,
        &author,
        key("/facts/node/node-1/joined/1"),
        FactPayload::from_static(b"joined"),
    )
    .await
    .expect("write mixed-backend source fact");
    let scope = sync_scope(&writer, &[&author]);

    let report =
        sync_panda_fact_stores(&mut left, &left_replica, &mut right, &right_replica, &scope)
            .await
            .expect("sync mixed backends");
    assert_eq!(report.right.received, 1);
    assert_eq!(report.right.imported, 1);
    let candidates = right
        .list_candidates(writer.island(), &pattern("/facts/node/>"), &writer)
        .expect("list mixed-backend synced candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].status(), CandidateStatus::Verified);

    let no_op =
        sync_panda_fact_stores(&mut left, &left_replica, &mut right, &right_replica, &scope)
            .await
            .expect("repeat mixed-backend sync");
    assert_eq!(no_op.left.received + no_op.right.received, 0);
}

#[tokio::test]
async fn p2panda_sync_preserves_bidirectional_conflict_candidates() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer_a = grant_prod(
        &authority,
        "writer-a",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let writer_b = grant_prod(
        &authority,
        "writer-b",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let left_replica = grant_prod(&authority, "left-replica", Grant::empty());
    let right_replica = grant_prod(&authority, "right-replica", Grant::empty());
    let author_a = PandaFactAuthor::new(principal("writer-a"));
    let author_b = PandaFactAuthor::new(principal("writer-b"));
    let mut left = store_from_bus(bus.clone());
    let mut right = store_from_bus(bus);
    for store in [&mut left, &mut right] {
        trust_author(store, &writer_a, &author_a);
        trust_author(store, &writer_b, &author_b);
    }
    trust_replica(&mut left, &left_replica);
    trust_replica(&mut right, &right_replica);

    let fact_key = key("/facts/node/node-1/joined/1");
    left.write_fact_payload(
        &writer_a,
        &author_a,
        fact_key.clone(),
        FactPayload::from_static(b"left"),
    )
    .await
    .expect("write left fact");
    right
        .write_fact_payload(
            &writer_b,
            &author_b,
            fact_key,
            FactPayload::from_static(b"right"),
        )
        .await
        .expect("write right fact");

    let scope = sync_scope(&writer_a, &[&author_a, &author_b]);
    let report =
        sync_panda_fact_stores(&mut left, &left_replica, &mut right, &right_replica, &scope)
            .await
            .expect("sync bidirectional stores");
    assert_eq!(report.left.conflict, 1);
    assert_eq!(report.right.conflict, 1);

    for store in [&left, &right] {
        let candidates = store
            .list_candidates(writer_a.island(), &pattern("/facts/node/>"), &writer_a)
            .expect("list conflict candidates");
        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.status() == CandidateStatus::Conflict)
        );
    }
}

#[tokio::test]
async fn p2panda_sync_rejects_untrusted_replica_and_scope_key_substitution() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    let left_replica = grant_prod(&authority, "left-replica", Grant::empty());
    let right_replica = grant_prod(&authority, "right-replica", Grant::empty());
    let untrusted_replica = grant_prod(&authority, "untrusted-replica", Grant::empty());
    let author = PandaFactAuthor::new(principal("writer"));
    let imposter = PandaFactAuthor::new(principal("writer"));
    let mut left = store_from_bus(bus.clone());
    let mut right = store_from_bus(bus);
    trust_author(&mut left, &writer, &author);
    trust_author(&mut right, &writer, &author);
    trust_replica(&mut left, &left_replica);
    trust_replica(&mut right, &right_replica);

    let scope = sync_scope(&writer, &[&author]);
    let error = sync_panda_fact_stores(
        &mut left,
        &untrusted_replica,
        &mut right,
        &right_replica,
        &scope,
    )
    .await
    .expect_err("untrusted replica cannot start sync");
    assert!(matches!(
        error,
        PandaFactSyncError::UnauthorizedReplica {
            side: PandaFactSyncSide::Left,
            ..
        }
    ));

    let substituted = sync_scope(&writer, &[&imposter]);
    let error = sync_panda_fact_stores(
        &mut left,
        &left_replica,
        &mut right,
        &right_replica,
        &substituted,
    )
    .await
    .expect_err("scope key substitution is rejected");
    assert!(matches!(
        error,
        PandaFactSyncError::ScopeAuthorKeyMismatch {
            side: PandaFactSyncSide::Left,
            ..
        }
    ));
}

#[tokio::test]
async fn p2panda_sync_rejects_replica_island_mismatch_and_missing_scope_author() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    let left_replica = grant_prod(&authority, "left-replica", Grant::empty());
    let right_replica = grant_prod(&authority, "right-replica", Grant::empty());
    let laptop_replica =
        authority.grant_in(island("laptop"), principal("left-replica"), Grant::empty());
    let author = PandaFactAuthor::new(principal("writer"));
    let mut left = store_from_bus(bus.clone());
    let mut right = store_from_bus(bus);
    trust_author(&mut left, &writer, &author);
    trust_author(&mut right, &writer, &author);
    trust_replica(&mut left, &left_replica);
    trust_replica(&mut right, &right_replica);

    let scope = sync_scope(&writer, &[&author]);
    let error = sync_panda_fact_stores(
        &mut left,
        &laptop_replica,
        &mut right,
        &right_replica,
        &scope,
    )
    .await
    .expect_err("replica island mismatch is rejected");
    assert!(matches!(
        error,
        PandaFactSyncError::ReplicaIslandMismatch {
            side: PandaFactSyncSide::Left,
            ..
        }
    ));

    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    let left_replica = grant_prod(&authority, "left-replica", Grant::empty());
    let right_replica = grant_prod(&authority, "right-replica", Grant::empty());
    let author = PandaFactAuthor::new(principal("writer"));
    let mut left = store_from_bus(bus.clone());
    let mut right = store_from_bus(bus);
    trust_author(&mut right, &writer, &author);
    trust_replica(&mut left, &left_replica);
    trust_replica(&mut right, &right_replica);

    let scope = sync_scope(&writer, &[&author]);
    let error =
        sync_panda_fact_stores(&mut left, &left_replica, &mut right, &right_replica, &scope)
            .await
            .expect_err("missing scope author key is rejected");
    assert!(matches!(
        error,
        PandaFactSyncError::ScopeAuthorKeyMissing {
            side: PandaFactSyncSide::Left,
            ..
        }
    ));
}

#[tokio::test]
async fn p2panda_sync_rejects_received_operation_without_writer_grant() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty().with_fact_write(pattern("/facts/>")),
    );
    let left_replica = grant_prod(&authority, "left-replica", Grant::empty());
    let right_replica = grant_prod(&authority, "right-replica", Grant::empty());
    let author = PandaFactAuthor::new(principal("writer"));
    let mut left = store_from_bus(bus.clone());
    let mut right = store_from_bus(bus);
    for store in [&mut left, &mut right] {
        trust_author(store, &writer, &author);
    }
    trust_replica(&mut left, &left_replica);
    trust_replica(&mut right, &right_replica);

    left.write_fact_payload(
        &writer,
        &author,
        key("/facts/node/node-1/joined/1"),
        FactPayload::from_static(b"payload"),
    )
    .await
    .expect("write source fact before grant revocation");
    authority.revoke(&writer);

    let scope = sync_scope(&writer, &[&author]);
    let error =
        sync_panda_fact_stores(&mut left, &left_replica, &mut right, &right_replica, &scope)
            .await
            .expect_err("received operation without writer grant is rejected");
    assert!(matches!(
        error,
        PandaFactSyncError::Import {
            side: PandaFactSyncSide::Right,
            source: PandaFactError::UnauthorizedWrite { .. },
        }
    ));
    assert!(
        right
            .list_candidates(writer.island(), &pattern("/facts/>"), &writer)
            .expect("list destination candidates")
            .is_empty()
    );
}

#[tokio::test]
async fn duplicate_import_rejects_same_header_with_corrupted_body() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = grant_prod(
        &authority,
        "writer",
        Grant::empty()
            .with_fact_write(pattern("/facts/>"))
            .with_fact_read(pattern("/facts/>")),
    );
    let author = PandaFactAuthor::new(principal("writer"));
    let mut source = store_from_bus(bus.clone());
    source
        .write_fact_payload(
            &writer,
            &author,
            key("/facts/node/node-corrupt/joined/1"),
            FactPayload::from_static(b"valid-payload"),
        )
        .await
        .expect("write source operation");
    let operation = source
        .export_operations()
        .next()
        .expect("operation was recorded")
        .clone();
    let mut imported = store_from_bus(bus);
    trust_author(&mut imported, &writer, &author);
    imported
        .import_operation(&writer, &operation)
        .await
        .expect("import valid operation once");

    let corrupted = PandaFactOperation::new(operation.header_bytes(), b"corrupted-body".to_vec());
    let error = imported
        .import_operation(&writer, &corrupted)
        .await
        .expect_err("same signed header with changed body is rejected");
    assert!(matches!(error, PandaFactError::InvalidOperation));
}
