use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use mvp_bus::{
    BusSession, FactContentHash, FactKey, FactKeyPattern, FactPayload, Grant, IslandId, PrincipalId,
};
use mvp_commands::{CommandContext, InMemoryCommandPhaseStore};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_projection::{
    BackendEndpoint, CandidateStatus, DnsProjection, DnsRecordFact, FactCandidate, FactKind,
    FactSource, FactSourceResult, GatewayProjection, GatewayRouteProjection, ProjectionReport,
    ProjectionState, RouteId, SnapshotWriteReport,
};
use mvp_routing::{
    DnsCommitId, GatewayCommitId, ProjectionCatchUp, RouteCommitId, RoutingResult, ServingCommitId,
    ServingCommitPlan, ServingFactWriter, WrittenServingFact, serving_commit_fact_key,
    serving_commit_fact_payload,
};

use crate::{
    BranchEnvironmentCommand, BranchEnvironmentRequest, EnvironmentBranchFact, EnvironmentBranchId,
    EnvironmentCommandId, EnvironmentCommandResult, EnvironmentEpoch, EnvironmentError,
    EnvironmentFactWriter, EnvironmentHeadFact, EnvironmentHeadId, EnvironmentId,
    EnvironmentPromoteDecisionFact, EnvironmentRollbackDecisionFact, EnvironmentRouteRef,
    EnvironmentServingPendingReason, EnvironmentVolumeForkEvidence,
    EnvironmentVolumeForkParticipant, EnvironmentVolumeForkReply, EnvironmentVolumeForkRequest,
    EnvironmentVolumeRef, PromoteEnvironmentCommand, PromoteEnvironmentRequest,
    RollbackEnvironmentCommand, RollbackEnvironmentRequest, WrittenEnvironmentFact,
    current_environment_head, decode_environment_head_fact, environment_branch_fact_key,
    environment_branch_fact_payload, environment_head_fact_key, environment_head_fact_payload,
    environment_promote_decision_fact_key, environment_rollback_decision_fact_key,
    read_environment_heads, require_expected_environment_epoch,
};

#[test]
fn current_head_selects_highest_epoch_and_records_superseded_candidates() {
    let fixture = Fixture::new();
    let env = environment("prod");
    let old_head = head(&env, 1, "head-1", "serving-1", vec!["volume-1"], None);
    let new_head = head(
        &env,
        2,
        "head-2",
        "serving-2",
        vec!["volume-2"],
        Some(&old_head),
    );
    let mut source = MemoryFactSource::default();
    source.insert_head(
        &fixture,
        PrincipalId::new("alice"),
        CandidateStatus::Verified,
        old_head,
    );
    source.insert_head(
        &fixture,
        PrincipalId::new("bob"),
        CandidateStatus::Verified,
        new_head,
    );

    let model = read_environment_heads(&source, &fixture.session, &env).expect("read heads");

    let current = model.current.expect("current head");
    assert_eq!(current.fact.epoch, epoch(2));
    assert_eq!(current.fact.volume_refs, vec![volume("volume-2")]);
    assert_eq!(model.superseded.len(), 1);
    assert_eq!(model.superseded[0].candidate.fact.epoch, epoch(1));
    assert_eq!(model.superseded[0].superseded_by_epoch, epoch(2));
    assert_eq!(
        model.superseded[0].superseded_by_principal,
        PrincipalId::new("bob")
    );
}

#[test]
fn same_epoch_conflict_resolves_by_content_hash() {
    let fixture = Fixture::new();
    let env = environment("prod");
    let left = head(&env, 3, "aaa", "serving-a", vec!["volume-a"], None);
    let right = head(&env, 3, "zzz", "serving-z", vec!["volume-z"], None);
    let mut source = MemoryFactSource::default();
    source.insert_head(
        &fixture,
        PrincipalId::new("left"),
        CandidateStatus::Conflict,
        left.clone(),
    );
    source.insert_head(
        &fixture,
        PrincipalId::new("right"),
        CandidateStatus::Conflict,
        right.clone(),
    );

    let model = read_environment_heads(&source, &fixture.session, &env).expect("read heads");
    let mut expected = vec![left, right]
        .into_iter()
        .map(|fact| {
            let payload = environment_head_fact_payload(&fact).expect("head payload");
            (FactContentHash::for_payload(&payload), fact)
        })
        .collect::<Vec<_>>();
    expected.sort_by(|left, right| left.0.cmp(&right.0));

    assert_eq!(model.current.expect("current").fact, expected[0].1);
}

