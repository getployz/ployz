use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use mvp_bus::{
    BusSession, FactContentHash, FactKey, FactKeyPattern, FactPayload, Grant, IslandId,
    PrincipalId, harness::InMemoryBus,
};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_lease::{LeaseClaimed, LeaseFact, LeaseHolder, LeaseTimestamp};
use mvp_projection::{
    CandidateStatus, FactCandidate, FactKind, FactSource, FactSourceResult, ProjectionFactPayload,
};

use crate::{
    VolumeError, VolumeFactWriteOutcome, VolumeFactWriter, VolumeId, VolumeNamespace,
    VolumeOwnershipEvidence, VolumeOwnershipFact, VolumeOwnershipLeaseEvidence,
    VolumeParticipantClient, VolumeReceiveReply, VolumeReceiveRequest, VolumeResult,
    VolumeSnapshotGuid, VolumeSnapshotId, VolumeSnapshotReply, VolumeSnapshotRequest,
    VolumeSourceCleanup, VolumeTransferCommand, VolumeTransferExecutor, VolumeTransferId,
    VolumeTransferTimeouts, current_volume_ownership, volume_ownership_fact_key,
};

#[test]
fn ownership_key_payload_and_reducer_are_deterministic() {
    let session = session("prod", "operator");
    let namespace = namespace("prod");
    let volume = volume("db");
    let lower = ownership_fact(&namespace, &volume, 1, "node-a", "transfer-a");
    let higher = ownership_fact(&namespace, &volume, 2, "node-b", "transfer-b");
    let store = FakeStore::new()
        .with_volume(&session, &lower, CandidateStatus::Verified)
        .with_volume(&session, &higher, CandidateStatus::Conflict);

    let current =
        current_volume_ownership(&store, &session, &namespace, &volume).expect("ownership reduces");

    let current = current.current.expect("current owner");
    assert_eq!(current.fact.owner, NodeId::new("node-b"));
    let VolumeOwnershipEvidence::Transferred {
        lease_claim_hash, ..
    } = current.fact.evidence
    else {
        panic!("expected transferred ownership evidence");
    };
    assert_eq!(lease_claim_hash, lease_hash("prod", "db", 1));
    assert_eq!(current.fact.epoch, 2);
    assert_eq!(
        current_volume_ownership(&store, &session, &namespace, &volume)
            .expect("ownership reduces")
            .superseded
            .len(),
        1
    );
}

#[test]
fn identifiers_reject_bad_path_bytes() {
    assert!(matches!(
        VolumeNamespace::parse(""),
        Err(VolumeError::InvalidIdentifier { .. })
    ));
    assert!(matches!(
        VolumeId::parse("bad/name"),
        Err(VolumeError::InvalidIdentifier { .. })
    ));
    assert!(matches!(
        VolumeTransferId::parse("bad name"),
        Err(VolumeError::InvalidIdentifier { .. })
    ));
    assert!(matches!(
        VolumeSnapshotId::parse("bad\nname"),
        Err(VolumeError::InvalidIdentifier { .. })
    ));
}

#[tokio::test]
async fn transfer_writes_lease_before_participant_rpc_and_then_owner() {
    let session = session("prod", "operator");
    let namespace = namespace("prod");
    let volume = volume("db");
    let initial = ownership_fact(&namespace, &volume, 1, "node-a", "initial");
    let events = TestEvents::new();
    let mut store = FakeStore::new().record_events(events.clone()).with_volume(
        &session,
        &initial,
        CandidateStatus::Verified,
    );
    let client = RecordingClient::valid().record_events(events.clone());
    let executor = executor(session.clone(), client.clone());

    let result = executor
        .transfer(
            &mut store,
            command(
                namespace.clone(),
                volume.clone(),
                "transfer-1",
                "node-b",
                10,
            ),
        )
        .await
        .expect("transfer succeeds");

    assert_eq!(result.old_owner, NodeId::new("node-a"));
    assert_eq!(result.new_owner, NodeId::new("node-b"));
    assert!(matches!(
        result.source_cleanup,
        VolumeSourceCleanup::Deferred
    ));
    let [first, second] = store.writes.as_slice() else {
        panic!("expected lease and ownership writes");
    };
    assert!(
        first
            .key
            .as_str()
            .starts_with("/facts/lease/volume.prod.db/claimed/")
    );
    assert!(!first.payload.is_empty());
    assert_eq!(second.key.as_str(), "/facts/volume/prod/db/ownership/2");
    assert!(!second.payload.is_empty());
    assert_eq!(client.snapshot_count(), 1);
    assert_eq!(client.receive_count(), 1);
    assert_eq!(
        events.snapshot(),
        vec![
            TestEvent::LeaseWrite,
            TestEvent::SnapshotRpc,
            TestEvent::ReceiveRpc,
            TestEvent::OwnershipWrite,
        ]
    );

    let recovered =
        current_volume_ownership(&store, &session, &namespace, &volume).expect("recover ownership");
    assert_eq!(
        recovered.current.expect("current owner").fact.owner,
        NodeId::new("node-b")
    );
}

