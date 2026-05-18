use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use mvp_acme::{AcmeChallengeId, AcmeChallengeToken, AcmeHostname};
use mvp_bus::{
    BusSession, FactContentHash, FactKey, FactKeyPattern, FactPayload, Grant, IslandId,
    PrincipalId, harness::InMemoryBus,
};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_lease::{LeaseClaimed, LeaseFact, LeaseHolder, LeaseTimestamp};
use mvp_p2panda_facts::{PandaFactAuthor, PandaFactStore};
use mvp_projection::{
    CandidateStatus, FactCandidate, FactKind, FactSource, FactSourceResult, ProjectionFactPayload,
};

use crate::{
    AcmeClaimCommand, AcmeClearHttp01Command, AcmeCommandError, AcmeCommandExecutor,
    AcmeFactWriter, AcmeLeaseHandle, AcmePresentHttp01Command, PandaAcmeCommandAdapter,
};

const TOKEN: &str = "tokCommand0123456789abcd";

#[tokio::test]
async fn present_requires_current_lease_and_clear_preflights_both_writes() {
    let mut fixture =
        Fixture::with_grant(Grant::empty().with_fact_write(pattern("/facts/lease/>")));
    let claim_command = fixture.claim_command(10, 100);
    let claim = fixture
        .adapter
        .claim(&mut fixture.store, claim_command)
        .await
        .expect("claim lease");

    let present = fixture
        .adapter
        .present(
            &mut fixture.store,
            AcmePresentHttp01Command::new(
                fixture.challenge.clone(),
                claim.lease().clone(),
                "thumbprint0123456789",
                at(11),
            ),
        )
        .await;

    assert!(matches!(
        present,
        Err(AcmeCommandError::UnauthorizedFactWrite { .. })
    ));

    let before_clear = visible_candidate_count(&fixture.store, fixture.adapter.session())
        .expect("candidate count");
    let clear = fixture
        .adapter
        .clear(
            &mut fixture.store,
            AcmeClearHttp01Command::new(fixture.challenge.clone(), claim.lease().clone(), at(12)),
        )
        .await;

    assert!(matches!(
        clear,
        Err(AcmeCommandError::UnauthorizedFactWrite { .. })
    ));
    assert_eq!(
        visible_candidate_count(&fixture.store, fixture.adapter.session())
            .expect("candidate count"),
        before_clear
    );
}

#[tokio::test]
async fn expired_or_released_lease_can_be_reclaimed_at_higher_epoch() {
    let mut fixture = Fixture::with_grant(Grant::empty().with_fact_write(pattern("/facts/>")));

    let first_command = fixture.claim_command(10, 20);
    let first = fixture
        .adapter
        .claim(&mut fixture.store, first_command)
        .await
        .expect("first claim");
    let second_command = fixture.claim_command(21, 50);
    let second = fixture
        .adapter
        .claim(&mut fixture.store, second_command)
        .await
        .expect("claim after expiry");

    assert_eq!(first.lease().epoch().value(), 1);
    assert_eq!(second.lease().epoch().value(), 2);
    assert_eq!(second.visible_nodes().len(), 2);
}

#[tokio::test]
async fn active_lease_conflict_fails_before_mutation() {
    let mut fixture = Fixture::with_grant(Grant::empty().with_fact_write(pattern("/facts/>")));

    let first_command = fixture.claim_command(10, 100);
    fixture
        .adapter
        .claim(&mut fixture.store, first_command)
        .await
        .expect("first claim");
    let before = visible_candidate_count(&fixture.store, fixture.adapter.session())
        .expect("candidate count");
    let conflict_command = fixture.claim_command(20, 100);
    let conflict = fixture
        .adapter
        .claim(&mut fixture.store, conflict_command)
        .await;

    assert!(matches!(conflict, Err(AcmeCommandError::Conflict { .. })));
    assert_eq!(
        visible_candidate_count(&fixture.store, fixture.adapter.session())
            .expect("candidate count"),
        before
    );
}

#[test]
fn lease_replay_fails_closed_for_unreadable_and_malformed_candidates() {
    let fixture = ExecutionFixture::new();
    let claim = lease_claimed(fixture.challenge.lease_resource().clone(), 1, 10, 100);
    let payload = ProjectionFactPayload::LeaseClaimed(claim.clone());
    let unreadable = FakeSource::new([candidate_for_claim(
        &fixture,
        &claim,
        &payload,
        CandidateStatus::Unauthorized,
    )])
    .with_payload(&payload);

    let unreadable_result = fixture
        .executor
        .prepare_claim(&unreadable, fixture.claim_command(11, 100));

    assert!(matches!(
        unreadable_result,
        Err(AcmeCommandError::UnreadableLeaseCandidate { .. })
    ));

    let missing_payload = FakeSource::new([candidate_for_claim(
        &fixture,
        &claim,
        &payload,
        CandidateStatus::Verified,
    )]);

    let missing_result = fixture
        .executor
        .prepare_claim(&missing_payload, fixture.claim_command(11, 100));

    assert!(matches!(
        missing_result,
        Err(AcmeCommandError::MissingLeasePayload { .. })
    ));

    let wrong_claim = lease_claimed(fixture.challenge.lease_resource().clone(), 2, 10, 100);
    let wrong_payload = ProjectionFactPayload::LeaseClaimed(wrong_claim);
    let malformed = FakeSource::new([candidate_for_claim(
        &fixture,
        &claim,
        &wrong_payload,
        CandidateStatus::Verified,
    )])
    .with_payload(&wrong_payload);

    let malformed_result = fixture
        .executor
        .prepare_claim(&malformed, fixture.claim_command(11, 100));

    assert!(matches!(
        malformed_result,
        Err(AcmeCommandError::MalformedLeaseCandidate { .. })
    ));
}