#[test]
fn malformed_and_wrong_key_payloads_are_structured_errors() {
    let fixture = Fixture::new();
    let env = environment("prod");
    let key = environment_head_fact_key(&env, epoch(1)).expect("head key");
    let malformed = FactPayload::from(b"not-json".to_vec());

    let malformed_error =
        decode_environment_head_fact(&key, &malformed).expect_err("malformed payload rejected");
    assert!(matches!(
        malformed_error,
        EnvironmentError::DecodePayload { key: error_key, .. } if error_key == key
    ));

    let wrong_payload = environment_head_fact_payload(&head(
        &environment("staging"),
        1,
        "head-1",
        "serving-1",
        vec!["volume-1"],
        None,
    ))
    .expect("wrong payload");
    let wrong_key_error =
        decode_environment_head_fact(&key, &wrong_payload).expect_err("wrong key rejected");
    assert!(matches!(
        wrong_key_error,
        EnvironmentError::FactKeyPayloadMismatch { key: error_key, environment: payload_environment }
            if error_key == key && payload_environment == environment("staging")
    ));

    let branch_key = environment_branch_fact_key(&env, &branch_id("pr-1")).expect("branch key");
    let branch_payload =
        environment_branch_fact_payload(&branch(&env, &branch_id("pr-1"))).expect("branch payload");
    let head_decode_error =
        decode_environment_head_fact(&branch_key, &branch_payload).expect_err("wrong kind");
    assert!(matches!(
        head_decode_error,
        EnvironmentError::WrongFactKeyShape { key: error_key } if error_key == branch_key
    ));

    assert!(
        current_environment_head(&MemoryFactSource::default(), &fixture.session, &env)
            .expect("empty read")
            .is_none()
    );
}

#[test]
fn stale_expected_epoch_fails_before_mutation() {
    let fixture = Fixture::new();
    let env = environment("prod");
    let mut source = MemoryFactSource::default();
    source.insert_head(
        &fixture,
        PrincipalId::new("alice"),
        CandidateStatus::Verified,
        head(&env, 4, "head-4", "serving-4", vec!["volume-4"], None),
    );

    let error = require_expected_environment_epoch(&source, &fixture.session, &env, epoch(3))
        .expect_err("stale epoch rejected");

    assert!(matches!(
        error,
        EnvironmentError::StaleExpectedEpoch {
            environment,
            expected,
            actual: Some(actual),
        } if environment == env && expected == epoch(3) && actual == epoch(4)
    ));
}

#[test]
fn head_facts_preserve_volume_lineage_without_serving_payloads() {
    let env = environment("prod");
    let previous = head(
        &env,
        1,
        "head-1",
        "serving-1",
        vec!["db-prod", "redis-prod"],
        None,
    );
    let promoted = head(
        &env,
        2,
        "head-2",
        "serving-2",
        vec!["db-branch", "redis-branch"],
        Some(&previous),
    );
    let payload = environment_head_fact_payload(&promoted).expect("head payload");
    let json = String::from_utf8(payload.as_bytes().to_vec()).expect("payload utf8");

    assert!(json.contains("db-branch"));
    assert!(json.contains("db-prod"));
    assert!(!json.contains("active_backends"));
    assert!(!json.contains("dns_records"));
    assert_eq!(
        promoted
            .previous_head
            .as_ref()
            .expect("previous head")
            .volume_refs,
        previous.volume_refs
    );
}

#[tokio::test]
async fn branch_writes_branch_and_head_after_exact_fork_evidence() {
    let fixture = Fixture::new();
    let env = environment("prod");
    let branch_env = environment("pr-123");
    let branch_id = branch_id("pr-123");
    let mut source = MemoryFactSource::default();
    source.insert_head(
        &fixture,
        PrincipalId::new("prod-writer"),
        CandidateStatus::Verified,
        head(
            &env,
            1,
            "head-prod-1",
            "serving-prod-1",
            vec!["db-prod"],
            None,
        ),
    );
    let writer = RecordingEnvironmentWriter::default();
    let fork = RecordingForkParticipant::default();
    let branch_head = branch_head(&branch_env, &branch_id, vec!["db-pr-123"]);

    let result = BranchEnvironmentCommand::new(&source, &fixture.session, &writer, &fork)
        .execute(BranchEnvironmentRequest {
            command_id: command("branch-1"),
            source_environment: env,
            expected_source_epoch: epoch(1),
            branch_id,
            branch_environment: branch_env,
            branch_head: branch_head.clone(),
            route_refs: vec![EnvironmentRouteRef::parse("route-pr-1").expect("route ref")],
            visible_nodes: visible_nodes(),
        })
        .await
        .expect("branch succeeds");

    assert_eq!(result.head, branch_head);
    assert_eq!(result.branch.forked_volume_refs, vec![volume("db-pr-123")]);
    assert_eq!(
        writer.events(),
        vec![
            RecordedEnvironmentEvent::Branch,
            RecordedEnvironmentEvent::Head
        ]
    );
    assert_eq!(fork.requests().len(), 1);
}

#[tokio::test]
async fn branch_rejects_source_head_change_after_fork_before_writing_facts() {
    let fixture = Fixture::new();
    let env = environment("prod");
    let branch_env = environment("pr-123");
    let branch_id = branch_id("pr-123");
    let original = head(
        &env,
        1,
        "head-prod-1",
        "serving-prod-1",
        vec!["db-prod"],
        None,
    );
    let mut changed = original.clone();
    changed.serving_commit_id = ServingCommitId::new("serving-prod-conflict");
    let source = SequencedFactSource::new([
        source_with_head(&fixture, original),
        source_with_head(&fixture, changed),
    ]);
    let writer = RecordingEnvironmentWriter::default();
    let fork = RecordingForkParticipant::default();

    let error = BranchEnvironmentCommand::new(&source, &fixture.session, &writer, &fork)
        .execute(BranchEnvironmentRequest {
            command_id: command("branch-1"),
            source_environment: env,
            expected_source_epoch: epoch(1),
            branch_id: branch_id.clone(),
            branch_environment: branch_env.clone(),
            branch_head: branch_head(&branch_env, &branch_id, vec!["db-pr-123"]),
            route_refs: Vec::new(),
            visible_nodes: visible_nodes(),
        })
        .await
        .expect_err("changed source head rejected");

    assert!(matches!(error, EnvironmentError::StaleExpectedEpoch { .. }));
    assert!(writer.events().is_empty());
}

