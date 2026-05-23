#[test]
fn reducer_projects_acme_http01_only_for_current_lease_holder() {
    let mut payloads = BTreeMap::new();
    let id = acme_id();
    let claim = acme_claim(&id, "issuer-a", LeaseEpoch::first());
    let claim_hash = LeaseFact::Claimed(claim.clone()).content_hash();
    let claim_payload = insert_payload(&mut payloads, ProjectionFactPayload::LeaseClaimed(claim));
    let presented = acme_presented(
        id.clone(),
        "issuer-a",
        LeaseEpoch::first(),
        claim_hash,
        "thumbprintA",
    );
    let presented_payload = insert_payload(
        &mut payloads,
        ProjectionFactPayload::AcmeHttp01Presented(presented),
    );
    let candidates = vec![
        candidate_as(
            "issuer-a",
            &id.lease_claimed_fact_key(LeaseEpoch::first()),
            FactKind::LeaseClaimed,
            claim_payload,
        ),
        candidate_as(
            "issuer-a",
            &id.presented_fact_key(LeaseEpoch::first()),
            FactKind::AcmeHttp01Presented,
            presented_payload,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    let challenge = state
        .acme_http01
        .values()
        .next()
        .expect("challenge projects");
    assert_eq!(challenge.holder, LeaseHolder::new("issuer-a"));
    assert_eq!(challenge.claim_hash, claim_hash);
    assert_eq!(challenge.expires_at, LeaseTimestamp::from_secs(160));
}

#[test]
fn reducer_rejects_acme_http01_without_matching_lease() {
    let mut payloads = BTreeMap::new();
    let id = acme_id();
    let claim = acme_claim(&id, "issuer-a", LeaseEpoch::first());
    let claim_hash = LeaseFact::Claimed(claim).content_hash();
    let presented = acme_presented(
        id.clone(),
        "issuer-a",
        LeaseEpoch::first(),
        claim_hash,
        "thumbprintA",
    );
    let presented_payload = insert_payload(
        &mut payloads,
        ProjectionFactPayload::AcmeHttp01Presented(presented),
    );
    let candidates = vec![candidate_as(
        "issuer-a",
        &id.presented_fact_key(LeaseEpoch::first()),
        FactKind::AcmeHttp01Presented,
        presented_payload,
    )];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    assert!(state.acme_http01.is_empty());
    assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 1);
}

#[test]
fn reducer_rejects_acme_http01_written_by_non_holder() {
    let mut payloads = BTreeMap::new();
    let id = acme_id();
    let claim = acme_claim(&id, "issuer-a", LeaseEpoch::first());
    let claim_hash = LeaseFact::Claimed(claim.clone()).content_hash();
    let claim_payload = insert_payload(&mut payloads, ProjectionFactPayload::LeaseClaimed(claim));
    let presented = acme_presented(
        id.clone(),
        "issuer-a",
        LeaseEpoch::first(),
        claim_hash,
        "thumbprintA",
    );
    let presented_payload = insert_payload(
        &mut payloads,
        ProjectionFactPayload::AcmeHttp01Presented(presented),
    );
    let candidates = vec![
        candidate_as(
            "issuer-a",
            &id.lease_claimed_fact_key(LeaseEpoch::first()),
            FactKind::LeaseClaimed,
            claim_payload,
        ),
        candidate_as(
            "issuer-b",
            &id.presented_fact_key(LeaseEpoch::first()),
            FactKind::AcmeHttp01Presented,
            presented_payload,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    assert!(state.acme_http01.is_empty());
    assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 1);
}

#[test]
fn reducer_rejects_acme_fact_key_with_untyped_hostname_or_token() {
    let mut payloads = BTreeMap::new();
    let id = acme_id();
    let claim = acme_claim(&id, "issuer-a", LeaseEpoch::first());
    let claim_hash = LeaseFact::Claimed(claim).content_hash();
    let presented_payload = insert_payload(
        &mut payloads,
        ProjectionFactPayload::AcmeHttp01Presented(acme_presented(
            id,
            "issuer-a",
            LeaseEpoch::first(),
            claim_hash,
            "thumbprintA",
        )),
    );
    let candidates = vec![candidate_as(
        "issuer-a",
        "/facts/acme/http01/bad name.example/short/presented/1",
        FactKind::AcmeHttp01Presented,
        presented_payload,
    )];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    assert!(state.acme_http01.is_empty());
    assert_eq!(
        status_count(&state, ProjectionIgnoreReason::MalformedPayload),
        1
    );
}

#[test]
fn reducer_ignores_forged_lease_claim_author_without_poisoning_valid_claim() {
    let mut payloads = BTreeMap::new();
    let id = acme_id();
    let valid_claim = acme_claim(&id, "issuer-a", LeaseEpoch::first());
    let valid_claim_hash = LeaseFact::Claimed(valid_claim.clone()).content_hash();
    let forged_epoch = LeaseEpoch::from_u64(2).expect("forged epoch");
    let forged_claim = acme_claim(&id, "issuer-b", forged_epoch);
    let valid_claim_payload = insert_payload(
        &mut payloads,
        ProjectionFactPayload::LeaseClaimed(valid_claim),
    );
    let forged_claim_payload = insert_payload(
        &mut payloads,
        ProjectionFactPayload::LeaseClaimed(forged_claim),
    );
    let presented_payload = insert_payload(
        &mut payloads,
        ProjectionFactPayload::AcmeHttp01Presented(acme_presented(
            id.clone(),
            "issuer-a",
            LeaseEpoch::first(),
            valid_claim_hash,
            "thumbprintA",
        )),
    );
    let candidates = vec![
        candidate_as(
            "issuer-a",
            &id.lease_claimed_fact_key(LeaseEpoch::first()),
            FactKind::LeaseClaimed,
            valid_claim_payload,
        ),
        candidate_as(
            "issuer-a",
            &id.lease_claimed_fact_key(forged_epoch),
            FactKind::LeaseClaimed,
            forged_claim_payload,
        ),
        candidate_as(
            "issuer-a",
            &id.presented_fact_key(LeaseEpoch::first()),
            FactKind::AcmeHttp01Presented,
            presented_payload,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    let challenge = state
        .acme_http01
        .values()
        .next()
        .expect("valid holder still projects");
    assert_eq!(challenge.holder, LeaseHolder::new("issuer-a"));
}

#[test]
fn reducer_picks_acme_same_epoch_winner_by_content_hash() {
    let mut payloads = BTreeMap::new();
    let id = acme_id();
    let claim = acme_claim(&id, "issuer-a", LeaseEpoch::first());
    let claim_hash = LeaseFact::Claimed(claim.clone()).content_hash();
    let claim_payload = insert_payload(&mut payloads, ProjectionFactPayload::LeaseClaimed(claim));
    let presented_a = acme_presented(
        id.clone(),
        "issuer-a",
        LeaseEpoch::first(),
        claim_hash,
        "thumbprintA",
    );
    let presented_b = acme_presented(
        id.clone(),
        "issuer-a",
        LeaseEpoch::first(),
        claim_hash,
        "thumbprintB",
    );
    let hash_a = insert_payload(
        &mut payloads,
        ProjectionFactPayload::AcmeHttp01Presented(presented_a),
    );
    let hash_b = insert_payload(
        &mut payloads,
        ProjectionFactPayload::AcmeHttp01Presented(presented_b),
    );
    let expected = if hash_a <= hash_b {
        "tokAcme0123456789abcdef.thumbprintA"
    } else {
        "tokAcme0123456789abcdef.thumbprintB"
    };
    let ordered = vec![
        candidate_as(
            "issuer-a",
            &id.lease_claimed_fact_key(LeaseEpoch::first()),
            FactKind::LeaseClaimed,
            claim_payload.clone(),
        ),
        candidate_as(
            "issuer-a",
            &id.presented_fact_key(LeaseEpoch::first()),
            FactKind::AcmeHttp01Presented,
            hash_a.clone(),
        ),
        candidate_as(
            "issuer-a",
            &id.presented_fact_key(LeaseEpoch::first()),
            FactKind::AcmeHttp01Presented,
            hash_b.clone(),
        ),
    ];
    let shuffled = vec![
        candidate_as(
            "issuer-a",
            &id.presented_fact_key(LeaseEpoch::first()),
            FactKind::AcmeHttp01Presented,
            hash_b,
        ),
        candidate_as(
            "issuer-a",
            &id.lease_claimed_fact_key(LeaseEpoch::first()),
            FactKind::LeaseClaimed,
            claim_payload,
        ),
        candidate_as(
            "issuer-a",
            &id.presented_fact_key(LeaseEpoch::first()),
            FactKind::AcmeHttp01Presented,
            hash_a,
        ),
    ];

    let ordered_state = reduce_facts(&island("prod"), &ordered, &payloads);
    let shuffled_state = reduce_facts(&island("prod"), &shuffled, &payloads);

    assert_eq!(ordered_state, shuffled_state);
    assert_eq!(
        ordered_state
            .acme_http01
            .values()
            .next()
            .expect("challenge")
            .key_authorization
            .as_str(),
        expected
    );
}

#[test]
fn reducer_only_matching_acme_clear_removes_presentation() {
    let mut payloads = BTreeMap::new();
    let id = acme_id();
    let epoch = LeaseEpoch::from_u64(2).expect("epoch");
    let claim = acme_claim(&id, "issuer-a", epoch);
    let claim_hash = LeaseFact::Claimed(claim.clone()).content_hash();
    let other_claim_hash = LeaseFact::Claimed(acme_claim(&id, "issuer-b", epoch)).content_hash();
    let claim_payload = insert_payload(&mut payloads, ProjectionFactPayload::LeaseClaimed(claim));
    let presented_payload = insert_payload(
        &mut payloads,
        ProjectionFactPayload::AcmeHttp01Presented(acme_presented(
            id.clone(),
            "issuer-a",
            epoch,
            claim_hash,
            "thumbprintA",
        )),
    );
    let stale_clear = insert_payload(
        &mut payloads,
        ProjectionFactPayload::AcmeHttp01Cleared(acme_cleared(
            id.clone(),
            "issuer-a",
            LeaseEpoch::first(),
            claim_hash,
            LeaseTimestamp::from_secs(120),
        )),
    );
    let non_matching_high_clear = insert_payload(
        &mut payloads,
        ProjectionFactPayload::AcmeHttp01Cleared(acme_cleared(
            id.clone(),
            "issuer-b",
            LeaseEpoch::from_u64(3).expect("epoch"),
            other_claim_hash,
            LeaseTimestamp::from_secs(120),
        )),
    );
    let matching_clear = insert_payload(
        &mut payloads,
        ProjectionFactPayload::AcmeHttp01Cleared(acme_cleared(
            id.clone(),
            "issuer-a",
            epoch,
            claim_hash,
            LeaseTimestamp::from_secs(120),
        )),
    );

    let without_match = reduce_facts(
        &island("prod"),
        &[
            candidate_as(
                "issuer-a",
                &id.lease_claimed_fact_key(epoch),
                FactKind::LeaseClaimed,
                claim_payload.clone(),
            ),
            candidate_as(
                "issuer-a",
                &id.presented_fact_key(epoch),
                FactKind::AcmeHttp01Presented,
                presented_payload.clone(),
            ),
            candidate_as(
                "issuer-a",
                &id.cleared_fact_key(LeaseEpoch::first(), claim_hash),
                FactKind::AcmeHttp01Cleared,
                stale_clear,
            ),
            candidate_as(
                "issuer-b",
                &id.cleared_fact_key(LeaseEpoch::from_u64(3).expect("epoch"), other_claim_hash),
                FactKind::AcmeHttp01Cleared,
                non_matching_high_clear,
            ),
        ],
        &payloads,
    );
    let with_match = reduce_facts(
        &island("prod"),
        &[
            candidate_as(
                "issuer-a",
                &id.lease_claimed_fact_key(epoch),
                FactKind::LeaseClaimed,
                claim_payload,
            ),
            candidate_as(
                "issuer-a",
                &id.presented_fact_key(epoch),
                FactKind::AcmeHttp01Presented,
                presented_payload,
            ),
            candidate_as(
                "issuer-a",
                &id.cleared_fact_key(epoch, claim_hash),
                FactKind::AcmeHttp01Cleared,
                matching_clear,
            ),
        ],
        &payloads,
    );

    assert_eq!(without_match.acme_http01.len(), 1);
    assert!(with_match.acme_http01.is_empty());
}

#[test]
fn reducer_rejects_payload_identity_that_does_not_match_fact_key() {
    let mut payloads = BTreeMap::new();
    let node_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-2"),
            epoch: 1,
            overlay_ip: "fd00::2".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    );
    let service_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
            service: ServiceName::new("queue"),
            node_id: NodeId::new("node-1"),
            version: "1.0.0".to_string(),
            endpoint_subject: "node.node-1.queue".to_string(),
            epoch: 1,
        }),
    );
    let route_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::RouteCommit(RouteCommitFact {
            route_commit_id: "route-2".to_string(),
            route_id: RouteId::new("web-http"),
            hostnames: Vec::new(),
            backends: Vec::new(),
            old_backends_to_drain: Vec::new(),
        }),
    );
    let lease_resource = LeaseResource::from_segments(["acme", "http01", "example", "token"]);
    let claim_hash = LeaseFact::Claimed(LeaseClaimed::new(
        lease_resource.clone(),
        LeaseHolder::new("issuer-a"),
        LeaseEpoch::first(),
        LeaseTimestamp::from_secs(100),
        LeaseTimestamp::from_secs(160),
    ))
    .content_hash();
    let other_claim_hash = LeaseFact::Claimed(LeaseClaimed::new(
        lease_resource.clone(),
        LeaseHolder::new("issuer-b"),
        LeaseEpoch::first(),
        LeaseTimestamp::from_secs(100),
        LeaseTimestamp::from_secs(160),
    ))
    .content_hash();
    let renewed_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::LeaseRenewed(LeaseRenewed::new(
            lease_resource.clone(),
            LeaseHolder::new("issuer-a"),
            LeaseEpoch::first(),
            claim_hash,
            LeaseTimestamp::from_secs(120),
            LeaseTimestamp::from_secs(180),
        )),
    );
    let released_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::LeaseReleased(LeaseReleased::new_at(
            lease_resource.clone(),
            LeaseHolder::new("issuer-a"),
            LeaseEpoch::first(),
            claim_hash,
            LeaseTimestamp::from_secs(130),
        )),
    );
    let candidates = vec![
        candidate(
            "/facts/node/node-1/joined/1",
            FactKind::NodeJoined,
            node_hash,
        ),
        candidate(
            "/facts/service/web/node-1/registered/1",
            FactKind::ServiceRegistered,
            service_hash,
        ),
        candidate("/facts/routes/route-1", FactKind::RouteCommit, route_hash),
        candidate(
            &format!(
                "/facts/lease/{}/renewed/1/{}/120",
                lease_resource, other_claim_hash
            ),
            FactKind::LeaseRenewed,
            renewed_hash,
        ),
        candidate(
            &format!(
                "/facts/lease/{}/released/1/{}/999",
                lease_resource, claim_hash
            ),
            FactKind::LeaseReleased,
            released_hash,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    assert!(state.nodes.is_empty());
    assert!(state.services.is_empty());
    assert!(state.gateway.is_none());
    assert_eq!(
        status_count(&state, ProjectionIgnoreReason::MalformedPayload),
        5
    );
}

#[test]
fn reducer_reports_ignored_candidates_without_raw_metadata() {
    let mut payloads = BTreeMap::new();
    let hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            overlay_ip: "fd00::1".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    );
    let candidates = vec![
        FactCandidate::new(
            island("laptop"),
            key("/facts/node/secret/joined/1"),
            principal("laptop-admin"),
            hash.clone(),
            FactKind::NodeJoined,
            0,
            CandidateStatus::Verified,
        ),
        FactCandidate::new(
            island("prod"),
            key("/facts/node/node-2/joined/1"),
            principal("intruder"),
            hash,
            FactKind::NodeJoined,
            0,
            CandidateStatus::Unauthorized,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    assert!(state.nodes.is_empty());
    assert_eq!(state.statuses.len(), 2);
    assert!(state.statuses.iter().any(|status| {
        status.reason == ProjectionIgnoreReason::CrossIsland && status.count == 1
    }));
    assert!(state.statuses.iter().any(|status| {
        status.reason == ProjectionIgnoreReason::Unauthorized && status.count == 1
    }));
}

#[test]
fn reducer_reports_malformed_payloads() {
    let mut payloads = BTreeMap::new();
    let payload: FactPayload = b"not-json".to_vec().into();
    let hash = FactContentHash::for_payload(&payload);
    payloads.insert(hash.clone(), payload);
    let candidates = vec![candidate(
        "/facts/node/node-1/joined/1",
        FactKind::NodeJoined,
        hash,
    )];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    assert_eq!(
        state.statuses,
        vec![crate::model::ProjectionStatus {
            reason: ProjectionIgnoreReason::MalformedPayload,
            count: 1,
        }]
    );
}