#[tokio::test]
async fn stale_clear_is_rejected_before_mutation() {
    let mut fixture = Fixture::with_grant(Grant::empty().with_fact_write(pattern("/facts/>")));
    let first_command = fixture.claim_command(10, 20);
    let first = fixture
        .adapter
        .claim(&mut fixture.store, first_command)
        .await
        .expect("first claim");
    let second_command = fixture.claim_command(21, 100);
    fixture
        .adapter
        .claim(&mut fixture.store, second_command)
        .await
        .expect("second claim after expiry");
    let before = visible_candidate_count(&fixture.store, fixture.adapter.session())
        .expect("candidate count");

    let stale_clear = fixture
        .adapter
        .clear(
            &mut fixture.store,
            AcmeClearHttp01Command::new(fixture.challenge.clone(), first.lease().clone(), at(22)),
        )
        .await;

    assert!(matches!(stale_clear, Err(AcmeCommandError::StaleLease)));
    assert_eq!(
        visible_candidate_count(&fixture.store, fixture.adapter.session())
            .expect("candidate count"),
        before
    );
}

#[tokio::test]
async fn clear_release_preflight_failure_happens_before_any_write() {
    let fixture = ExecutionFixture::new();
    let claim = lease_claimed(fixture.challenge.lease_resource().clone(), 1, 10, 100);
    let claim_hash = LeaseFact::Claimed(claim.clone()).content_hash();
    let payload = ProjectionFactPayload::LeaseClaimed(claim.clone());
    let source = FakeSource::new([candidate_for_claim(
        &fixture,
        &claim,
        &payload,
        CandidateStatus::Verified,
    )])
    .with_payload(&payload);
    let lease = AcmeLeaseHandle::new(
        fixture.challenge.clone(),
        LeaseHolder::new(fixture.session.principal().as_str()),
        claim.epoch(),
        claim_hash,
    );
    let prepared = fixture
        .executor
        .prepare_clear(
            &source,
            AcmeClearHttp01Command::new(fixture.challenge.clone(), lease, at(11)),
        )
        .expect("clear prepares");
    let release_key = prepared.release.key.clone();
    let mut writer = RecordingWriter::deny(release_key.clone());

    let result = async {
        writer.preflight(&fixture.session, &prepared.release.key)?;
        writer.preflight(&fixture.session, &prepared.clear.key)?;
        writer
            .write(
                &fixture.session,
                prepared.release.key,
                prepared.release.payload,
            )
            .await?;
        writer
            .write(&fixture.session, prepared.clear.key, prepared.clear.payload)
            .await
    }
    .await;

    assert!(matches!(
        result,
        Err(AcmeCommandError::UnauthorizedFactWrite { .. })
    ));
    assert_eq!(writer.preflighted.into_inner(), vec![release_key]);
    assert!(writer.writes.is_empty());
}

struct Fixture {
    challenge: AcmeChallengeId,
    store: PandaFactStore,
    adapter: PandaAcmeCommandAdapter,
}

impl Fixture {
    fn with_grant(grant: Grant) -> Self {
        let island = IslandId::new("prod");
        let (bus, authority) = InMemoryBus::new_with_authority();
        let challenge = challenge();
        let session = authority.grant_in(
            island,
            PrincipalId::new("issuer"),
            grant.with_fact_read(pattern("/facts/>")),
        );
        let visible_nodes = VisibleNodes::new([NodeId::new("node-a"), NodeId::new("node-b")]);
        let store = PandaFactStore::new(Arc::new(bus));
        let author = PandaFactAuthor::new(session.principal().clone());
        let adapter = PandaAcmeCommandAdapter::new(session, author, visible_nodes);
        Self {
            challenge,
            store,
            adapter,
        }
    }

    fn claim_command(&self, acquired_at: u64, expires_at: u64) -> AcmeClaimCommand {
        AcmeClaimCommand::new(self.challenge.clone(), at(acquired_at), at(expires_at))
    }
}