#[tokio::test]
async fn branch_rejects_forged_fork_evidence_before_writing_facts() {
    let fixture = Fixture::new();
    let env = environment("prod");
    let branch_env = environment("pr-123");
    let branch_id = branch_id("pr-123");
    let mut source = MemoryFactSource::default();
    source.insert_head(
        &fixture,
        PrincipalId::new("prod-writer"),
        CandidateStatus::Verified,
        head(
            &env,
            1,
            "head-prod-1",
            "serving-prod-1",
            vec!["db-prod"],
            None,
        ),
    );
    let writer = RecordingEnvironmentWriter::default();
    let fork = RecordingForkParticipant::with_forged_source(volume("other-volume"));

    let error = BranchEnvironmentCommand::new(&source, &fixture.session, &writer, &fork)
        .execute(BranchEnvironmentRequest {
            command_id: command("branch-1"),
            source_environment: env,
            expected_source_epoch: epoch(1),
            branch_id: branch_id.clone(),
            branch_environment: branch_env.clone(),
            branch_head: branch_head(&branch_env, &branch_id, vec!["db-pr-123"]),
            route_refs: Vec::new(),
            visible_nodes: visible_nodes(),
        })
        .await
        .expect_err("forged evidence rejected");

    assert!(matches!(
        error,
        EnvironmentError::DecisionHeadMismatch { .. }
    ));
    assert!(writer.events().is_empty());
}

#[tokio::test]
async fn promote_writes_decision_before_serving_and_requires_projection_catchup() {
    let fixture = Fixture::new();
    let env = environment("prod");
    let branch_env = environment("pr-123");
    let production = head(
        &env,
        1,
        "head-prod-1",
        "serving-prod-1",
        vec!["db-prod"],
        None,
    );
    let branch = head(
        &branch_env,
        1,
        "head-pr-1",
        "serving-pr-1",
        vec!["db-pr-123"],
        None,
    );
    let mut source = MemoryFactSource::default();
    source.insert_head(
        &fixture,
        PrincipalId::new("prod"),
        CandidateStatus::Verified,
        production,
    );
    source.insert_head(
        &fixture,
        PrincipalId::new("branch"),
        CandidateStatus::Verified,
        branch,
    );
    let writer = RecordingEnvironmentWriter::default();
    let serving = RecordingServingWriter::default();
    let plan = serving_plan("serving-promote-1", "node-branch", "fd00::2:8080", 2);

    let command = PromoteEnvironmentCommand::new(&source, &fixture.session, &writer, &serving);
    let cx = CommandContext::new(Arc::new(InMemoryCommandPhaseStore::empty()));
    let request = PromoteEnvironmentRequest {
        command_id: command_id("promote-1"),
        environment: env,
        expected_environment_epoch: epoch(1),
        branch_environment: branch_env,
        expected_branch_epoch: epoch(1),
        serving_commit: plan.clone(),
        visible_nodes: visible_nodes(),
    };
    let no_catchup = command
        .execute_phased(&cx, request.clone(), None)
        .await
        .expect("pending without catchup");

    assert!(matches!(
        no_catchup,
        EnvironmentCommandResult::Pending {
            reason: EnvironmentServingPendingReason::ProjectionCatchUpMissing
        }
    ));
    assert_eq!(
        writer.events()[0],
        RecordedEnvironmentEvent::PromoteDecision
    );
    assert_eq!(writer.events().len(), 1);
    assert_eq!(serving.writes(), vec![plan.serving_commit_id.clone()]);

    let complete = command
        .execute_phased(&cx, request, Some(catch_up(&fixture, &plan)))
        .await
        .expect("promote finalize");

    assert!(
        matches!(complete, EnvironmentCommandResult::Complete { head } if head.volume_refs == vec![volume("db-pr-123")])
    );
}

