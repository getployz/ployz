use super::reduce_facts;
use crate::facts::{
    BackendEndpoint, DnsCommitFact, DnsRecordFact, GatewayCommitFact, NodeJoinedFact,
    NodeRemovalStartedFact, NodeTombstonedFact, ProjectionFactPayload, RouteCommitFact, RouteId,
    ServiceName, ServiceRegistrationFact, ServingCommitFact,
};
use crate::model::{ProjectionIgnoreReason, ProjectionState};
use crate::source::{CandidateStatus, FactCandidate, FactKind};
use mvp_acme::{
    AcmeCertificateActivatedFact, AcmeChallengeId, AcmeChallengeToken, AcmeHostname,
    AcmeHttp01ClearedFact, AcmeHttp01PresentedFact, AcmeKeyAuthorization, AcmeOrderUrl,
};
use mvp_bus::{FactContentHash, FactKey, FactPayload, IslandId, PrincipalId};
use mvp_identity::NodeId;
use mvp_lease::{
    LeaseClaimed, LeaseEpoch, LeaseFact, LeaseHolder, LeaseReleased, LeaseRenewed, LeaseResource,
    LeaseTimestamp,
};
use std::collections::BTreeMap;

fn island(value: &str) -> IslandId {
    IslandId::new(value)
}

fn principal(value: &str) -> PrincipalId {
    PrincipalId::new(value)
}

fn key(value: &str) -> FactKey {
    FactKey::parse(value).expect("fact key parses")
}

fn insert_payload(
    payloads: &mut BTreeMap<FactContentHash, FactPayload>,
    payload: ProjectionFactPayload,
) -> FactContentHash {
    let bytes = payload.to_fact_bytes().expect("payload serializes");
    let fact_payload: FactPayload = bytes.into();
    let hash = FactContentHash::for_payload(&fact_payload);
    payloads.insert(hash.clone(), fact_payload);
    hash
}

fn candidate(key_value: &str, kind: FactKind, hash: FactContentHash) -> FactCandidate {
    candidate_as("writer", key_value, kind, hash)
}

fn candidate_as(
    author: &str,
    key_value: &str,
    kind: FactKind,
    hash: FactContentHash,
) -> FactCandidate {
    FactCandidate::verified(
        island("prod"),
        key(key_value),
        principal(author),
        hash,
        kind,
        0,
    )
}

fn status_count(state: &ProjectionState, reason: ProjectionIgnoreReason) -> usize {
    state
        .statuses
        .iter()
        .find(|status| status.reason == reason)
        .map_or(0, |status| status.count)
}

fn acme_id() -> AcmeChallengeId {
    AcmeChallengeId::new(
        AcmeHostname::parse("example.test").expect("hostname"),
        AcmeChallengeToken::parse("tokAcme0123456789abcdef").expect("token"),
    )
}

fn acme_claim(id: &AcmeChallengeId, holder: &str, epoch: LeaseEpoch) -> LeaseClaimed {
    LeaseClaimed::new(
        id.lease_resource().clone(),
        LeaseHolder::new(holder),
        epoch,
        LeaseTimestamp::from_secs(100),
        LeaseTimestamp::from_secs(160),
    )
}

fn acme_presented(
    id: AcmeChallengeId,
    holder: &str,
    epoch: LeaseEpoch,
    claim_hash: mvp_lease::LeaseContentHash,
    thumbprint: &str,
) -> AcmeHttp01PresentedFact {
    let key_authorization = AcmeKeyAuthorization::parse_for_token(
        id.token(),
        format!("{}.{thumbprint}", id.token().as_str()),
    )
    .expect("key authorization");
    AcmeHttp01PresentedFact::from_parts(
        id,
        key_authorization,
        LeaseHolder::new(holder),
        epoch,
        claim_hash,
        LeaseTimestamp::from_secs(110),
    )
    .expect("presented fact")
}

