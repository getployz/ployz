use std::path::PathBuf;
use std::time::Duration;

use mvp_bus::{FactContentHash, FactKeyPattern, Grant, IslandId, PrincipalId};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_projection::{
    BackendEndpoint, BusFactSource, DnsProjection, DnsRecordFact, DnsRecordProjection,
    GatewayProjection, GatewayRouteProjection, ProjectionReport, ProjectionState, RouteId,
    SnapshotWriteReport,
};

use crate::{
    BusDeployFactWriter, CleanupFailureKind, CleanupStatus, DeployDecisionCandidate,
    DeployDecisionFact, DeployError, DeployFactWriteStatus, DeployFactWriter, DeployId,
    DeployManifest, DeployOutcome, DeployStateMachine, DnsCommitId, GatewayCommitId, PhaseId,
    PhasePolicy, ProjectionCatchUp, RouteCommitId, ServingCommitId, ServingCommitPlan,
    decode_deploy_decision_fact, deploy_cleanup_done_fact_key, deploy_decision_fact_key,
    deploy_decision_fact_payload, read_deploy_decision, select_deploy_decision,
    write_serving_commit,
};

fn serving_commit() -> ServingCommitPlan {
    ServingCommitPlan {
        serving_commit_id: ServingCommitId::new("serving-commit-1"),
        route_commit_id: RouteCommitId::new("route-commit-1"),
        gateway_commit_id: GatewayCommitId::new("gateway-commit-1"),
        dns_commit_id: DnsCommitId::new("dns-commit-1"),
        route_id: RouteId::new("web"),
        hostnames: vec!["app.example.test".to_string()],
        active_backends: vec![BackendEndpoint {
            node_id: NodeId::new("node-new"),
            address: "fd00::2:8080".to_string(),
        }],
        old_backends_to_drain: vec![BackendEndpoint {
            node_id: NodeId::new("node-old"),
            address: "fd00::1:8080".to_string(),
        }],
        dns_records: vec![DnsRecordFact {
            name: "app.example.test".to_string(),
            record_type: "AAAA".to_string(),
            value: "fd00::2".to_string(),
            ttl_seconds: 30,
        }],
        epoch: 1,
    }
}

fn manifest(deploy_id: &str, serving_epoch: u64) -> DeployManifest {
    let mut serving = serving_commit();
    serving.epoch = serving_epoch;
    DeployManifest::new(DeployId::new(deploy_id), Vec::new(), serving)
}

fn visible_nodes() -> VisibleNodes {
    VisibleNodes::new([NodeId::new("node-new"), NodeId::new("node-old")])
}

fn decision_fact(deploy_id: &str, serving_epoch: u64) -> DeployDecisionFact {
    DeployDecisionFact::new(manifest(deploy_id, serving_epoch), visible_nodes())
}

#[test]
fn drain_before_serving_commit_is_rejected() {
    let mut state = DeployStateMachine::new(DeployId::new("deploy-1"), [PhaseId::new(1)]);

    let error = state.finish_cleanup().expect_err("cleanup requires commit");

    assert!(matches!(error, DeployError::ServingCommitRequired));
}

#[test]
fn phase_commit_requires_readiness() {
    let mut state = DeployStateMachine::new(DeployId::new("deploy-1"), [PhaseId::new(1)]);

    let error = state
        .commit_phase(PhaseId::new(1), PhasePolicy::irreversible())
        .expect_err("phase cannot commit before ready");

    assert!(matches!(error, DeployError::PhaseNotReady { .. }));
}

#[test]
fn cleanup_pending_preserves_serving_commit_success() {
    let mut state = DeployStateMachine::new(DeployId::new("deploy-1"), [PhaseId::new(1)]);
    state.mark_preparing(PhaseId::new(1)).unwrap();
    state.mark_ready(PhaseId::new(1)).unwrap();
    state
        .commit_phase(PhaseId::new(1), PhasePolicy::reversible())
        .unwrap();
    state
        .commit_serving(serving_commit().serving_commit_id)
        .unwrap();

    let result = state
        .cleanup_pending(CleanupStatus::Pending {
            reason: crate::CleanupPendingReason::StopUnavailable {
                node_id: NodeId::new("node-old"),
                cause: CleanupFailureKind::NoResponders,
            },
        })
        .unwrap();

    assert_eq!(result.outcome, DeployOutcome::CleanupPending);
    assert_eq!(
        result.serving_commit_id.as_ref().map(ToString::to_string),
        Some("serving-commit-1".to_string())
    );
}