#[tokio::test]
async fn phased_promote_resumes_after_serving_commit_without_rewriting_decision() {
    let fixture = Fixture::new();
    let env = environment("prod");
    let branch_env = environment("pr-123");
    let production = head(
        &env,
        1,
        "head-prod-1",
        "serving-prod-1",
        vec!["db-prod"],
        None,
    );
    let branch = head(
        &branch_env,
        1,
        "head-pr-1",
        "serving-pr-1",
        vec!["db-pr-123"],
        None,
    );
    let mut source = MemoryFactSource::default();
    source.insert_head(
        &fixture,
        PrincipalId::new("prod"),
        CandidateStatus::Verified,
        production,
    );
    source.insert_head(
        &fixture,
        PrincipalId::new("branch"),
        CandidateStatus::Verified,
        branch,
    );
    let writer = RecordingEnvironmentWriter::default();
    let serving = RecordingServingWriter::default();
    let plan = serving_plan("serving-promote-1", "node-branch", "fd00::2:8080", 2);
    let command = PromoteEnvironmentCommand::new(&source, &fixture.session, &writer, &serving);
    let cx = CommandContext::new(Arc::new(InMemoryCommandPhaseStore::empty()));
    let request = PromoteEnvironmentRequest {
        command_id: command_id("promote-1"),
        environment: env,
        expected_environment_epoch: epoch(1),
        branch_environment: branch_env,
        expected_branch_epoch: epoch(1),
        serving_commit: plan.clone(),
        visible_nodes: visible_nodes(),
    };

    let pending = command
        .execute_phased(&cx, request.clone(), None)
        .await
        .expect("phased promote pending");

    assert!(matches!(
        pending,
        EnvironmentCommandResult::Pending {
            reason: EnvironmentServingPendingReason::ProjectionCatchUpMissing
        }
    ));
    assert_eq!(
        writer.events(),
        vec![RecordedEnvironmentEvent::PromoteDecision]
    );
    assert_eq!(serving.writes(), vec![plan.serving_commit_id.clone()]);

    let mut changed_request = request;
    changed_request.serving_commit =
        serving_plan("serving-promote-wrong", "node-wrong", "fd00::99:8080", 99);
    let complete = command
        .execute_phased(&cx, changed_request, Some(catch_up(&fixture, &plan)))
        .await
        .expect("phased promote resumes");

    assert!(
        matches!(complete, EnvironmentCommandResult::Complete { head } if head.volume_refs == vec![volume("db-pr-123")])
    );
    assert_eq!(
        writer.events(),
        vec![
            RecordedEnvironmentEvent::PromoteDecision,
            RecordedEnvironmentEvent::Head
        ]
    );
    assert_eq!(serving.writes(), vec![plan.serving_commit_id]);
}

#[tokio::test]
async fn promote_rejects_second_read_head_change_before_any_mutation() {
    let fixture = Fixture::new();
    let env = environment("prod");
    let branch_env = environment("pr-123");
    let production = head(
        &env,
        1,
        "head-prod-1",
        "serving-prod-1",
        vec!["db-prod"],
        None,
    );
    let mut changed_production = production.clone();
    changed_production.serving_commit_id = ServingCommitId::new("serving-prod-conflict");
    let branch = head(
        &branch_env,
        1,
        "head-pr-1",
        "serving-pr-1",
        vec!["db-pr-123"],
        None,
    );
    let source = SequencedFactSource::new([
        source_with_head(&fixture, production),
        source_with_head(&fixture, branch.clone()),
        source_with_head(&fixture, changed_production),
        source_with_head(&fixture, branch),
    ]);
    let writer = RecordingEnvironmentWriter::default();
    let serving = RecordingServingWriter::default();

    let error = PromoteEnvironmentCommand::new(&source, &fixture.session, &writer, &serving)
        .execute_phased(
            &CommandContext::new(Arc::new(InMemoryCommandPhaseStore::empty())),
            PromoteEnvironmentRequest {
                command_id: command_id("promote-1"),
                environment: env,
                expected_environment_epoch: epoch(1),
                branch_environment: branch_env,
                expected_branch_epoch: epoch(1),
                serving_commit: serving_plan("serving-promote-1", "node-branch", "fd00::2:8080", 2),
                visible_nodes: visible_nodes(),
            },
            None,
        )
        .await
        .expect_err("stale second read rejected");

    assert!(matches!(error, EnvironmentError::StaleExpectedEpoch { .. }));
    assert!(writer.events().is_empty());
    assert!(serving.writes().is_empty());
}

#[tokio::test]
async fn rollback_writes_forward_head_using_previous_volume_refs() {
    let fixture = Fixture::new();
    let env = environment("prod");
    let previous = head(
        &env,
        1,
        "head-prod-1",
        "serving-prod-1",
        vec!["db-prod"],
        None,
    );
    let current = head(
        &env,
        2,
        "head-prod-2",
        "serving-promote-1",
        vec!["db-pr-123"],
        Some(&previous),
    );
    let mut source = MemoryFactSource::default();
    source.insert_head(
        &fixture,
        PrincipalId::new("prod"),
        CandidateStatus::Verified,
        current,
    );
    let writer = RecordingEnvironmentWriter::default();
    let serving = RecordingServingWriter::default();
    let plan = serving_plan("serving-rollback-1", "node-prod", "fd00::1:8080", 3);
    let command = RollbackEnvironmentCommand::new(&source, &fixture.session, &writer, &serving);

    let cx = CommandContext::new(Arc::new(InMemoryCommandPhaseStore::empty()));
    let complete = command
        .execute_phased(
            &cx,
            RollbackEnvironmentRequest {
                command_id: command_id("rollback-1"),
                environment: env,
                expected_environment_epoch: epoch(2),
                serving_commit: plan.clone(),
                visible_nodes: visible_nodes(),
            },
            Some(catch_up(&fixture, &plan)),
        )
        .await
        .expect("rollback finalize");

    assert_eq!(
        writer.events()[0],
        RecordedEnvironmentEvent::RollbackDecision
    );
    assert!(
        matches!(complete, EnvironmentCommandResult::Complete { head } if head.epoch == epoch(3) && head.volume_refs == vec![volume("db-prod")])
    );
}

