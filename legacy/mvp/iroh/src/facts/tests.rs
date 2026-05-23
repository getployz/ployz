use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use mvp_bus::{
    FactContentHash, FactKey, FactKeyPattern, Grant, IslandId, PrincipalId, harness::InMemoryBus,
};
use mvp_identity::NodeId;
use mvp_projection::{
    CandidateStatus, FactCandidate, FactKind, FactSource, NodeJoinedFact, ProjectionFactPayload,
};

use super::{
    IrohDocsFactSource, IrohFactError, IrohFactLocalView, IrohFactNode, IrohImmutableWriteOutcome,
    IrohRejectedFactReason, LocalFactEntry, LocalFactMetadata,
};

fn island(value: &str) -> IslandId {
    IslandId::new(value)
}

fn principal(value: &str) -> PrincipalId {
    PrincipalId::new(value)
}

fn key(value: &str) -> FactKey {
    FactKey::parse(value).expect("fact key parses")
}

fn pattern(value: &str) -> FactKeyPattern {
    FactKeyPattern::parse(value).expect("fact pattern parses")
}

fn node_joined_payload(node_id: &str, epoch: u64, overlay_ip: &str) -> mvp_bus::Payload {
    ProjectionFactPayload::NodeJoined(NodeJoinedFact {
        node_id: NodeId::new(node_id),
        epoch,
        overlay_ip: overlay_ip.to_string(),
        iroh_endpoint_id: "iroh-test".to_string(),
        wg_public_key: "wg-test".to_string(),
    })
    .to_fact_bytes()
    .expect("payload serializes")
    .into()
}

#[tokio::test]
async fn docs_sync_updates_synchronous_fact_source_view() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = authority.grant_in(
        island("prod"),
        principal("node-1"),
        Grant::empty().with_fact_write(pattern("/facts/node/>")),
    );
    let projection = authority.grant_in(
        island("prod"),
        principal("projection"),
        Grant::empty().with_fact_read(pattern("/facts/node/>")),
    );
    let payload = ProjectionFactPayload::NodeJoined(NodeJoinedFact {
        node_id: NodeId::new("node-1"),
        epoch: 1,
        overlay_ip: "fd00::1".to_string(),
        iroh_endpoint_id: "iroh-test".to_string(),
        wg_public_key: "wg-test".to_string(),
    })
    .to_fact_bytes()
    .expect("payload serializes");

    let node_a = IrohFactNode::memory().await.expect("spawn node a");
    let node_b = IrohFactNode::memory().await.expect("spawn node b");
    let author = node_a
        .create_author(writer.principal().clone())
        .await
        .expect("create author");
    let doc_a = node_a
        .create_fact_doc(island("prod"))
        .await
        .expect("create fact doc");
    let fact_key = key("/facts/node/node-1/joined/1");
    let written_hash = doc_a
        .write_fact_payload(&author, fact_key.clone(), payload.into(), &bus)
        .await
        .expect("write fact through iroh docs");
    let ticket = doc_a.share().await.expect("share doc");
    let doc_b = node_b.import_fact_doc(ticket).await.expect("import doc");
    doc_b
        .wait_for_content_hash(&fact_key, &written_hash, Duration::from_secs(5))
        .await
        .expect("remote key becomes visible");

    let source = IrohDocsFactSource::new(node_b.local_view(), Arc::new(bus.clone()));
    let candidates = source
        .list_candidates(&island("prod"), &pattern("/facts/node/>"), &projection)
        .expect("list candidates");
    let payloads = source
        .read_payloads(&island("prod"), &candidates, &projection)
        .expect("read payloads");
    let projected_payload = payloads
        .get(&written_hash)
        .expect("payload is available to projection");

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].kind(), FactKind::NodeJoined);
    assert_eq!(*candidates[0].content_hash(), written_hash);
    assert_eq!(
        FactContentHash::for_payload(projected_payload),
        written_hash
    );

    node_a.shutdown().await.expect("shutdown node a");
    node_b.shutdown().await.expect("shutdown node b");
}

