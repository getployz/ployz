use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mvp_bus::{
    BusSession, FactContentHash, FactKey, FactKeyPattern, FactPayload, Grant, IslandId, PrincipalId,
};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_projection::{
    BackendEndpoint, CandidateStatus, DnsProjection, DnsRecordFact, FactCandidate, FactKind,
    FactSource, FactSourceResult, GatewayProjection, GatewayRouteProjection, ProjectionReport,
    ProjectionState, RouteId, SnapshotWriteReport,
};
use mvp_routing::{
    DnsCommitId, GatewayCommitId, ProjectionCatchUp, RouteCommitId, RoutingResult, ServingCommitId,
    ServingCommitPlan, ServingFactWriter, WrittenServingFact, serving_commit_fact_key,
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
    let pending = command
        .begin(PromoteEnvironmentRequest {
            command_id: command_id("promote-1"),
            environment: env,
            expected_environment_epoch: epoch(1),
            branch_environment: branch_env,
            expected_branch_epoch: epoch(1),
            serving_commit: plan.clone(),
            visible_nodes: visible_nodes(),
        })
        .await
        .expect("promote begin");
    let no_catchup = command
        .finalize(pending.clone(), None)
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
    assert_eq!(serving.writes(), vec![plan.serving_commit_id.clone()]);

    let complete = command
        .finalize(pending, Some(catch_up(&fixture, &plan)))
        .await
        .expect("promote finalize");

    assert!(
        matches!(complete, EnvironmentCommandResult::Complete { head } if head.volume_refs == vec![volume("db-pr-123")])
    );
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

    let pending = command
        .begin(RollbackEnvironmentRequest {
            command_id: command_id("rollback-1"),
            environment: env,
            expected_environment_epoch: epoch(2),
            serving_commit: plan.clone(),
            visible_nodes: visible_nodes(),
        })
        .await
        .expect("rollback begin");
    let complete = command
        .finalize(pending, Some(catch_up(&fixture, &plan)))
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

#[derive(Default)]
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

#[derive(Default)]
struct RecordingEnvironmentWriter {
    events: Arc<Mutex<Vec<RecordedEnvironmentEvent>>>,
}

impl RecordingEnvironmentWriter {
    fn events(&self) -> Vec<RecordedEnvironmentEvent> {
        self.events.lock().expect("events lock").clone()
    }
}

impl EnvironmentFactWriter for RecordingEnvironmentWriter {
    fn write_head<'a>(
        &'a self,
        fact: EnvironmentHeadFact,
    ) -> Pin<Box<dyn Future<Output = crate::EnvironmentResult<WrittenEnvironmentFact>> + Send + 'a>>
    {
        Box::pin(async move {
            self.events
                .lock()
                .expect("events lock")
                .push(RecordedEnvironmentEvent::Head);
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
            self.events
                .lock()
                .expect("events lock")
                .push(RecordedEnvironmentEvent::Branch);
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
            self.events
                .lock()
                .expect("events lock")
                .push(RecordedEnvironmentEvent::PromoteDecision);
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
            self.events
                .lock()
                .expect("events lock")
                .push(RecordedEnvironmentEvent::RollbackDecision);
            Ok(WrittenEnvironmentFact {
                key: environment_rollback_decision_fact_key(&fact.environment, &fact.command_id)?,
            })
        })
    }
}

#[derive(Default)]
struct RecordingForkParticipant {
    requests: Arc<Mutex<Vec<EnvironmentVolumeForkRequest>>>,
    forged_source: Option<EnvironmentVolumeRef>,
}

impl RecordingForkParticipant {
    fn with_forged_source(forged_source: EnvironmentVolumeRef) -> Self {
        Self {
            requests: Arc::default(),
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

#[derive(Default)]
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
                FactContentHash::new(format!("hash-{}", commit.serving_commit_id)),
            ))
        })
    }
}

fn command(value: &str) -> EnvironmentCommandId {
    command_id(value)
}

fn command_id(value: &str) -> EnvironmentCommandId {
    EnvironmentCommandId::parse(value).expect("command id")
}

fn branch_head(
    branch_environment: &EnvironmentId,
    branch_id: &EnvironmentBranchId,
    volumes: Vec<&str>,
) -> EnvironmentHeadFact {
    let mut head = head(
        branch_environment,
        1,
        "head-pr-1",
        "serving-pr-1",
        volumes,
        None,
    );
    head.source_branch_id = Some(branch_id.clone());
    head
}

fn visible_nodes() -> VisibleNodes {
    VisibleNodes::new([NodeId::new("node-1"), NodeId::new("node-2")])
}

fn serving_plan(id: &str, node: &str, address: &str, epoch: u64) -> ServingCommitPlan {
    ServingCommitPlan {
        serving_commit_id: ServingCommitId::new(id),
        route_commit_id: RouteCommitId::new(format!("{id}-route")),
        gateway_commit_id: GatewayCommitId::new(format!("{id}-gateway")),
        dns_commit_id: DnsCommitId::new(format!("{id}-dns")),
        route_id: RouteId::new("web"),
        hostnames: vec!["app.example.test".to_string()],
        active_backends: vec![BackendEndpoint {
            node_id: NodeId::new(node),
            address: address.to_string(),
        }],
        old_backends_to_drain: Vec::new(),
        dns_records: vec![DnsRecordFact {
            name: "app.example.test".to_string(),
            record_type: "AAAA".to_string(),
            value: "fd00::1".to_string(),
            ttl_seconds: 30,
        }],
        epoch,
    }
}

fn catch_up(fixture: &Fixture, plan: &ServingCommitPlan) -> ProjectionCatchUp {
    let mut state = ProjectionState::for_island(fixture.island.clone());
    state.gateway = Some(GatewayProjection {
        gateway_commit_id: plan.gateway_commit_id.to_string(),
        route_commit_id: plan.route_commit_id.to_string(),
        routes: vec![GatewayRouteProjection {
            route_id: plan.route_id.clone(),
            hostnames: plan.hostnames.clone(),
            backends: plan.active_backends.clone(),
            old_backends_to_drain: plan.old_backends_to_drain.clone(),
        }],
    });
    state.dns = Some(DnsProjection {
        dns_commit_id: plan.dns_commit_id.to_string(),
        records: plan.dns_records.iter().cloned().map(Into::into).collect(),
    });
    ProjectionCatchUp::from_report(
        plan,
        &ProjectionReport {
            state,
            sqlite_path: PathBuf::from("projection.sqlite"),
            gateway_snapshot: Some(SnapshotWriteReport {
                path: PathBuf::from("gateway.snapshot"),
                bytes_written: 1,
                revision: format!(
                    "gateway:{}:{}",
                    plan.gateway_commit_id, plan.route_commit_id
                ),
            }),
            dns_snapshot: Some(SnapshotWriteReport {
                path: PathBuf::from("dns.snapshot"),
                bytes_written: 1,
                revision: format!("dns:{}", plan.dns_commit_id),
            }),
            duration: Duration::from_millis(1),
        },
    )
    .expect("projection catchup")
}