#[tokio::test]
async fn phased_rollback_resumes_after_serving_commit_without_rewriting_decision() {
    let fixture = Fixture::new();
    let env = environment("prod");
    let previous = head(
        &env,
        1,
        "head-prod-1",
        "serving-prod-1",
        vec!["db-prod"],
        None,
    );
    let current = head(
        &env,
        2,
        "head-prod-2",
        "serving-promote-1",
        vec!["db-pr-123"],
        Some(&previous),
    );
    let mut source = MemoryFactSource::default();
    source.insert_head(
        &fixture,
        PrincipalId::new("prod"),
        CandidateStatus::Verified,
        current,
    );
    let writer = RecordingEnvironmentWriter::default();
    let serving = RecordingServingWriter::default();
    let plan = serving_plan("serving-rollback-1", "node-prod", "fd00::1:8080", 3);
    let command = RollbackEnvironmentCommand::new(&source, &fixture.session, &writer, &serving);
    let cx = CommandContext::new(Arc::new(InMemoryCommandPhaseStore::empty()));
    let request = RollbackEnvironmentRequest {
        command_id: command_id("rollback-1"),
        environment: env,
        expected_environment_epoch: epoch(2),
        serving_commit: plan.clone(),
        visible_nodes: visible_nodes(),
    };

    let pending = command
        .execute_phased(&cx, request.clone(), None)
        .await
        .expect("phased rollback pending");

    assert!(matches!(
        pending,
        EnvironmentCommandResult::Pending {
            reason: EnvironmentServingPendingReason::ProjectionCatchUpMissing
        }
    ));
    assert_eq!(
        writer.events(),
        vec![RecordedEnvironmentEvent::RollbackDecision]
    );
    assert_eq!(serving.writes(), vec![plan.serving_commit_id.clone()]);

    let mut changed_request = request;
    changed_request.serving_commit =
        serving_plan("serving-rollback-wrong", "node-wrong", "fd00::99:8080", 99);
    let complete = command
        .execute_phased(&cx, changed_request, Some(catch_up(&fixture, &plan)))
        .await
        .expect("phased rollback resumes");

    assert!(
        matches!(complete, EnvironmentCommandResult::Complete { head } if head.epoch == epoch(3) && head.volume_refs == vec![volume("db-prod")])
    );
    assert_eq!(
        writer.events(),
        vec![
            RecordedEnvironmentEvent::RollbackDecision,
            RecordedEnvironmentEvent::Head
        ]
    );
    assert_eq!(serving.writes(), vec![plan.serving_commit_id]);
}

#[tokio::test]
async fn rollback_rejects_missing_target_and_second_read_change_before_any_mutation() {
    let fixture = Fixture::new();
    let env = environment("prod");
    let source = source_with_head(
        &fixture,
        head(
            &env,
            1,
            "head-prod-1",
            "serving-prod-1",
            vec!["db-prod"],
            None,
        ),
    );
    let writer = RecordingEnvironmentWriter::default();
    let serving = RecordingServingWriter::default();

    let missing_error =
        RollbackEnvironmentCommand::new(&source, &fixture.session, &writer, &serving)
            .execute_phased(
                &CommandContext::new(Arc::new(InMemoryCommandPhaseStore::empty())),
                RollbackEnvironmentRequest {
                    command_id: command_id("rollback-1"),
                    environment: env.clone(),
                    expected_environment_epoch: epoch(1),
                    serving_commit: serving_plan(
                        "serving-rollback-1",
                        "node-prod",
                        "fd00::1:8080",
                        2,
                    ),
                    visible_nodes: visible_nodes(),
                },
                None,
            )
            .await
            .expect_err("missing rollback target rejected");
    assert!(matches!(
        missing_error,
        EnvironmentError::RollbackTargetMissing { .. }
    ));
    assert!(writer.events().is_empty());
    assert!(serving.writes().is_empty());

    let previous = head(
        &env,
        1,
        "head-prod-1",
        "serving-prod-1",
        vec!["db-prod"],
        None,
    );
    let current = head(
        &env,
        2,
        "head-prod-2",
        "serving-promote-1",
        vec!["db-pr-123"],
        Some(&previous),
    );
    let mut changed_current = current.clone();
    changed_current.serving_commit_id = ServingCommitId::new("serving-promote-conflict");
    let source = SequencedFactSource::new([
        source_with_head(&fixture, current),
        source_with_head(&fixture, changed_current),
    ]);

    let stale_error = RollbackEnvironmentCommand::new(&source, &fixture.session, &writer, &serving)
        .execute_phased(
            &CommandContext::new(Arc::new(InMemoryCommandPhaseStore::empty())),
            RollbackEnvironmentRequest {
                command_id: command_id("rollback-2"),
                environment: env,
                expected_environment_epoch: epoch(2),
                serving_commit: serving_plan("serving-rollback-2", "node-prod", "fd00::1:8080", 3),
                visible_nodes: visible_nodes(),
            },
            None,
        )
        .await
        .expect_err("stale second read rejected");
    assert!(matches!(
        stale_error,
        EnvironmentError::StaleExpectedEpoch { .. }
    ));
    assert!(writer.events().is_empty());
    assert!(serving.writes().is_empty());
}