#[test]
fn irreversible_phase_failure_is_loud() {
    let mut state = DeployStateMachine::new(DeployId::new("deploy-1"), [PhaseId::new(1)]);
    state.mark_preparing(PhaseId::new(1)).unwrap();
    state.mark_ready(PhaseId::new(1)).unwrap();
    state
        .commit_phase(PhaseId::new(1), PhasePolicy::irreversible())
        .unwrap();

    let error = state
        .block_after_irreversible()
        .expect_err("irreversible phase should block");

    assert!(matches!(error, DeployError::BlockedAfterIrreversiblePhase));
    assert_eq!(
        state.result().expect("terminal result").outcome,
        DeployOutcome::DeployBlockedAfterIrreversiblePhase
    );
}

#[tokio::test]
async fn conflicting_serving_commit_fact_rejects_cutover() {
    let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
    let session = authority.grant_in(
        mvp_bus::IslandId::new("prod"),
        mvp_bus::PrincipalId::new("deploy"),
        mvp_bus::Grant::allow_all(),
    );
    let commit = serving_commit();
    write_serving_commit(&bus, &session, &commit)
        .await
        .expect("first serving commit writes");

    let mut conflicting = commit.clone();
    conflicting.hostnames = vec!["other.example.test".to_string()];

    let error = write_serving_commit(&bus, &session, &conflicting)
        .await
        .expect_err("conflicting serving commit should be rejected");

    assert!(matches!(error, DeployError::ServingFactConflict { .. }));
}

#[test]
fn projection_catch_up_allows_unrelated_gateway_revision_suffix() {
    let commit = serving_commit();
    let report = projection_report_for_commit(
        &commit,
        "gateway:gateway-commit-1:route-commit-1:acme:none",
        "dns:dns-commit-1",
    );

    let proof = ProjectionCatchUp::from_report(&commit, &report).expect("catch-up proof");

    assert_eq!(proof.serving_commit_id(), &commit.serving_commit_id);
}

#[test]
fn deploy_fact_keys_are_deploy_id_scoped() {
    let deploy_id = DeployId::new("deploy-1");

    assert_eq!(
        deploy_decision_fact_key(&deploy_id)
            .expect("decision key")
            .as_str(),
        "/facts/deploy/deploy-1/decision"
    );
    assert_eq!(
        deploy_cleanup_done_fact_key(&deploy_id)
            .expect("cleanup key")
            .as_str(),
        "/facts/deploy/deploy-1/cleanup/done"
    );
}

#[tokio::test]
async fn duplicate_deploy_decision_is_already_present() {
    let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
    let session = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("deploy"),
        Grant::allow_all(),
    );
    let writer = BusDeployFactWriter::new(bus, session);
    let fact = decision_fact("deploy-1", 1);

    let inserted = writer
        .write_decision(fact.clone())
        .await
        .expect("insert decision");
    let repeated = writer.write_decision(fact).await.expect("repeat decision");

    assert_eq!(inserted.status(), DeployFactWriteStatus::Inserted);
    assert_eq!(repeated.status(), DeployFactWriteStatus::AlreadyPresent);
    assert_eq!(inserted.content_hash(), repeated.content_hash());
}

#[tokio::test]
async fn conflicting_deploy_decision_returns_structured_conflict() {
    let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
    let session = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("deploy"),
        Grant::allow_all(),
    );
    let writer = BusDeployFactWriter::new(bus, session);
    writer
        .write_decision(decision_fact("deploy-1", 1))
        .await
        .expect("insert decision");

    let error = writer
        .write_decision(decision_fact("deploy-1", 2))
        .await
        .expect_err("conflicting decision should fail");

    assert!(matches!(
        error,
        DeployError::DeployFactConflict {
            key,
            principal,
            ..
        } if key.as_str() == "/facts/deploy/deploy-1/decision"
            && principal == PrincipalId::new("deploy")
    ));
}

#[test]
fn deploy_decision_selection_uses_epoch_desc_then_hash_asc() {
    let key = deploy_decision_fact_key(&DeployId::new("deploy-1")).expect("key");
    let low_epoch = DeployDecisionCandidate {
        fact: decision_fact("deploy-1", 1),
        author: PrincipalId::new("deploy-a"),
        content_hash: FactContentHash::new("b3:0002"),
    };
    let high_epoch_high_hash = DeployDecisionCandidate {
        fact: decision_fact("deploy-1", 2),
        author: PrincipalId::new("deploy-b"),
        content_hash: FactContentHash::new("b3:ffff"),
    };
    let high_epoch_low_hash = DeployDecisionCandidate {
        fact: decision_fact("deploy-1", 2),
        author: PrincipalId::new("deploy-c"),
        content_hash: FactContentHash::new("b3:0001"),
    };

    let selection = select_deploy_decision(
        vec![
            low_epoch,
            high_epoch_high_hash.clone(),
            high_epoch_low_hash.clone(),
        ],
        key,
    )
    .expect("select decision");

    assert_eq!(selection.winner, high_epoch_low_hash);
    assert_eq!(selection.superseded[0], high_epoch_high_hash);
}