fn acme_cleared(
    id: AcmeChallengeId,
    holder: &str,
    epoch: LeaseEpoch,
    claim_hash: mvp_lease::LeaseContentHash,
    cleared_at: LeaseTimestamp,
) -> AcmeHttp01ClearedFact {
    AcmeHttp01ClearedFact::from_parts(id, LeaseHolder::new(holder), epoch, claim_hash, cleared_at)
}

#[test]
fn reducer_projects_latest_acme_certificate_by_hostname() {
    let mut payloads = BTreeMap::new();
    let old = AcmeCertificateActivatedFact {
        hostname: AcmeHostname::parse("Example.test").expect("hostname"),
        order_url: AcmeOrderUrl::parse("https://pebble.test/order/old").expect("order url"),
        fullchain_pem: "old-chain".to_string(),
        private_key_pem: "old-key".to_string(),
        issued_at_secs: 10,
        not_before_secs: Some(9),
        not_after_secs: Some(90),
    };
    let new = AcmeCertificateActivatedFact {
        hostname: AcmeHostname::parse("example.test").expect("hostname"),
        order_url: AcmeOrderUrl::parse("https://pebble.test/order/new").expect("order url"),
        fullchain_pem: "new-chain".to_string(),
        private_key_pem: "new-key".to_string(),
        issued_at_secs: 20,
        not_before_secs: Some(19),
        not_after_secs: Some(190),
    };
    let old_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::AcmeCertificateActivated(old),
    );
    let new_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::AcmeCertificateActivated(new),
    );
    let candidates = vec![
        candidate(
            "/facts/acme/certificate/example.test/activated/10",
            FactKind::AcmeCertificateActivated,
            old_hash,
        ),
        candidate(
            "/facts/acme/certificate/example.test/activated/20",
            FactKind::AcmeCertificateActivated,
            new_hash,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    let certificate = state
        .certificates
        .get(&AcmeHostname::parse("example.test").expect("hostname"))
        .expect("certificate projects");
    assert_eq!(certificate.fullchain_pem, "new-chain");
    assert_eq!(certificate.private_key_pem, "new-key");
    assert_eq!(
        status_count(&state, ProjectionIgnoreReason::Superseded),
        1
    );
}

#[test]
fn reducer_projects_node_service_gateway_and_dns_state() {
    let mut payloads = BTreeMap::new();
    let node_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            overlay_ip: "fd00::1".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    );
    let service_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
            service: ServiceName::new("web"),
            node_id: NodeId::new("node-1"),
            version: "1.0.0".to_string(),
            endpoint_subject: "node.node-1.rpc.inspect".to_string(),
            epoch: 1,
        }),
    );
    let route_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::RouteCommit(RouteCommitFact {
            route_commit_id: "route-1".to_string(),
            route_id: RouteId::new("web-http"),
            hostnames: vec!["b.example.com".to_string(), "a.example.com".to_string()],
            backends: vec![BackendEndpoint {
                node_id: NodeId::new("node-1"),
                address: "10.0.0.1:8080".to_string(),
            }],
            old_backends_to_drain: Vec::new(),
        }),
    );
    let gateway_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
            gateway_commit_id: "gateway-1".to_string(),
            route_commit_id: "route-1".to_string(),
            epoch: 1,
        }),
    );
    let dns_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::DnsCommit(DnsCommitFact {
            dns_commit_id: "dns-1".to_string(),
            epoch: 1,
            records: vec![DnsRecordFact {
                name: "web.example.com".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::1".to_string(),
                ttl_seconds: 30,
            }],
        }),
    );
    let candidates = vec![
        candidate("/facts/dns/dns-1", FactKind::DnsCommit, dns_hash),
        candidate(
            "/facts/service/web/node-1/registered/1",
            FactKind::ServiceRegistered,
            service_hash,
        ),
        candidate("/facts/routes/route-1", FactKind::RouteCommit, route_hash),
        candidate(
            "/facts/gateway/gateway-1",
            FactKind::GatewayCommit,
            gateway_hash,
        ),
        candidate(
            "/facts/node/node-1/joined/1",
            FactKind::NodeJoined,
            node_hash,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    assert_eq!(state.nodes.len(), 1);
    assert_eq!(state.services.len(), 1);
    assert_eq!(
        state.gateway.as_ref().expect("gateway projects").routes[0].hostnames,
        vec!["a.example.com", "b.example.com"]
    );
    assert_eq!(
        state.dns.as_ref().expect("dns projects").records[0].value,
        "fd00::1"
    );
    assert!(state.statuses.is_empty());
}

#[test]
fn reducer_is_deterministic_for_shuffled_candidates() {
    let mut payloads = BTreeMap::new();
    let one = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            overlay_ip: "fd00::1".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    );
    let two = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-2"),
            epoch: 1,
            overlay_ip: "fd00::2".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    );
    let ordered = vec![
        candidate(
            "/facts/node/node-1/joined/1",
            FactKind::NodeJoined,
            one.clone(),
        ),
        candidate(
            "/facts/node/node-2/joined/1",
            FactKind::NodeJoined,
            two.clone(),
        ),
    ];
    let shuffled = vec![
        candidate("/facts/node/node-2/joined/1", FactKind::NodeJoined, two),
        candidate("/facts/node/node-1/joined/1", FactKind::NodeJoined, one),
    ];

    assert_eq!(
        reduce_facts(&island("prod"), &ordered, &payloads),
        reduce_facts(&island("prod"), &shuffled, &payloads)
    );
}