#[tokio::test]
async fn command_entry_conflict_rejects_before_participant_calls() {
    let fixture = Fixture::new();
    let env = environment("prod");
    let branch_env = environment("pr-123");
    let branch_id = branch_id("pr-123");
    let mut source = MemoryFactSource::default();
    source.insert_head(
        &fixture,
        PrincipalId::new("prod"),
        CandidateStatus::Verified,
        head(
            &env,
            2,
            "head-prod-2",
            "serving-prod-2",
            vec!["db-prod"],
            None,
        ),
    );
    let writer = RecordingEnvironmentWriter::default();
    let fork = RecordingForkParticipant::default();

    let error = BranchEnvironmentCommand::new(&source, &fixture.session, &writer, &fork)
        .execute(BranchEnvironmentRequest {
            command_id: command("branch-1"),
            source_environment: env,
            expected_source_epoch: epoch(1),
            branch_id: branch_id.clone(),
            branch_environment: branch_env.clone(),
            branch_head: branch_head(&branch_env, &branch_id, vec!["db-pr-123"]),
            route_refs: Vec::new(),
            visible_nodes: visible_nodes(),
        })
        .await
        .expect_err("stale before mutation");

    assert!(matches!(error, EnvironmentError::StaleExpectedEpoch { .. }));
    assert!(fork.requests().is_empty());
    assert!(writer.events().is_empty());
}

#[derive(Default, Clone)]
struct MemoryFactSource {
    candidates: Vec<FactCandidate>,
    payloads: BTreeMap<FactContentHash, FactPayload>,
}

impl MemoryFactSource {
    fn insert_head(
        &mut self,
        fixture: &Fixture,
        author: PrincipalId,
        status: CandidateStatus,
        fact: EnvironmentHeadFact,
    ) {
        let key = environment_head_fact_key(&fact.environment, fact.epoch).expect("head key");
        let payload = environment_head_fact_payload(&fact).expect("head payload");
        self.insert(fixture, author, status, key, payload, fact.epoch.get());
    }

    fn insert(
        &mut self,
        fixture: &Fixture,
        author: PrincipalId,
        status: CandidateStatus,
        key: FactKey,
        payload: FactPayload,
        epoch: u64,
    ) {
        let content_hash = FactContentHash::for_payload(&payload);
        self.payloads.insert(content_hash.clone(), payload);
        self.candidates.push(FactCandidate::new(
            fixture.island.clone(),
            key,
            author,
            content_hash,
            FactKind::Unsupported,
            epoch,
            status,
        ));
    }
}

impl FactSource for MemoryFactSource {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        _session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        Ok(self
            .candidates
            .iter()
            .filter(|candidate| candidate.island() == island && pattern.matches(candidate.key()))
            .cloned()
            .collect())
    }

    fn read_payloads(
        &self,
        _island: &IslandId,
        candidates: &[FactCandidate],
        _session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        Ok(candidates
            .iter()
            .filter_map(|candidate| {
                self.payloads
                    .get(candidate.content_hash())
                    .cloned()
                    .map(|payload| (candidate.content_hash().clone(), payload))
            })
            .collect())
    }
}

struct SequencedFactSource {
    sources: Mutex<Vec<MemoryFactSource>>,
    active_payloads: Mutex<BTreeMap<FactContentHash, FactPayload>>,
}

impl SequencedFactSource {
    fn new(sources: impl IntoIterator<Item = MemoryFactSource>) -> Self {
        let mut sources = sources.into_iter().collect::<Vec<_>>();
        sources.reverse();
        Self {
            sources: Mutex::new(sources),
            active_payloads: Mutex::new(BTreeMap::new()),
        }
    }
}

impl FactSource for SequencedFactSource {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        let mut sources = self.sources.lock().expect("sequenced sources lock");
        let source = sources
            .pop()
            .or_else(|| sources.last().cloned())
            .unwrap_or_default();
        *self.active_payloads.lock().expect("active payloads lock") = source.payloads.clone();
        source.list_candidates(island, pattern, session)
    }

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        let source = MemoryFactSource {
            candidates: candidates.to_vec(),
            payloads: self
                .active_payloads
                .lock()
                .expect("active payloads lock")
                .clone(),
        };
        source.read_payloads(island, candidates, session)
    }
}

fn source_with_head(fixture: &Fixture, head: EnvironmentHeadFact) -> MemoryFactSource {
    let mut source = MemoryFactSource::default();
    source.insert_head(
        fixture,
        PrincipalId::new("sequenced-writer"),
        CandidateStatus::Verified,
        head,
    );
    source
}

struct Fixture {
    island: IslandId,
    session: BusSession,
}

impl Fixture {
    fn new() -> Self {
        let (_bus, authority) = mvp_bus::harness::InMemoryBus::new_with_authority();
        let island = IslandId::new("prod");
        let session =
            authority.grant_in(island.clone(), PrincipalId::new("reader"), Grant::empty());
        Self { island, session }
    }
}