#[test]
fn malformed_deploy_fact_payload_is_structured() {
    let key = deploy_decision_fact_key(&DeployId::new("deploy-1")).expect("key");
    let error = decode_deploy_decision_fact(&key, &b"{not-json".as_slice().into())
        .expect_err("malformed payload should fail");

    assert!(matches!(error, DeployError::WirePayload { .. }));
}

#[test]
fn wrong_deploy_fact_kind_is_structured() {
    let key = deploy_decision_fact_key(&DeployId::new("deploy-1")).expect("key");
    let cleanup = crate::deploy_cleanup_done_fact_payload(&crate::DeployCleanupDoneFact::new(
        &manifest("deploy-1", 1),
    ))
    .expect("cleanup payload");
    let error =
        decode_deploy_decision_fact(&key, &cleanup).expect_err("wrong fact kind should fail");

    assert!(matches!(
        error,
        DeployError::DeployFactKindMismatch {
            expected_kind: "decision",
            ..
        }
    ));
}

#[test]
fn deploy_decision_reader_selects_conflict_candidate_without_operator_choice() {
    let (raw_bus, authority) = mvp_bus::harness::InMemoryBus::new_with_authority();
    let source = BusFactSource::new(raw_bus.clone());
    let writer_a = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("deploy-a"),
        Grant::empty()
            .with_fact_write(FactKeyPattern::parse("/facts/deploy/>").expect("pattern"))
            .with_fact_read(FactKeyPattern::parse("/facts/deploy/>").expect("pattern")),
    );
    let writer_b = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("deploy-b"),
        Grant::empty()
            .with_fact_write(FactKeyPattern::parse("/facts/deploy/>").expect("pattern"))
            .with_fact_read(FactKeyPattern::parse("/facts/deploy/>").expect("pattern")),
    );
    let deploy_id = DeployId::new("deploy-1");
    let key = deploy_decision_fact_key(&deploy_id).expect("key");
    raw_bus
        .write_fact_payload(
            &writer_a,
            key.clone(),
            deploy_decision_fact_payload(&decision_fact("deploy-1", 1)).expect("payload"),
        )
        .expect("first write");
    let _ = raw_bus
        .write_fact_payload(
            &writer_b,
            key,
            deploy_decision_fact_payload(&decision_fact("deploy-1", 2)).expect("payload"),
        )
        .expect("second write reports conflict after storing candidate");

    let selection = read_deploy_decision(&source, writer_a.island(), &writer_a, &deploy_id)
        .expect("read conflicted decisions");

    assert_eq!(selection.winner.fact.serving_epoch, 2);
    assert_eq!(selection.superseded.len(), 1);
}

fn projection_report_for_commit(
    commit: &ServingCommitPlan,
    gateway_revision: &str,
    dns_revision: &str,
) -> ProjectionReport {
    let mut state = ProjectionState::for_island(mvp_bus::IslandId::new("prod"));
    let mut hostnames = commit.hostnames.clone();
    hostnames.sort();
    let mut backends = commit.active_backends.clone();
    backends.sort();
    let mut old_backends_to_drain = commit.old_backends_to_drain.clone();
    old_backends_to_drain.sort();
    let mut dns_records = commit
        .dns_records
        .clone()
        .into_iter()
        .map(DnsRecordProjection::from)
        .collect::<Vec<_>>();
    dns_records.sort();
    state.gateway = Some(GatewayProjection {
        gateway_commit_id: commit.gateway_commit_id.to_string(),
        route_commit_id: commit.route_commit_id.to_string(),
        routes: vec![GatewayRouteProjection {
            route_id: commit.route_id.clone(),
            hostnames,
            backends,
            old_backends_to_drain,
        }],
    });
    state.dns = Some(DnsProjection {
        dns_commit_id: commit.dns_commit_id.to_string(),
        records: dns_records,
    });
    ProjectionReport {
        state,
        sqlite_path: PathBuf::from("projections.sqlite"),
        gateway_snapshot: Some(SnapshotWriteReport {
            path: PathBuf::from("gateway.snapshot"),
            bytes_written: 1,
            revision: gateway_revision.to_string(),
        }),
        dns_snapshot: Some(SnapshotWriteReport {
            path: PathBuf::from("dns.snapshot"),
            bytes_written: 1,
            revision: dns_revision.to_string(),
        }),
        duration: Duration::from_millis(1),
    }
}