#[tokio::test]
async fn docs_source_reports_conflicting_candidates_to_projection() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer_one = authority.grant_in(
        island("prod"),
        principal("node-1"),
        Grant::empty().with_fact_write(pattern("/facts/node/>")),
    );
    let writer_two = authority.grant_in(
        island("prod"),
        principal("node-2"),
        Grant::empty().with_fact_write(pattern("/facts/node/>")),
    );
    let projection = authority.grant_in(
        island("prod"),
        principal("projection"),
        Grant::empty().with_fact_read(pattern("/facts/node/>")),
    );

    let node = IrohFactNode::memory().await.expect("spawn node");
    let doc = node
        .create_fact_doc(island("prod"))
        .await
        .expect("create fact doc");
    let author_one = node
        .create_author(writer_one.principal().clone())
        .await
        .expect("create author one");
    let author_two = node
        .create_author(writer_two.principal().clone())
        .await
        .expect("create author two");
    let fact_key = key("/facts/node/node-1/joined/1");
    doc.write_fact_payload(
        &author_one,
        fact_key.clone(),
        node_joined_payload("node-1", 1, "fd00::1"),
        &bus,
    )
    .await
    .expect("write first candidate");
    doc.write_fact_payload(
        &author_two,
        fact_key,
        node_joined_payload("node-1", 1, "fd00::2"),
        &bus,
    )
    .await
    .expect("write conflicting candidate");

    let source = IrohDocsFactSource::new(node.local_view(), Arc::new(bus.clone()));
    let candidates = source
        .list_candidates(&island("prod"), &pattern("/facts/node/>"), &projection)
        .expect("list candidates");
    let payloads = source
        .read_payloads(&island("prod"), &candidates, &projection)
        .expect("read payloads");

    assert_eq!(candidates.len(), 2);
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.status() == CandidateStatus::Conflict)
    );
    assert_eq!(payloads.len(), 2);

    node.shutdown().await.expect("shutdown node");
}

#[tokio::test]
async fn docs_source_marks_revoked_author_unauthorized() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = authority.grant_in(
        island("prod"),
        principal("node-1"),
        Grant::empty().with_fact_write(pattern("/facts/node/>")),
    );
    let projection = authority.grant_in(
        island("prod"),
        principal("projection"),
        Grant::empty().with_fact_read(pattern("/facts/node/>")),
    );

    let node = IrohFactNode::memory().await.expect("spawn node");
    let doc = node
        .create_fact_doc(island("prod"))
        .await
        .expect("create fact doc");
    let author = node
        .create_author(writer.principal().clone())
        .await
        .expect("create author");
    doc.write_fact_payload(
        &author,
        key("/facts/node/node-1/joined/1"),
        node_joined_payload("node-1", 1, "fd00::1"),
        &bus,
    )
    .await
    .expect("write candidate before revoke");
    assert!(authority.revoke(&writer));

    let source = IrohDocsFactSource::new(node.local_view(), Arc::new(bus.clone()));
    let candidates = source
        .list_candidates(&island("prod"), &pattern("/facts/node/>"), &projection)
        .expect("list candidates");
    let payloads = source
        .read_payloads(&island("prod"), &candidates, &projection)
        .expect("read payloads");

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].status(), CandidateStatus::Unauthorized);
    assert!(payloads.is_empty());

    let forged = FactCandidate::new(
        candidates[0].island().clone(),
        candidates[0].key().clone(),
        candidates[0].author().clone(),
        candidates[0].content_hash().clone(),
        candidates[0].kind(),
        candidates[0].epoch(),
        CandidateStatus::Verified,
    );
    let forged_payloads = source
        .read_payloads(&island("prod"), &[forged], &projection)
        .expect("read forged payloads");
    assert!(forged_payloads.is_empty());

    node.shutdown().await.expect("shutdown node");
}