fn environment(value: &str) -> EnvironmentId {
    EnvironmentId::parse(value).expect("environment id")
}

fn branch_id(value: &str) -> EnvironmentBranchId {
    EnvironmentBranchId::parse(value).expect("branch id")
}

fn epoch(value: u64) -> EnvironmentEpoch {
    EnvironmentEpoch::new(value).expect("environment epoch")
}

fn volume(value: &str) -> EnvironmentVolumeRef {
    EnvironmentVolumeRef::parse(value).expect("volume ref")
}

fn head(
    environment: &EnvironmentId,
    epoch_value: u64,
    head_id: &str,
    serving: &str,
    volumes: Vec<&str>,
    previous: Option<&EnvironmentHeadFact>,
) -> EnvironmentHeadFact {
    EnvironmentHeadFact {
        environment: environment.clone(),
        epoch: epoch(epoch_value),
        head_id: EnvironmentHeadId::parse(head_id).expect("head id"),
        source_command_id: EnvironmentCommandId::parse(format!("cmd-{head_id}"))
            .expect("command id"),
        serving_commit_id: ServingCommitId::new(serving),
        previous_head: previous.map(EnvironmentHeadFact::reference),
        volume_refs: volumes.into_iter().map(volume).collect(),
        source_branch_id: None,
    }
}

fn branch(
    source_environment: &EnvironmentId,
    branch_id: &EnvironmentBranchId,
) -> EnvironmentBranchFact {
    EnvironmentBranchFact {
        source_environment: source_environment.clone(),
        source_epoch: epoch(1),
        source_head_id: EnvironmentHeadId::parse("head-1").expect("head id"),
        branch_id: branch_id.clone(),
        branch_environment: environment("pr-1"),
        route_refs: vec![EnvironmentRouteRef::parse("route-1").expect("route ref")],
        forked_volume_refs: vec![volume("volume-branch")],
        visible_nodes: VisibleNodes::new([NodeId::new("node-1")]),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordedEnvironmentEvent {
    Head,
    Branch,
    PromoteDecision,
    RollbackDecision,
}

#[derive(Clone, Default)]
struct RecordingEnvironmentWriter {
    events: Arc<Mutex<Vec<RecordedEnvironmentEvent>>>,
}

impl RecordingEnvironmentWriter {
    fn events(&self) -> Vec<RecordedEnvironmentEvent> {
        self.events.lock().expect("events lock").clone()
    }

    fn record(&self, event: RecordedEnvironmentEvent) {
        self.events.lock().expect("events lock").push(event);
    }
}

impl EnvironmentFactWriter for RecordingEnvironmentWriter {
    fn write_head<'a>(
        &'a self,
        fact: EnvironmentHeadFact,
    ) -> Pin<Box<dyn Future<Output = crate::EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>>
    {
        Box::pin(async move {
            self.record(RecordedEnvironmentEvent::Head);
            Ok(WrittenEnvironmentFact {
                key: environment_head_fact_key(&fact.environment, fact.epoch)?,
            })
        })
    }

    fn write_branch<'a>(
        &'a self,
        fact: EnvironmentBranchFact,
    ) -> Pin<Box<dyn Future<Output = crate::EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>>
    {
        Box::pin(async move {
            self.record(RecordedEnvironmentEvent::Branch);
            Ok(WrittenEnvironmentFact {
                key: environment_branch_fact_key(&fact.source_environment, &fact.branch_id)?,
            })
        })
    }

    fn write_promote_decision<'a>(
        &'a self,
        fact: EnvironmentPromoteDecisionFact,
    ) -> Pin<Box<dyn Future<Output = crate::EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>>
    {
        Box::pin(async move {
            self.record(RecordedEnvironmentEvent::PromoteDecision);
            Ok(WrittenEnvironmentFact {
                key: environment_promote_decision_fact_key(&fact.environment, &fact.command_id)?,
            })
        })
    }

    fn write_rollback_decision<'a>(
        &'a self,
        fact: EnvironmentRollbackDecisionFact,
    ) -> Pin<Box<dyn Future<Output = crate::EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>>
    {
        Box::pin(async move {
            self.record(RecordedEnvironmentEvent::RollbackDecision);
            Ok(WrittenEnvironmentFact {
                key: environment_rollback_decision_fact_key(&fact.environment, &fact.command_id)?,
            })
        })
    }
}

#[derive(Clone, Default)]
struct RecordingForkParticipant {
    requests: Arc<Mutex<Vec<EnvironmentVolumeForkRequest>>>,
    forged_source: Option<EnvironmentVolumeRef>,
}

impl RecordingForkParticipant {
    fn with_forged_source(forged_source: EnvironmentVolumeRef) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            forged_source: Some(forged_source),
        }
    }

    fn requests(&self) -> Vec<EnvironmentVolumeForkRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl EnvironmentVolumeForkParticipant for RecordingForkParticipant {
    fn fork_volume<'a>(
        &'a self,
        request: EnvironmentVolumeForkRequest,
    ) -> Pin<
        Box<dyn Future<Output = crate::EnvironmentResult<EnvironmentVolumeForkReply>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.requests
                .lock()
                .expect("requests lock")
                .push(request.clone());
            let source_volume = self
                .forged_source
                .clone()
                .unwrap_or_else(|| request.source_volume.clone());
            Ok(EnvironmentVolumeForkReply {
                evidence: EnvironmentVolumeForkEvidence {
                    command_id: request.command_id,
                    source_environment: request.source_environment,
                    branch_environment: request.branch_environment,
                    source_volume,
                    forked_volume: volume("db-pr-123"),
                },
            })
        })
    }
}