#[tokio::test]
async fn active_lease_conflict_fails_before_participant_rpc() {
    let session = session("prod", "operator");
    let namespace = namespace("prod");
    let volume = volume("db");
    let initial = ownership_fact(&namespace, &volume, 1, "node-a", "initial");
    let mut store = FakeStore::new()
        .with_volume(&session, &initial, CandidateStatus::Verified)
        .with_lease_claim(&session, &namespace, &volume, "other", 1, (10, 100));
    let client = RecordingClient::valid();
    let executor = executor(session, client.clone());

    let error = executor
        .transfer(
            &mut store,
            command(namespace, volume, "transfer-1", "node-b", 20),
        )
        .await
        .expect_err("lease conflict");

    assert!(matches!(error, VolumeError::LeaseConflict { .. }));
    assert!(store.writes.is_empty());
    assert_eq!(client.snapshot_count(), 0);
    assert_eq!(client.receive_count(), 0);
}

#[tokio::test]
async fn ownership_preflight_fails_before_lease_or_participant_rpc() {
    let session = session("prod", "operator");
    let namespace = namespace("prod");
    let volume = volume("db");
    let initial = ownership_fact(&namespace, &volume, 1, "node-a", "initial");
    let mut store = FakeStore::new()
        .with_volume(&session, &initial, CandidateStatus::Verified)
        .deny_ownership_preflight();
    let client = RecordingClient::valid();
    let executor = executor(session, client.clone());

    let error = executor
        .transfer(
            &mut store,
            command(namespace, volume, "transfer-1", "node-b", 10),
        )
        .await
        .expect_err("ownership preflight rejects");

    assert!(matches!(error, VolumeError::UnauthorizedFactWrite { .. }));
    assert!(store.writes.is_empty());
    assert_eq!(client.snapshot_count(), 0);
    assert_eq!(client.receive_count(), 0);
}

#[tokio::test]
async fn lease_expiry_during_participant_work_fails_before_ownership_commit() {
    let session = session("prod", "operator");
    let namespace = namespace("prod");
    let volume = volume("db");
    let initial = ownership_fact(&namespace, &volume, 1, "node-a", "initial");
    let mut store = FakeStore::new().with_volume(&session, &initial, CandidateStatus::Verified);
    let client = RecordingClient::valid();
    let executor = executor(session, client.clone());

    let error = executor
        .transfer(
            &mut store,
            command_with_window(namespace, volume, "transfer-1", "node-b", 10, 101, 100),
        )
        .await
        .expect_err("lease expires before commit");

    assert!(matches!(error, VolumeError::StaleLease { .. }));
    assert_eq!(store.writes.len(), 1);
    assert_eq!(client.snapshot_count(), 1);
    assert_eq!(client.receive_count(), 1);
}

