use std::collections::BTreeMap;

use mvp_bus::{
    BusSession, FactContentHash, FactKey, FactKeyPattern, FactPayload, Grant, IslandId, PrincipalId,
};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_projection::{CandidateStatus, FactCandidate, FactKind, FactSource, FactSourceResult};
use mvp_routing::ServingCommitId;

use crate::{
    EnvironmentBranchFact, EnvironmentBranchId, EnvironmentCommandId, EnvironmentEpoch,
    EnvironmentError, EnvironmentHeadFact, EnvironmentHeadId, EnvironmentId, EnvironmentRouteRef,
    EnvironmentVolumeRef, current_environment_head, decode_environment_head_fact,
    environment_branch_fact_key, environment_branch_fact_payload, environment_head_fact_key,
    environment_head_fact_payload, read_environment_heads, require_expected_environment_epoch,
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