#[derive(Clone, Default)]
struct RecordingServingWriter {
    writes: Arc<Mutex<Vec<ServingCommitId>>>,
}

impl RecordingServingWriter {
    fn writes(&self) -> Vec<ServingCommitId> {
        self.writes.lock().expect("serving writes lock").clone()
    }
}

impl ServingFactWriter for RecordingServingWriter {
    fn write_serving_commit<'a>(
        &'a self,
        commit: &'a ServingCommitPlan,
    ) -> Pin<Box<dyn Future<Output = RoutingResult<WrittenServingFact>> + Send + 'a>> {
        Box::pin(async move {
            self.writes
                .lock()
                .expect("serving writes lock")
                .push(commit.serving_commit_id.clone());
            Ok(WrittenServingFact::inserted(
                serving_commit_fact_key(&commit.serving_commit_id)?,
                FactContentHash::for_payload(&serving_commit_fact_payload(commit)?),
            ))
        })
    }
}

fn branch_head(
    branch_environment: &EnvironmentId,
    branch_id: &EnvironmentBranchId,
    volumes: Vec<&str>,
) -> EnvironmentHeadFact {
    EnvironmentHeadFact {
        environment: branch_environment.clone(),
        epoch: epoch(1),
        head_id: EnvironmentHeadId::parse("head-pr-123").expect("head id"),
        source_command_id: command_id("branch-1"),
        serving_commit_id: ServingCommitId::new("serving-pr-1"),
        previous_head: None,
        volume_refs: volumes.into_iter().map(volume).collect(),
        source_branch_id: Some(branch_id.clone()),
    }
}

fn command(value: &str) -> EnvironmentCommandId {
    command_id(value)
}

fn command_id(value: &str) -> EnvironmentCommandId {
    EnvironmentCommandId::parse(value).expect("command id")
}

fn visible_nodes() -> VisibleNodes {
    VisibleNodes::new([NodeId::new("node-1"), NodeId::new("node-2")])
}

fn serving_plan(
    serving_commit_id: &str,
    node_id: &str,
    address: &str,
    epoch_value: u64,
) -> ServingCommitPlan {
    let hostname = "app.example.test".to_string();
    ServingCommitPlan {
        serving_commit_id: ServingCommitId::new(serving_commit_id),
        route_commit_id: RouteCommitId::new(format!("route-{serving_commit_id}")),
        gateway_commit_id: GatewayCommitId::new(format!("gateway-{serving_commit_id}")),
        dns_commit_id: DnsCommitId::new(format!("dns-{serving_commit_id}")),
        route_id: RouteId::new("web"),
        hostnames: vec![hostname.clone()],
        active_backends: vec![BackendEndpoint {
            node_id: NodeId::new(node_id),
            address: address.to_string(),
        }],
        old_backends_to_drain: Vec::new(),
        dns_records: vec![DnsRecordFact {
            name: hostname,
            record_type: "AAAA".to_string(),
            value: "fd00::1".to_string(),
            ttl_seconds: 30,
        }],
        epoch: epoch_value,
    }
}

fn catch_up(fixture: &Fixture, commit: &ServingCommitPlan) -> ProjectionCatchUp {
    let mut state = ProjectionState::for_island(fixture.island.clone());
    state.gateway = Some(GatewayProjection {
        gateway_commit_id: commit.gateway_commit_id.to_string(),
        route_commit_id: commit.route_commit_id.to_string(),
        routes: vec![GatewayRouteProjection {
            route_id: commit.route_id.clone(),
            hostnames: commit.hostnames.clone(),
            backends: commit.active_backends.clone(),
            old_backends_to_drain: commit.old_backends_to_drain.clone(),
        }],
    });
    state.dns = Some(DnsProjection {
        dns_commit_id: commit.dns_commit_id.to_string(),
        records: commit
            .dns_records
            .clone()
            .into_iter()
            .map(Into::into)
            .collect(),
    });
    ProjectionCatchUp::from_report(
        commit,
        &ProjectionReport {
            state,
            sqlite_path: std::path::PathBuf::from("/tmp/mvp-environment-test.sqlite"),
            gateway_snapshot: Some(SnapshotWriteReport {
                path: std::path::PathBuf::from("/tmp/mvp-gateway.snapshot"),
                bytes_written: 1,
                revision: format!(
                    "gateway:{}:{}:acme:none",
                    commit.gateway_commit_id, commit.route_commit_id
                ),
            }),
            dns_snapshot: Some(SnapshotWriteReport {
                path: std::path::PathBuf::from("/tmp/mvp-dns.snapshot"),
                bytes_written: 1,
                revision: format!("dns:{}", commit.dns_commit_id),
            }),
            duration: std::time::Duration::from_millis(1),
        },
    )
    .expect("projection catch-up")
}