#[tokio::test]
async fn ownership_race_during_participant_work_fails_before_ownership_commit() {
    let session = session("prod", "operator");
    let namespace = namespace("prod");
    let volume = volume("db");
    let initial = ownership_fact(&namespace, &volume, 1, "node-a", "initial");
    let raced = ownership_fact(&namespace, &volume, 2, "node-c", "raced");
    let mut store = FakeStore::new()
        .with_volume(&session, &initial, CandidateStatus::Verified)
        .inject_owner_after_lease(raced);
    let client = RecordingClient::valid();
    let executor = executor(session, client.clone());

    let error = executor
        .transfer(
            &mut store,
            command(namespace, volume, "transfer-1", "node-b", 10),
        )
        .await
        .expect_err("ownership changes before commit");

    assert!(matches!(error, VolumeError::StaleOwnership { .. }));
    assert_eq!(store.writes.len(), 1);
    assert_eq!(client.snapshot_count(), 1);
    assert_eq!(client.receive_count(), 1);
}

#[tokio::test]
async fn ownership_write_conflict_is_a_foreground_failure() {
    let session = session("prod", "operator");
    let namespace = namespace("prod");
    let volume = volume("db");
    let initial = ownership_fact(&namespace, &volume, 1, "node-a", "initial");
    let mut store = FakeStore::new()
        .with_volume(&session, &initial, CandidateStatus::Verified)
        .conflict_ownership_write();
    let client = RecordingClient::valid();
    let executor = executor(session, client.clone());

    let error = executor
        .transfer(
            &mut store,
            command(namespace, volume, "transfer-1", "node-b", 10),
        )
        .await
        .expect_err("ownership write conflict");

    assert!(matches!(error, VolumeError::FactConflict { .. }));
    assert_eq!(store.writes.len(), 1);
    assert_eq!(client.snapshot_count(), 1);
    assert_eq!(client.receive_count(), 1);
}

#[tokio::test]
async fn forged_receive_evidence_rejects_before_ownership_commit() {
    let session = session("prod", "operator");
    let namespace = namespace("prod");
    let volume = volume("db");
    let initial = ownership_fact(&namespace, &volume, 1, "node-a", "initial");
    for forged_field in ForgedReceiveField::ALL {
        let mut store = FakeStore::new().with_volume(&session, &initial, CandidateStatus::Verified);
        let client = RecordingClient::valid().forge_receive(forged_field);
        let executor = executor(session.clone(), client);

        let error = executor
            .transfer(
                &mut store,
                command(
                    namespace.clone(),
                    volume.clone(),
                    "transfer-1",
                    "node-b",
                    10,
                ),
            )
            .await
            .expect_err("forged receive");

        assert!(
            matches!(
                error,
                VolumeError::ReceiveEvidenceMismatch { .. }
                    | VolumeError::ReceiveTargetMismatch { .. }
                    | VolumeError::ReceivedSnapshotMismatch { .. }
            ),
            "unexpected error for {forged_field:?}: {error:?}"
        );
        assert_eq!(store.writes.len(), 1);
    }
}

fn executor(
    client_session: BusSession,
    client: RecordingClient,
) -> VolumeTransferExecutor<RecordingClient> {
    VolumeTransferExecutor::new(
        client_session,
        client,
        VisibleNodes::new([NodeId::new("node-a"), NodeId::new("node-b")]),
        VolumeTransferTimeouts::new(Duration::from_millis(50)),
    )
}

fn command(
    namespace: VolumeNamespace,
    volume: VolumeId,
    transfer_id: &str,
    target: &str,
    observed_at: u64,
) -> VolumeTransferCommand {
    VolumeTransferCommand::new(
        namespace,
        volume,
        transfer(transfer_id),
        NodeId::new(target),
        LeaseTimestamp::from_secs(observed_at),
        LeaseTimestamp::from_secs(observed_at + 1),
        LeaseTimestamp::from_secs(observed_at + 90),
    )
}

fn command_with_window(
    namespace: VolumeNamespace,
    volume: VolumeId,
    transfer_id: &str,
    target: &str,
    observed_at: u64,
    commit_observed_at: u64,
    expires_at: u64,
) -> VolumeTransferCommand {
    VolumeTransferCommand::new(
        namespace,
        volume,
        transfer(transfer_id),
        NodeId::new(target),
        LeaseTimestamp::from_secs(observed_at),
        LeaseTimestamp::from_secs(commit_observed_at),
        LeaseTimestamp::from_secs(expires_at),
    )
}