#[tokio::test]
async fn docs_write_rejects_unauthorized_principal_without_local_candidate() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = authority.grant_in(island("prod"), principal("node-1"), Grant::empty());
    let projection = authority.grant_in(
        island("prod"),
        principal("projection"),
        Grant::empty().with_fact_read(pattern("/facts/node/>")),
    );

    let node = IrohFactNode::memory().await.expect("spawn node");
    let doc = node
        .create_fact_doc(island("prod"))
        .await
        .expect("create fact doc");
    let author = node
        .create_author(writer.principal().clone())
        .await
        .expect("create author");
    let fact_key = key("/facts/node/node-1/joined/1");
    let error = doc
        .write_fact_payload(
            &author,
            fact_key.clone(),
            node_joined_payload("node-1", 1, "fd00::1"),
            &bus,
        )
        .await
        .expect_err("unauthorized write should fail");
    assert!(matches!(error, IrohFactError::UnauthorizedWrite { .. }));

    let source = IrohDocsFactSource::new(node.local_view(), Arc::new(bus.clone()));
    let candidates = source
        .list_candidates(&island("prod"), &pattern("/facts/node/>"), &projection)
        .expect("list candidates");
    assert!(candidates.is_empty());

    node.shutdown().await.expect("shutdown node");
}

#[tokio::test]
async fn docs_source_marks_unknown_author_unverified_and_redacts_payload() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = authority.grant_in(
        island("prod"),
        principal("node-2"),
        Grant::empty().with_fact_write(pattern("/facts/node/>")),
    );
    let projection = authority.grant_in(
        island("prod"),
        principal("projection"),
        Grant::empty().with_fact_read(pattern("/facts/node/>")),
    );

    let node_a = IrohFactNode::memory().await.expect("spawn node a");
    let node_b = IrohFactNode::memory().await.expect("spawn node b");
    let doc_a = node_a
        .create_fact_doc(island("prod"))
        .await
        .expect("create fact doc");
    let ticket = doc_a.share().await.expect("share empty doc");
    let doc_b = node_b.import_fact_doc(ticket).await.expect("import doc");
    let author = node_a
        .create_author(writer.principal().clone())
        .await
        .expect("create unbound author");
    let fact_key = key("/facts/node/node-2/joined/1");
    doc_a
        .write_fact_payload(
            &author,
            fact_key.clone(),
            node_joined_payload("node-2", 1, "fd00::2"),
            &bus,
        )
        .await
        .expect("write unbound author fact");
    doc_b
        .wait_for_key(&fact_key, Duration::from_secs(5))
        .await
        .expect("remote key becomes visible");

    let source = IrohDocsFactSource::new(node_b.local_view(), Arc::new(bus.clone()));
    let candidates = source
        .list_candidates(&island("prod"), &pattern("/facts/node/>"), &projection)
        .expect("list candidates");
    let payloads = source
        .read_payloads(&island("prod"), &candidates, &projection)
        .expect("read payloads");

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].status(), CandidateStatus::Unverified);
    assert!(payloads.is_empty());

    node_a.shutdown().await.expect("shutdown node a");
    node_b.shutdown().await.expect("shutdown node b");
}