#[test]
fn reducer_uses_numeric_epochs_for_node_and_service_heads() {
    let mut payloads = BTreeMap::new();
    let node_10 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 10,
            overlay_ip: "fd00::10".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    );
    let node_9 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 9,
            overlay_ip: "fd00::9".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    );
    let service_10 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
            service: ServiceName::new("web"),
            node_id: NodeId::new("node-1"),
            version: "10.0.0".to_string(),
            endpoint_subject: "node.node-1.web.v10".to_string(),
            epoch: 10,
        }),
    );
    let service_9 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
            service: ServiceName::new("web"),
            node_id: NodeId::new("node-1"),
            version: "9.0.0".to_string(),
            endpoint_subject: "node.node-1.web.v9".to_string(),
            epoch: 9,
        }),
    );
    let candidates = vec![
        candidate("/facts/node/node-1/joined/9", FactKind::NodeJoined, node_9),
        candidate(
            "/facts/node/node-1/joined/10",
            FactKind::NodeJoined,
            node_10,
        ),
        candidate(
            "/facts/service/web/node-1/registered/9",
            FactKind::ServiceRegistered,
            service_9,
        ),
        candidate(
            "/facts/service/web/node-1/registered/10",
            FactKind::ServiceRegistered,
            service_10,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);
    let node = state.nodes.get(&NodeId::new("node-1")).expect("node");
    let service = state
        .services
        .get(&(ServiceName::new("web"), NodeId::new("node-1")))
        .expect("service");

    assert_eq!(node.epoch, 10);
    assert_eq!(node.overlay_ip, "fd00::10");
    assert_eq!(service.epoch, 10);
    assert_eq!(service.version, "10.0.0");
}

#[test]
fn reducer_excludes_tombstoned_nodes_and_services() {
    let mut payloads = BTreeMap::new();
    let node_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            overlay_ip: "fd00::1".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    );
    let service_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
            service: ServiceName::new("web"),
            node_id: NodeId::new("node-1"),
            version: "1.0.0".to_string(),
            endpoint_subject: "node.node-1.web".to_string(),
            epoch: 1,
        }),
    );
    let tombstone_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeTombstoned(NodeTombstonedFact {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            reason: "force-remove".to_string(),
        }),
    );
    let candidates = vec![
        candidate(
            "/facts/node/node-1/joined/1",
            FactKind::NodeJoined,
            node_hash,
        ),
        candidate(
            "/facts/node/node-1/tombstoned/1",
            FactKind::NodeTombstoned,
            tombstone_hash,
        ),
        candidate(
            "/facts/service/web/node-1/registered/1",
            FactKind::ServiceRegistered,
            service_hash,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    assert!(state.nodes.is_empty());
    assert_eq!(state.tombstoned_nodes.get(&NodeId::new("node-1")), Some(&1));
    assert!(state.services.is_empty());
    assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 2);
}

