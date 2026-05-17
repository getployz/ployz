use std::path::PathBuf;
use std::time::Duration;

use mvp_projection::{
    BackendEndpoint, DnsProjection, DnsRecordFact, DnsRecordProjection, GatewayProjection,
    GatewayRouteProjection, NodeId, ProjectionReport, ProjectionState, RouteId,
    SnapshotWriteReport,
};

use crate::{
    CleanupFailureKind, CleanupStatus, DeployError, DeployId, DeployOutcome, DeployStateMachine,
    DnsCommitId, GatewayCommitId, PhaseId, PhasePolicy, ProjectionCatchUp, RouteCommitId,
    ServingCommitId, ServingCommitPlan, write_serving_commit,
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
                node_id: crate::DeployNodeId::new("node-old"),
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