fn ownership_fact(
    namespace: &VolumeNamespace,
    volume: &VolumeId,
    epoch: u64,
    owner: &str,
    transfer_id: &str,
) -> VolumeOwnershipFact {
    VolumeOwnershipFact {
        namespace: namespace.clone(),
        volume: volume.clone(),
        epoch,
        owner: NodeId::new(owner),
        evidence: VolumeOwnershipEvidence::transferred(
            NodeId::new("node-a"),
            transfer(transfer_id),
            snapshot_id("snap-1"),
            snapshot_guid("guid-1"),
            1024,
            VolumeOwnershipLeaseEvidence::new(
                LeaseHolder::new("operator"),
                mvp_lease::LeaseEpoch::from_u64(1).expect("epoch"),
                lease_hash(namespace.as_str(), volume.as_str(), 1),
            ),
        ),
        visible_nodes: VisibleNodes::new([NodeId::new("node-a"), NodeId::new("node-b")]),
    }
}

fn lease_hash(namespace: &str, volume: &str, epoch: u64) -> mvp_lease::LeaseContentHash {
    let claim = LeaseClaimed::new(
        VolumeId::parse(volume)
            .expect("volume")
            .lease_resource(&VolumeNamespace::parse(namespace).expect("namespace")),
        LeaseHolder::new("operator"),
        mvp_lease::LeaseEpoch::from_u64(epoch).expect("epoch"),
        LeaseTimestamp::from_secs(1),
        LeaseTimestamp::from_secs(90),
    );
    LeaseFact::Claimed(claim).content_hash()
}

fn session(island: &str, principal: &str) -> BusSession {
    let (_raw_bus, authority) = InMemoryBus::new_with_authority();
    authority.grant_in(
        IslandId::new(island),
        PrincipalId::new(principal),
        Grant::allow_all(),
    )
}

fn namespace(value: &str) -> VolumeNamespace {
    VolumeNamespace::parse(value).expect("namespace")
}

fn volume(value: &str) -> VolumeId {
    VolumeId::parse(value).expect("volume")
}

fn transfer(value: &str) -> VolumeTransferId {
    VolumeTransferId::parse(value).expect("transfer")
}

fn snapshot_id(value: &str) -> VolumeSnapshotId {
    VolumeSnapshotId::parse(value).expect("snapshot")
}

fn snapshot_guid(value: &str) -> VolumeSnapshotGuid {
    VolumeSnapshotGuid::parse(value).expect("guid")
}

#[derive(Default)]
struct FakeStore {
    candidates: Vec<FactCandidate>,
    payloads: BTreeMap<FactContentHash, FactPayload>,
    writes: Vec<RecordedWrite>,
    events: Option<TestEvents>,
    deny_ownership_preflight: bool,
    conflict_ownership_write: bool,
    inject_owner_after_lease: Option<VolumeOwnershipFact>,
}

impl FakeStore {
    fn new() -> Self {
        Self::default()
    }

    fn record_events(mut self, events: TestEvents) -> Self {
        self.events = Some(events);
        self
    }

    fn deny_ownership_preflight(mut self) -> Self {
        self.deny_ownership_preflight = true;
        self
    }

    fn conflict_ownership_write(mut self) -> Self {
        self.conflict_ownership_write = true;
        self
    }

    fn inject_owner_after_lease(mut self, fact: VolumeOwnershipFact) -> Self {
        self.inject_owner_after_lease = Some(fact);
        self
    }

    fn with_volume(
        mut self,
        session: &BusSession,
        fact: &VolumeOwnershipFact,
        status: CandidateStatus,
    ) -> Self {
        let key = volume_ownership_fact_key(&fact.namespace, &fact.volume, fact.epoch)
            .expect("volume key");
        let payload = fact.to_payload().expect("volume payload");
        self.insert(
            session,
            key,
            payload,
            FactKind::Unsupported,
            fact.epoch,
            status,
        );
        self
    }