#[test]
fn reducer_projects_removal_started_without_removing_live_state() {
    let mut payloads = BTreeMap::new();
    let node_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            overlay_ip: "fd00::1".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    );
    let service_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
            service: ServiceName::new("web"),
            node_id: NodeId::new("node-1"),
            version: "1.0.0".to_string(),
            endpoint_subject: "node.node-1.web".to_string(),
            epoch: 1,
        }),
    );
    let removal_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeRemovalStarted(NodeRemovalStartedFact {
            node_id: NodeId::new("node-1"),
            epoch: 2,
            reason: "graceful-remove".to_string(),
        }),
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
        candidate(
            "/facts/node/node-1/removal_started/2",
            FactKind::NodeRemovalStarted,
            removal_hash,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    assert!(state.nodes.contains_key(&NodeId::new("node-1")));
    assert!(
        state
            .services
            .contains_key(&(ServiceName::new("web"), NodeId::new("node-1")))
    );
    let removing = state
        .removing_nodes
        .get(&NodeId::new("node-1"))
        .expect("node marked removing");
    assert_eq!(removing.epoch, 2);
    assert_eq!(removing.reason, "graceful-remove");
}

#[test]
fn reducer_tombstone_supersedes_removal_started() {
    let mut payloads = BTreeMap::new();
    let node_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            overlay_ip: "fd00::1".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    );
    let removal_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeRemovalStarted(NodeRemovalStartedFact {
            node_id: NodeId::new("node-1"),
            epoch: 2,
            reason: "graceful-remove".to_string(),
        }),
    );
    let tombstone_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeTombstoned(NodeTombstonedFact {
            node_id: NodeId::new("node-1"),
            epoch: 3,
            reason: "removed".to_string(),
        }),
    );
    let candidates = vec![
        candidate(
            "/facts/node/node-1/joined/1",
            FactKind::NodeJoined,
            node_hash,
        ),
        candidate(
            "/facts/node/node-1/removal_started/2",
            FactKind::NodeRemovalStarted,
            removal_hash,
        ),
        candidate(
            "/facts/node/node-1/tombstoned/3",
            FactKind::NodeTombstoned,
            tombstone_hash,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    assert!(state.nodes.is_empty());
    assert!(state.removing_nodes.is_empty());
    assert_eq!(state.tombstoned_nodes.get(&NodeId::new("node-1")), Some(&3));
    assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 2);
}

#[test]
fn reducer_selects_removal_started_candidate_deterministically() {
    let mut payloads = BTreeMap::new();
    let first_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeRemovalStarted(NodeRemovalStartedFact {
            node_id: NodeId::new("node-1"),
            epoch: 2,
            reason: "first".to_string(),
        }),
    );
    let second_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeRemovalStarted(NodeRemovalStartedFact {
            node_id: NodeId::new("node-1"),
            epoch: 2,
            reason: "second".to_string(),
        }),
    );
    let expected_reason = if first_hash <= second_hash {
        "first"
    } else {
        "second"
    };
    let candidates = vec![
        candidate(
            "/facts/node/node-1/removal_started/2/a",
            FactKind::NodeRemovalStarted,
            first_hash,
        ),
        candidate(
            "/facts/node/node-1/removal_started/2/b",
            FactKind::NodeRemovalStarted,
            second_hash,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    let removing = state
        .removing_nodes
        .get(&NodeId::new("node-1"))
        .expect("node marked removing");
    assert_eq!(removing.epoch, 2);
    assert_eq!(removing.reason, expected_reason);
    assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 1);
}