#[tokio::test]
async fn malformed_docs_entry_is_reported_without_blocking_valid_facts() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = authority.grant_in(
        island("prod"),
        principal("node-1"),
        Grant::empty().with_fact_write(pattern("/facts/node/>")),
    );
    let projection = authority.grant_in(
        island("prod"),
        principal("projection"),
        Grant::empty().with_fact_read(pattern("/facts/node/>")),
    );

    let node_a = IrohFactNode::memory().await.expect("spawn node a");
    let node_b = IrohFactNode::memory().await.expect("spawn node b");
    let doc_a = node_a
        .create_fact_doc(island("prod"))
        .await
        .expect("create fact doc");
    let author = node_a
        .create_author(writer.principal().clone())
        .await
        .expect("create author");
    doc_a
        .doc
        .set_bytes(author.raw, b"not/a/fact".to_vec(), b"bad".to_vec())
        .await
        .expect("write malformed docs entry");
    let fact_key = key("/facts/node/node-1/joined/1");
    let valid_hash = doc_a
        .write_fact_payload(
            &author,
            fact_key,
            node_joined_payload("node-1", 1, "fd00::1"),
            &bus,
        )
        .await
        .expect("write valid docs fact");
    let ticket = doc_a.share().await.expect("share doc");
    let doc_b = node_b.import_fact_doc(ticket).await.expect("import doc");

    let source = IrohDocsFactSource::new(node_b.local_view(), Arc::new(bus.clone()));
    let started = Instant::now();
    let (rejected, candidates, payloads) = loop {
        doc_b
            .refresh_local_view()
            .await
            .expect("refresh skips malformed entry and applies valid entry");
        let rejected = node_b
            .local_view()
            .rejected_entries()
            .expect("read rejected entries");
        let candidates = source
            .list_candidates(&island("prod"), &pattern("/facts/node/>"), &projection)
            .expect("list candidates");
        let payloads = source
            .read_payloads(&island("prod"), &candidates, &projection)
            .expect("read payloads");
        let malformed_was_reported = rejected.iter().any(|entry| {
            matches!(
                entry.reason(),
                IrohRejectedFactReason::InvalidEntryKey { key, .. } if key == "not/a/fact"
            )
        });
        if malformed_was_reported && payloads.contains_key(&valid_hash) {
            break (rejected, candidates, payloads);
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timed out waiting for malformed entry report and valid payload"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };

    assert!(rejected.iter().any(|entry| {
        matches!(
            entry.reason(),
            IrohRejectedFactReason::InvalidEntryKey { key, .. } if key == "not/a/fact"
        )
    }));
    assert_eq!(candidates.len(), 1);
    assert!(payloads.contains_key(&valid_hash));

    node_a.shutdown().await.expect("shutdown node a");
    node_b.shutdown().await.expect("shutdown node b");
}

#[tokio::test]
async fn same_author_rewrite_replaces_local_candidate() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = authority.grant_in(
        island("prod"),
        principal("node-1"),
        Grant::empty().with_fact_write(pattern("/facts/node/>")),
    );
    let projection = authority.grant_in(
        island("prod"),
        principal("projection"),
        Grant::empty().with_fact_read(pattern("/facts/node/>")),
    );

    let node = IrohFactNode::memory().await.expect("spawn node");
    let doc = node
        .create_fact_doc(island("prod"))
        .await
        .expect("create fact doc");
    let author = node
        .create_author(writer.principal().clone())
        .await
        .expect("create author");
    let fact_key = key("/facts/node/node-1/joined/1");
    let first_hash = doc
        .write_fact_payload(
            &author,
            fact_key.clone(),
            node_joined_payload("node-1", 1, "fd00::1"),
            &bus,
        )
        .await
        .expect("write first candidate");
    let second_hash = doc
        .write_fact_payload(
            &author,
            fact_key,
            node_joined_payload("node-1", 1, "fd00::2"),
            &bus,
        )
        .await
        .expect("rewrite candidate");

    let source = IrohDocsFactSource::new(node.local_view(), Arc::new(bus.clone()));
    let candidates = source
        .list_candidates(&island("prod"), &pattern("/facts/node/>"), &projection)
        .expect("list candidates");
    let payloads = source
        .read_payloads(&island("prod"), &candidates, &projection)
        .expect("read payloads");

    assert_ne!(first_hash, second_hash);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].status(), CandidateStatus::Verified);
    assert_eq!(*candidates[0].content_hash(), second_hash);
    assert_eq!(payloads.len(), 1);
    assert!(payloads.contains_key(&second_hash));

    node.shutdown().await.expect("shutdown node");
}

#[tokio::test]
async fn immutable_write_reports_already_present_for_same_payload() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = authority.grant_in(
        island("prod"),
        principal("node-1"),
        Grant::empty().with_fact_write(pattern("/facts/node/>")),
    );

    let node = IrohFactNode::memory().await.expect("spawn node");
    let doc = node
        .create_fact_doc(island("prod"))
        .await
        .expect("create fact doc");
    let author = node
        .create_author(writer.principal().clone())
        .await
        .expect("create author");
    let fact_key = key("/facts/node/node-1/joined/1");
    let payload = node_joined_payload("node-1", 1, "fd00::1");
    let inserted = doc
        .write_immutable_fact_payload(&author, fact_key.clone(), payload.clone(), &bus)
        .await
        .expect("insert immutable fact");
    let repeated = doc
        .write_immutable_fact_payload(&author, fact_key, payload, &bus)
        .await
        .expect("repeat immutable fact");

    assert!(matches!(inserted, IrohImmutableWriteOutcome::Inserted(_)));
    assert!(matches!(
        repeated,
        IrohImmutableWriteOutcome::AlreadyPresent(_)
    ));

    node.shutdown().await.expect("shutdown node");
}