fn challenge() -> AcmeChallengeId {
    AcmeChallengeId::new(
        AcmeHostname::parse("example.test").expect("hostname parses"),
        AcmeChallengeToken::parse(TOKEN).expect("token parses"),
    )
}

fn pattern(value: &str) -> FactKeyPattern {
    FactKeyPattern::parse(value).expect("fact pattern parses")
}

fn at(value: u64) -> LeaseTimestamp {
    LeaseTimestamp::from_secs(value)
}

struct ExecutionFixture {
    session: BusSession,
    challenge: AcmeChallengeId,
    executor: AcmeCommandExecutor,
}

impl ExecutionFixture {
    fn new() -> Self {
        let island = IslandId::new("prod");
        let (bus, authority) = InMemoryBus::new_with_authority();
        let session = authority.grant_in(
            island,
            PrincipalId::new("issuer"),
            Grant::empty().with_fact_read(pattern("/facts/>")),
        );
        let challenge = challenge();
        let executor = AcmeCommandExecutor::new(
            session.clone(),
            VisibleNodes::new([NodeId::new("node-a"), NodeId::new("node-b")]),
        );
        drop(bus);
        Self {
            session,
            challenge,
            executor,
        }
    }

    fn claim_command(&self, acquired_at: u64, expires_at: u64) -> AcmeClaimCommand {
        AcmeClaimCommand::new(self.challenge.clone(), at(acquired_at), at(expires_at))
    }
}

#[derive(Default)]
struct FakeSource {
    candidates: Vec<FactCandidate>,
    payloads: BTreeMap<FactContentHash, FactPayload>,
}

impl FakeSource {
    fn new(candidates: impl IntoIterator<Item = FactCandidate>) -> Self {
        Self {
            candidates: candidates.into_iter().collect(),
            payloads: BTreeMap::new(),
        }
    }

    fn with_payload(mut self, payload: &ProjectionFactPayload) -> Self {
        let bytes = payload.to_fact_bytes().expect("payload serializes");
        let payload = FactPayload::copy_from_slice(&bytes);
        self.payloads
            .insert(FactContentHash::for_payload(&payload), payload);
        self
    }
}

impl FactSource for FakeSource {
    fn list_candidates(
        &self,
        _island: &IslandId,
        _pattern: &FactKeyPattern,
        _session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        Ok(self.candidates.clone())
    }

    fn read_payloads(
        &self,
        _island: &IslandId,
        _candidates: &[FactCandidate],
        _session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        Ok(self.payloads.clone())
    }
}

struct RecordingWriter {
    denied: FactKey,
    preflighted: RefCell<Vec<FactKey>>,
    writes: Vec<FactKey>,
}

impl RecordingWriter {
    fn deny(denied: FactKey) -> Self {
        Self {
            denied,
            preflighted: RefCell::new(Vec::new()),
            writes: Vec::new(),
        }
    }
}

impl AcmeFactWriter for RecordingWriter {
    fn preflight(&self, session: &BusSession, key: &FactKey) -> Result<(), AcmeCommandError> {
        self.preflighted.borrow_mut().push(key.clone());
        if key == &self.denied {
            return Err(AcmeCommandError::UnauthorizedFactWrite {
                principal: session.principal().clone(),
                key: key.clone(),
            });
        }
        Ok(())
    }

    async fn write(
        &mut self,
        _session: &BusSession,
        key: FactKey,
        _payload: ProjectionFactPayload,
    ) -> Result<(), AcmeCommandError> {
        self.writes.push(key);
        Ok(())
    }
}

fn lease_claimed(
    resource: mvp_lease::LeaseResource,
    epoch: u64,
    acquired_at: u64,
    expires_at: u64,
) -> LeaseClaimed {
    LeaseClaimed::new(
        resource,
        LeaseHolder::new("issuer"),
        mvp_lease::LeaseEpoch::from_u64(epoch).expect("epoch valid"),
        at(acquired_at),
        at(expires_at),
    )
}

fn candidate_for_claim(
    fixture: &ExecutionFixture,
    claim: &LeaseClaimed,
    payload: &ProjectionFactPayload,
    status: CandidateStatus,
) -> FactCandidate {
    let bytes = payload.to_fact_bytes().expect("payload serializes");
    FactCandidate::new(
        fixture.session.island().clone(),
        FactKey::parse(fixture.challenge.lease_claimed_fact_key(claim.epoch()))
            .expect("lease claim key parses"),
        fixture.session.principal().clone(),
        FactContentHash::for_payload(&FactPayload::copy_from_slice(&bytes)),
        FactKind::LeaseClaimed,
        claim.epoch().value(),
        status,
    )
}

fn visible_candidate_count(
    store: &PandaFactStore,
    session: &mvp_bus::BusSession,
) -> Result<usize, String> {
    store
        .list_candidates(session.island(), &pattern("/facts/>"), session)
        .map(|candidates| candidates.len())
        .map_err(|error| error.to_string())
}