#[test]
fn reducer_excludes_tombstoned_node_services_even_with_newer_service_epoch() {
    let mut payloads = BTreeMap::new();
    let node_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            overlay_ip: "fd00::1".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    );
    let service_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
            service: ServiceName::new("web"),
            node_id: NodeId::new("node-1"),
            version: "3.0.0".to_string(),
            endpoint_subject: "node.node-1.web".to_string(),
            epoch: 3,
        }),
    );
    let tombstone_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeTombstoned(NodeTombstonedFact {
            node_id: NodeId::new("node-1"),
            epoch: 2,
            reason: "force-remove".to_string(),
        }),
    );
    let candidates = vec![
        candidate(
            "/facts/node/node-1/joined/1",
            FactKind::NodeJoined,
            node_hash,
        ),
        candidate(
            "/facts/service/web/node-1/registered/3",
            FactKind::ServiceRegistered,
            service_hash,
        ),
        candidate(
            "/facts/node/node-1/tombstoned/2",
            FactKind::NodeTombstoned,
            tombstone_hash,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    assert!(state.nodes.is_empty());
    assert_eq!(state.tombstoned_nodes.get(&NodeId::new("node-1")), Some(&2));
    assert!(state.services.is_empty());
    assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 2);
}

#[test]
fn reducer_keeps_tombstone_until_explicit_reinvite_exists() {
    let mut payloads = BTreeMap::new();
    let node_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 2,
            overlay_ip: "fd00::1".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    );
    let tombstone_hash = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeTombstoned(NodeTombstonedFact {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            reason: "old-remove".to_string(),
        }),
    );
    let candidates = vec![
        candidate(
            "/facts/node/node-1/joined/2",
            FactKind::NodeJoined,
            node_hash,
        ),
        candidate(
            "/facts/node/node-1/tombstoned/1",
            FactKind::NodeTombstoned,
            tombstone_hash,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    assert!(state.nodes.is_empty());
    assert_eq!(state.tombstoned_nodes.get(&NodeId::new("node-1")), Some(&1));
    assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 1);
}

#[test]
fn reducer_uses_commit_epochs_for_gateway_and_dns_heads() {
    let mut payloads = BTreeMap::new();
    let route_9 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::RouteCommit(RouteCommitFact {
            route_commit_id: "route-9".to_string(),
            route_id: RouteId::new("web-http"),
            hostnames: vec!["old.example.com".to_string()],
            backends: Vec::new(),
            old_backends_to_drain: Vec::new(),
        }),
    );
    let route_10 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::RouteCommit(RouteCommitFact {
            route_commit_id: "route-10".to_string(),
            route_id: RouteId::new("web-http"),
            hostnames: vec!["new.example.com".to_string()],
            backends: Vec::new(),
            old_backends_to_drain: Vec::new(),
        }),
    );
    let gateway_9 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
            gateway_commit_id: "gateway-9".to_string(),
            route_commit_id: "route-9".to_string(),
            epoch: 9,
        }),
    );
    let gateway_10 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
            gateway_commit_id: "gateway-10".to_string(),
            route_commit_id: "route-10".to_string(),
            epoch: 10,
        }),
    );
    let dns_9 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::DnsCommit(DnsCommitFact {
            dns_commit_id: "dns-9".to_string(),
            epoch: 9,
            records: vec![DnsRecordFact {
                name: "web.example.com".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::9".to_string(),
                ttl_seconds: 30,
            }],
        }),
    );
    let dns_10 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::DnsCommit(DnsCommitFact {
            dns_commit_id: "dns-10".to_string(),
            epoch: 10,
            records: vec![DnsRecordFact {
                name: "web.example.com".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::10".to_string(),
                ttl_seconds: 30,
            }],
        }),
    );
    let candidates = vec![
        candidate("/facts/routes/route-9", FactKind::RouteCommit, route_9),
        candidate("/facts/routes/route-10", FactKind::RouteCommit, route_10),
        candidate(
            "/facts/gateway/gateway-9",
            FactKind::GatewayCommit,
            gateway_9,
        ),
        candidate(
            "/facts/gateway/gateway-10",
            FactKind::GatewayCommit,
            gateway_10,
        ),
        candidate("/facts/dns/dns-9", FactKind::DnsCommit, dns_9),
        candidate("/facts/dns/dns-10", FactKind::DnsCommit, dns_10),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    assert_eq!(
        state.gateway.expect("gateway").gateway_commit_id,
        "gateway-10"
    );
    assert_eq!(state.dns.expect("dns").dns_commit_id, "dns-10");
}