#[tokio::test]
async fn immutable_write_rejects_same_author_changed_payload_before_overwrite() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = authority.grant_in(
        island("prod"),
        principal("node-1"),
        Grant::empty().with_fact_write(pattern("/facts/node/>")),
    );

    let node = IrohFactNode::memory().await.expect("spawn node");
    let doc = node
        .create_fact_doc(island("prod"))
        .await
        .expect("create fact doc");
    let author = node
        .create_author(writer.principal().clone())
        .await
        .expect("create author");
    let fact_key = key("/facts/node/node-1/joined/1");
    doc.write_immutable_fact_payload(
        &author,
        fact_key.clone(),
        node_joined_payload("node-1", 1, "fd00::1"),
        &bus,
    )
    .await
    .expect("insert immutable fact");
    let conflict = doc
        .write_immutable_fact_payload(
            &author,
            fact_key.clone(),
            node_joined_payload("node-1", 1, "fd00::2"),
            &bus,
        )
        .await
        .expect("changed immutable write reports conflict");

    let source = IrohDocsFactSource::new(node.local_view(), Arc::new(bus.clone()));
    let projection = authority.grant_in(
        island("prod"),
        principal("projection"),
        Grant::empty().with_fact_read(pattern("/facts/node/>")),
    );
    let candidates = source
        .list_candidates(&island("prod"), &pattern("/facts/node/>"), &projection)
        .expect("list candidates");

    assert!(matches!(conflict, IrohImmutableWriteOutcome::Conflict(_)));
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        *candidates[0].content_hash(),
        FactContentHash::for_payload(&node_joined_payload("node-1", 1, "fd00::1"))
    );

    node.shutdown().await.expect("shutdown node");
}

#[tokio::test]
async fn immutable_write_detects_authorized_conflict_without_read_grant() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer_one = authority.grant_in(
        island("prod"),
        principal("node-1"),
        Grant::empty().with_fact_write(pattern("/facts/node/>")),
    );
    let writer_two = authority.grant_in(
        island("prod"),
        principal("node-2"),
        Grant::empty().with_fact_write(pattern("/facts/node/>")),
    );

    let node = IrohFactNode::memory().await.expect("spawn node");
    let doc = node
        .create_fact_doc(island("prod"))
        .await
        .expect("create fact doc");
    let author_one = node
        .create_author(writer_one.principal().clone())
        .await
        .expect("create author one");
    let author_two = node
        .create_author(writer_two.principal().clone())
        .await
        .expect("create author two");
    let fact_key = key("/facts/node/node-1/joined/1");
    doc.write_immutable_fact_payload(
        &author_one,
        fact_key.clone(),
        node_joined_payload("node-1", 1, "fd00::1"),
        &bus,
    )
    .await
    .expect("insert first immutable fact");
    let conflict = doc
        .write_immutable_fact_payload(
            &author_two,
            fact_key,
            node_joined_payload("node-1", 1, "fd00::2"),
            &bus,
        )
        .await
        .expect("second writer sees conflict");

    let IrohImmutableWriteOutcome::Conflict(existing) = conflict else {
        panic!("expected immutable conflict");
    };
    assert_eq!(existing.author(), writer_one.principal());

    node.shutdown().await.expect("shutdown node");
}