    fn with_lease_claim(
        mut self,
        session: &BusSession,
        namespace: &VolumeNamespace,
        volume: &VolumeId,
        holder: &str,
        epoch: u64,
        window: (u64, u64),
    ) -> Self {
        let epoch = mvp_lease::LeaseEpoch::from_u64(epoch).expect("epoch");
        let claim = LeaseClaimed::new(
            volume.lease_resource(namespace),
            LeaseHolder::new(holder),
            epoch,
            LeaseTimestamp::from_secs(window.0),
            LeaseTimestamp::from_secs(window.1),
        );
        let key = FactKey::parse(format!(
            "/facts/lease/{}/claimed/{epoch}",
            volume.lease_resource(namespace)
        ))
        .expect("lease key");
        let payload = ProjectionFactPayload::LeaseClaimed(claim)
            .to_fact_bytes()
            .map(FactPayload::from)
            .expect("lease payload");
        self.insert(
            session,
            key,
            payload,
            FactKind::LeaseClaimed,
            epoch.value(),
            CandidateStatus::Verified,
        );
        self
    }

    fn insert(
        &mut self,
        session: &BusSession,
        key: FactKey,
        payload: FactPayload,
        kind: FactKind,
        epoch: u64,
        status: CandidateStatus,
    ) {
        let content_hash = FactContentHash::for_payload(&payload);
        self.payloads.insert(content_hash.clone(), payload);
        self.candidates.push(FactCandidate::new(
            session.island().clone(),
            key,
            session.principal().clone(),
            content_hash,
            kind,
            epoch,
            status,
        ));
    }
}