#[test]
fn reducer_marks_same_epoch_identity_conflicts_instead_of_picking_by_sort_order() {
    let mut payloads = BTreeMap::new();
    let node_a = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            overlay_ip: "fd00::1".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    );
    let node_b = insert_payload(
        &mut payloads,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            overlay_ip: "fd00::2".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    );
    let service_a = insert_payload(
        &mut payloads,
        ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
            service: ServiceName::new("web"),
            node_id: NodeId::new("node-1"),
            version: "1.0.0".to_string(),
            endpoint_subject: "node.node-1.web.a".to_string(),
            epoch: 1,
        }),
    );
    let service_b = insert_payload(
        &mut payloads,
        ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
            service: ServiceName::new("web"),
            node_id: NodeId::new("node-1"),
            version: "1.0.1".to_string(),
            endpoint_subject: "node.node-1.web.b".to_string(),
            epoch: 1,
        }),
    );
    let candidates = vec![
        candidate(
            "/facts/node/node-1/joined/1/a",
            FactKind::NodeJoined,
            node_a,
        ),
        candidate(
            "/facts/node/node-1/joined/1/b",
            FactKind::NodeJoined,
            node_b,
        ),
        candidate(
            "/facts/service/web/node-1/registered/1/a",
            FactKind::ServiceRegistered,
            service_a,
        ),
        candidate(
            "/facts/service/web/node-1/registered/1/b",
            FactKind::ServiceRegistered,
            service_b,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    assert!(state.nodes.is_empty());
    assert!(state.services.is_empty());
    assert_eq!(status_count(&state, ProjectionIgnoreReason::Conflict), 2);
}

#[test]
fn reducer_supersedes_same_epoch_gateway_and_dns_heads_deterministically() {
    let mut payloads = BTreeMap::new();
    let route_1 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::RouteCommit(RouteCommitFact {
            route_commit_id: "route-1".to_string(),
            route_id: RouteId::new("web-http"),
            hostnames: vec!["one.example.com".to_string()],
            backends: Vec::new(),
            old_backends_to_drain: Vec::new(),
        }),
    );
    let route_2 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::RouteCommit(RouteCommitFact {
            route_commit_id: "route-2".to_string(),
            route_id: RouteId::new("web-http"),
            hostnames: vec!["two.example.com".to_string()],
            backends: Vec::new(),
            old_backends_to_drain: Vec::new(),
        }),
    );
    let gateway_1 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
            gateway_commit_id: "gateway-1".to_string(),
            route_commit_id: "route-1".to_string(),
            epoch: 1,
        }),
    );
    let gateway_2 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
            gateway_commit_id: "gateway-2".to_string(),
            route_commit_id: "route-2".to_string(),
            epoch: 1,
        }),
    );
    let dns_1 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::DnsCommit(DnsCommitFact {
            dns_commit_id: "dns-1".to_string(),
            epoch: 1,
            records: Vec::new(),
        }),
    );
    let dns_2 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::DnsCommit(DnsCommitFact {
            dns_commit_id: "dns-2".to_string(),
            epoch: 1,
            records: Vec::new(),
        }),
    );
    let expected_gateway = if gateway_1 <= gateway_2 {
        ("gateway-1", "one.example.com")
    } else {
        ("gateway-2", "two.example.com")
    };
    let expected_dns = if dns_1 <= dns_2 { "dns-1" } else { "dns-2" };
    let candidates = vec![
        candidate("/facts/routes/route-1", FactKind::RouteCommit, route_1),
        candidate("/facts/routes/route-2", FactKind::RouteCommit, route_2),
        candidate(
            "/facts/gateway/gateway-1",
            FactKind::GatewayCommit,
            gateway_1,
        ),
        candidate(
            "/facts/gateway/gateway-2",
            FactKind::GatewayCommit,
            gateway_2,
        ),
        candidate("/facts/dns/dns-1", FactKind::DnsCommit, dns_1),
        candidate("/facts/dns/dns-2", FactKind::DnsCommit, dns_2),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    let gateway = state.gateway.as_ref().expect("gateway");
    assert_eq!(gateway.gateway_commit_id, expected_gateway.0);
    assert_eq!(gateway.routes[0].hostnames, vec![expected_gateway.1]);
    assert_eq!(state.dns.as_ref().expect("dns").dns_commit_id, expected_dns);
    assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 2);
    assert_eq!(status_count(&state, ProjectionIgnoreReason::Conflict), 0);
}