#[test]
fn missing_payload_rewrite_replaces_stale_payload() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer = authority.grant_in(
        island("prod"),
        principal("node-1"),
        Grant::empty().with_fact_write(pattern("/facts/node/>")),
    );
    let projection = authority.grant_in(
        island("prod"),
        principal("projection"),
        Grant::empty().with_fact_read(pattern("/facts/node/>")),
    );
    let island = island("prod");
    let fact_key = key("/facts/node/node-1/joined/1");
    let author = writer.principal().clone();
    let first_payload = node_joined_payload("node-1", 1, "fd00::1");
    let first_hash = FactContentHash::for_payload(&first_payload);
    let second_hash = FactContentHash::new("b3:payload-not-ready");
    let local_view = IrohFactLocalView::default();

    local_view
        .upsert(LocalFactEntry {
            metadata: LocalFactMetadata {
                island: island.clone(),
                key: fact_key.clone(),
                author: author.clone(),
                author_verified: true,
                content_hash: first_hash.clone(),
            },
            payload: Some(first_payload),
        })
        .expect("insert first local payload");
    local_view
        .upsert(LocalFactEntry {
            metadata: LocalFactMetadata {
                island: island.clone(),
                key: fact_key,
                author,
                author_verified: true,
                content_hash: second_hash.clone(),
            },
            payload: None,
        })
        .expect("replace with payload-missing metadata");

    let source = IrohDocsFactSource::new(local_view, Arc::new(bus));
    let candidates = source
        .list_candidates(&island, &pattern("/facts/node/>"), &projection)
        .expect("list candidates");
    let payloads = source
        .read_payloads(&island, &candidates, &projection)
        .expect("read payloads");

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].status(), CandidateStatus::Verified);
    assert_eq!(*candidates[0].content_hash(), second_hash);
    assert!(payloads.is_empty());
}

#[tokio::test]
async fn unauthorized_same_key_candidate_does_not_poison_authorized_candidate() {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let trusted = authority.grant_in(
        island("prod"),
        principal("node-trusted"),
        Grant::empty().with_fact_write(pattern("/facts/node/>")),
    );
    let revoked = authority.grant_in(
        island("prod"),
        principal("node-revoked"),
        Grant::empty().with_fact_write(pattern("/facts/node/>")),
    );
    let projection = authority.grant_in(
        island("prod"),
        principal("projection"),
        Grant::empty().with_fact_read(pattern("/facts/node/>")),
    );

    let node = IrohFactNode::memory().await.expect("spawn node");
    let doc = node
        .create_fact_doc(island("prod"))
        .await
        .expect("create fact doc");
    let trusted_author = node
        .create_author(trusted.principal().clone())
        .await
        .expect("create trusted author");
    let revoked_author = node
        .create_author(revoked.principal().clone())
        .await
        .expect("create revoked author");
    let fact_key = key("/facts/node/node-1/joined/1");
    let trusted_hash = doc
        .write_fact_payload(
            &trusted_author,
            fact_key.clone(),
            node_joined_payload("node-1", 1, "fd00::1"),
            &bus,
        )
        .await
        .expect("write trusted candidate");
    doc.write_fact_payload(
        &revoked_author,
        fact_key,
        node_joined_payload("node-1", 1, "fd00::2"),
        &bus,
    )
    .await
    .expect("write soon-revoked candidate");
    assert!(authority.revoke(&revoked));

    let source = IrohDocsFactSource::new(node.local_view(), Arc::new(bus.clone()));
    let candidates = source
        .list_candidates(&island("prod"), &pattern("/facts/node/>"), &projection)
        .expect("list candidates");
    let payloads = source
        .read_payloads(&island("prod"), &candidates, &projection)
        .expect("read payloads");

    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().any(|candidate| {
        candidate.status() == CandidateStatus::Verified && *candidate.content_hash() == trusted_hash
    }));
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.status() == CandidateStatus::Unauthorized)
    );
    assert_eq!(payloads.len(), 1);
    assert!(payloads.contains_key(&trusted_hash));

    node.shutdown().await.expect("shutdown node");
}

#[tokio::test]
async fn wait_for_key_returns_structured_timeout() {
    let node = IrohFactNode::memory().await.expect("spawn node");
    let doc = node
        .create_fact_doc(island("prod"))
        .await
        .expect("create fact doc");
    let timeout = Duration::ZERO;
    let error = doc
        .wait_for_key(&key("/facts/node/missing/joined/1"), timeout)
        .await
        .expect_err("missing key should time out");

    assert!(matches!(
        error,
        IrohFactError::Timeout {
            operation: "wait for fact key",
            timeout: observed
        } if observed == timeout
    ));

    node.shutdown().await.expect("shutdown node");
}