impl FactSource for FakeStore {
    fn list_candidates(
        &self,
        _island: &IslandId,
        pattern: &FactKeyPattern,
        _session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        Ok(self
            .candidates
            .iter()
            .filter(|candidate| pattern.matches(candidate.key()))
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

struct RecordedWrite {
    key: FactKey,
    payload: FactPayload,
}

impl VolumeFactWriter for FakeStore {
    fn preflight(&self, session: &BusSession, key: &FactKey) -> VolumeResult<()> {
        if self.deny_ownership_preflight && key.as_str().starts_with("/facts/volume/") {
            return Err(VolumeError::UnauthorizedFactWrite {
                principal: session.principal().clone(),
                key: key.clone(),
            });
        }
        Ok(())
    }

    fn write_fact<'a>(
        &'a mut self,
        session: &'a BusSession,
        key: FactKey,
        payload: FactPayload,
    ) -> Pin<Box<dyn Future<Output = VolumeResult<VolumeFactWriteOutcome>> + Send + 'a>> {
        Box::pin(async move {
            if self.conflict_ownership_write && key.as_str().starts_with("/facts/volume/") {
                return Ok(VolumeFactWriteOutcome::Conflict);
            }
            let is_lease = key.as_str().starts_with("/facts/lease/");
            let kind = if is_lease {
                FactKind::LeaseClaimed
            } else {
                FactKind::Unsupported
            };
            let epoch = key
                .segments()
                .next_back()
                .and_then(|segment| segment.parse::<u64>().ok())
                .unwrap_or(0);
            self.insert(
                session,
                key.clone(),
                payload.clone(),
                kind,
                epoch,
                CandidateStatus::Verified,
            );
            self.writes.push(RecordedWrite { key, payload });
            if is_lease {
                if let Some(events) = &self.events {
                    events.push(TestEvent::LeaseWrite);
                }
                if let Some(fact) = self.inject_owner_after_lease.take() {
                    let key = volume_ownership_fact_key(&fact.namespace, &fact.volume, fact.epoch)
                        .expect("injected volume key");
                    let payload = fact.to_payload().expect("injected volume payload");
                    self.insert(
                        session,
                        key,
                        payload,
                        FactKind::Unsupported,
                        fact.epoch,
                        CandidateStatus::Verified,
                    );
                }
            } else if let Some(events) = &self.events {
                events.push(TestEvent::OwnershipWrite);
            }
            Ok(VolumeFactWriteOutcome::Inserted)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TestEvent {
    LeaseWrite,
    SnapshotRpc,
    ReceiveRpc,
    OwnershipWrite,
}

#[derive(Debug, Clone, Default)]
struct TestEvents {
    inner: Arc<Mutex<Vec<TestEvent>>>,
}

impl TestEvents {
    fn new() -> Self {
        Self::default()
    }

    fn push(&self, event: TestEvent) {
        self.inner.lock().expect("test event lock").push(event);
    }

    fn snapshot(&self) -> Vec<TestEvent> {
        self.inner.lock().expect("test event lock").clone()
    }
}

#[derive(Debug, Clone)]
struct RecordingClient {
    snapshot_count: Arc<AtomicUsize>,
    receive_count: Arc<AtomicUsize>,
    forge_receive: Option<ForgedReceiveField>,
    events: Option<TestEvents>,
}

impl RecordingClient {
    fn valid() -> Self {
        Self {
            snapshot_count: Arc::new(AtomicUsize::new(0)),
            receive_count: Arc::new(AtomicUsize::new(0)),
            forge_receive: None,
            events: None,
        }
    }

    fn record_events(mut self, events: TestEvents) -> Self {
        self.events = Some(events);
        self
    }

    fn forge_receive(mut self, field: ForgedReceiveField) -> Self {
        self.forge_receive = Some(field);
        self
    }

    fn snapshot_count(&self) -> usize {
        self.snapshot_count.load(Ordering::SeqCst)
    }

    fn receive_count(&self) -> usize {
        self.receive_count.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Copy)]
enum ForgedReceiveField {
    Source,
    Target,
    TransferId,
    Namespace,
    Volume,
    SnapshotId,
    SnapshotGuid,
    Bytes,
}

impl ForgedReceiveField {
    const ALL: [Self; 8] = [
        Self::Source,
        Self::Target,
        Self::TransferId,
        Self::Namespace,
        Self::Volume,
        Self::SnapshotId,
        Self::SnapshotGuid,
        Self::Bytes,
    ];
}

impl VolumeParticipantClient for RecordingClient {
    fn snapshot<'a>(
        &'a self,
        _session: &'a BusSession,
        request: VolumeSnapshotRequest,
        _timeout: std::time::Duration,
    ) -> Pin<Box<dyn Future<Output = VolumeResult<VolumeSnapshotReply>> + Send + 'a>> {
        Box::pin(async move {
            self.snapshot_count.fetch_add(1, Ordering::SeqCst);
            if let Some(events) = &self.events {
                events.push(TestEvent::SnapshotRpc);
            }
            Ok(VolumeSnapshotReply {
                source: request.source,
                transfer_id: request.transfer_id,
                namespace: request.namespace,
                volume: request.volume,
                snapshot_id: snapshot_id("snap-1"),
                snapshot_guid: snapshot_guid("guid-1"),
                bytes: 1024,
            })
        })
    }

    fn receive<'a>(
        &'a self,
        _session: &'a BusSession,
        request: VolumeReceiveRequest,
        _timeout: std::time::Duration,
    ) -> Pin<Box<dyn Future<Output = VolumeResult<VolumeReceiveReply>> + Send + 'a>> {
        Box::pin(async move {
            self.receive_count.fetch_add(1, Ordering::SeqCst);
            if let Some(events) = &self.events {
                events.push(TestEvent::ReceiveRpc);
            }
            let mut reply = VolumeReceiveReply {
                source: request.source,
                target: request.target,
                namespace: request.namespace,
                volume: request.volume,
                transfer_id: request.transfer_id,
                snapshot_id: request.snapshot_id,
                snapshot_guid: request.snapshot_guid,
                bytes: request.bytes,
            };
            match self.forge_receive {
                Some(ForgedReceiveField::Source) => reply.source = NodeId::new("node-c"),
                Some(ForgedReceiveField::Target) => reply.target = NodeId::new("node-c"),
                Some(ForgedReceiveField::TransferId) => reply.transfer_id = transfer("forged"),
                Some(ForgedReceiveField::Namespace) => reply.namespace = namespace("forged"),
                Some(ForgedReceiveField::Volume) => reply.volume = volume("forged"),
                Some(ForgedReceiveField::SnapshotId) => reply.snapshot_id = snapshot_id("snap-2"),
                Some(ForgedReceiveField::SnapshotGuid) => {
                    reply.snapshot_guid = snapshot_guid("guid-forged");
                }
                Some(ForgedReceiveField::Bytes) => reply.bytes += 1,
                None => {}
            }
            Ok(reply)
        })
    }
}