#[test]
fn reducer_projects_serving_commit_as_single_gateway_dns_boundary() {
    let mut payloads = BTreeMap::new();
    let serving_1 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::ServingCommit(ServingCommitFact {
            serving_commit_id: "serving-1".to_string(),
            route_commit_id: "route-1".to_string(),
            gateway_commit_id: "gateway-1".to_string(),
            dns_commit_id: "dns-1".to_string(),
            route_id: RouteId::new("web-http"),
            hostnames: vec!["one.example.com".to_string()],
            backends: vec![BackendEndpoint {
                node_id: NodeId::new("node-1"),
                address: "fd00::1:8080".to_string(),
            }],
            old_backends_to_drain: Vec::new(),
            dns_records: vec![DnsRecordFact {
                name: "one.example.com".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::1".to_string(),
                ttl_seconds: 30,
            }],
            epoch: 1,
        }),
    );
    let serving_2 = insert_payload(
        &mut payloads,
        ProjectionFactPayload::ServingCommit(ServingCommitFact {
            serving_commit_id: "serving-2".to_string(),
            route_commit_id: "route-2".to_string(),
            gateway_commit_id: "gateway-2".to_string(),
            dns_commit_id: "dns-2".to_string(),
            route_id: RouteId::new("web-http"),
            hostnames: vec!["two.example.com".to_string()],
            backends: vec![BackendEndpoint {
                node_id: NodeId::new("node-2"),
                address: "fd00::2:8080".to_string(),
            }],
            old_backends_to_drain: Vec::new(),
            dns_records: vec![DnsRecordFact {
                name: "two.example.com".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::2".to_string(),
                ttl_seconds: 30,
            }],
            epoch: 1,
        }),
    );
    let expected = if serving_1 <= serving_2 {
        ("gateway-1", "dns-1", "one.example.com", "fd00::1")
    } else {
        ("gateway-2", "dns-2", "two.example.com", "fd00::2")
    };
    let candidates = vec![
        candidate(
            "/facts/serving/serving-1",
            FactKind::ServingCommit,
            serving_1,
        ),
        candidate(
            "/facts/serving/serving-2",
            FactKind::ServingCommit,
            serving_2,
        ),
    ];

    let state = reduce_facts(&island("prod"), &candidates, &payloads);

    let gateway = state.gateway.as_ref().expect("gateway");
    let dns = state.dns.as_ref().expect("dns");
    assert_eq!(gateway.gateway_commit_id, expected.0);
    assert_eq!(gateway.routes[0].hostnames, vec![expected.2]);
    assert_eq!(dns.dns_commit_id, expected.1);
    assert_eq!(dns.records[0].value, expected.3);
    assert_eq!(status_count(&state, ProjectionIgnoreReason::Superseded), 1);
}
