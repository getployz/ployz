use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use mvp_bus::{BusSession, FactContentHash, FactKey, FactKeyPattern, FactPayload, IslandId};
use mvp_machine::{MachineFactWriter, MachineRemoveError, MachineRemoveResult, WrittenMachineFact};
use mvp_mesh::{removal_started_fact_key, removal_started_fact_payload, tombstone_fact_key};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactAuthorKey, PandaFactOperation, PandaFactStore, PandaFactWriteOutcome,
};
use mvp_projection::{
    FactCandidate, FactSource, FactSourceError, FactSourceResult, NodeRemovalStartedFact,
    NodeTombstonedFact, ProjectionFactPayload,
};
use mvp_routing_p2panda::PandaServingFactSink;
use tokio::sync::Mutex;

pub trait PandaMachineFactSink: Send + Sync {
    fn write_fact_payload<'a>(
        &'a self,
        session: &'a BusSession,
        author: &'a PandaFactAuthor,
        key: FactKey,
        payload: FactPayload,
    ) -> Pin<Box<dyn Future<Output = mvp_p2panda_facts::Result<PandaFactWriteOutcome>> + Send + 'a>>;
}

#[derive(Clone)]
pub struct PandaMachineFactStore {
    store: Arc<Mutex<PandaFactStore>>,
}

impl PandaMachineFactStore {
    #[must_use]
    pub fn new(store: PandaFactStore) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
        }
    }

    pub async fn write_fact_payload(
        &self,
        session: &BusSession,
        author: &PandaFactAuthor,
        key: FactKey,
        payload: FactPayload,
    ) -> mvp_p2panda_facts::Result<PandaFactWriteOutcome> {
        self.store
            .lock()
            .await
            .write_fact_payload(session, author, key, payload)
            .await
    }

    pub async fn trust_author_key(
        &self,
        island: &IslandId,
        principal: mvp_bus::PrincipalId,
        author_key: PandaFactAuthorKey,
    ) -> mvp_p2panda_facts::Result<()> {
        self.store
            .lock()
            .await
            .trust_author_key(island, principal, author_key)
    }

    pub async fn trust_replica_peer(&self, island: &IslandId, principal: mvp_bus::PrincipalId) {
        self.store
            .lock()
            .await
            .trust_replica_peer(island, principal);
    }

    pub async fn import_replica_operation(
        &self,
        session: &BusSession,
        operation: &PandaFactOperation,
    ) -> mvp_p2panda_facts::Result<PandaFactWriteOutcome> {
        self.store
            .lock()
            .await
            .import_replica_operation(session, operation)
            .await
    }

    pub async fn export_operations(&self) -> Vec<PandaFactOperation> {
        self.store
            .lock()
            .await
            .export_operations()
            .cloned()
            .collect()
    }

    fn unavailable() -> FactSourceError {
        FactSourceError::Unavailable {
            name: "p2panda machine fact store is locked by a writer".to_string(),
        }
    }
}

impl PandaServingFactSink for PandaMachineFactStore {
    fn write_fact_payload<'a>(
        &'a self,
        session: &'a BusSession,
        author: &'a PandaFactAuthor,
        key: FactKey,
        payload: FactPayload,
    ) -> Pin<Box<dyn Future<Output = mvp_p2panda_facts::Result<PandaFactWriteOutcome>> + Send + 'a>>
    {
        Box::pin(async move { self.write_fact_payload(session, author, key, payload).await })
    }
}

impl PandaMachineFactSink for PandaMachineFactStore {
    fn write_fact_payload<'a>(
        &'a self,
        session: &'a BusSession,
        author: &'a PandaFactAuthor,
        key: FactKey,
        payload: FactPayload,
    ) -> Pin<Box<dyn Future<Output = mvp_p2panda_facts::Result<PandaFactWriteOutcome>> + Send + 'a>>
    {
        Box::pin(async move { self.write_fact_payload(session, author, key, payload).await })
    }
}

impl FactSource for PandaMachineFactStore {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        self.store
            .try_lock()
            .map_err(|_| Self::unavailable())?
            .list_candidates(island, pattern, session)
    }

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        let store = self.store.try_lock().map_err(|_| Self::unavailable())?;
        validate_candidates_still_current(&store, island, candidates, session)?;
        store.read_payloads(island, candidates, session)
    }
}

fn validate_candidates_still_current(
    store: &PandaFactStore,
    island: &IslandId,
    candidates: &[FactCandidate],
    session: &BusSession,
) -> FactSourceResult<()> {
    for candidate in candidates {
        let pattern = FactKeyPattern::parse(candidate.key().as_str()).map_err(|error| {
            FactSourceError::Unavailable {
                name: format!("candidate key no longer parses as an exact pattern: {error}"),
            }
        })?;
        let current = store.list_candidates(island, &pattern, session)?;
        if !current.iter().any(|stored| stored == candidate) {
            return Err(FactSourceError::Unavailable {
                name: format!(
                    "p2panda machine fact candidate changed while projection was reading {}",
                    candidate.key()
                ),
            });
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct PandaMachineFactWriter<S = PandaMachineFactStore> {
    facts: S,
    session: BusSession,
    author: Arc<PandaFactAuthor>,
}

impl<S> PandaMachineFactWriter<S> {
    #[must_use]
    pub fn new(facts: S, session: BusSession, author: Arc<PandaFactAuthor>) -> Self {
        Self {
            facts,
            session,
            author,
        }
    }
}

impl<S> PandaMachineFactWriter<S>
where
    S: PandaMachineFactSink,
{
    async fn write_machine_fact(
        &self,
        key: FactKey,
        payload: FactPayload,
        operation: &'static str,
    ) -> MachineRemoveResult<WrittenMachineFact> {
        let outcome = self
            .facts
            .write_fact_payload(&self.session, self.author.as_ref(), key, payload)
            .await
            .map_err(|error| machine_fact_store_error(operation, error))?;
        written_machine_fact_from_outcome(outcome)
    }
}

impl<S> MachineFactWriter for PandaMachineFactWriter<S>
where
    S: PandaMachineFactSink,
{
    fn write_removal_started<'a>(
        &'a self,
        fact: NodeRemovalStartedFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = removal_started_fact_key(&fact.node_id, fact.epoch)?;
            let payload = removal_started_fact_payload(&fact.node_id, fact.epoch, fact.reason)?;
            self.write_machine_fact(key, payload, "write removal-started fact")
                .await
        })
    }

    fn write_tombstone<'a>(
        &'a self,
        fact: NodeTombstonedFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = tombstone_fact_key(&fact.node_id, fact.epoch)?;
            let payload = ProjectionFactPayload::NodeTombstoned(fact)
                .to_fact_bytes()
                .map(FactPayload::from)
                .map_err(|source| MachineRemoveError::WirePayload {
                    context: "serialize tombstone fact",
                    source,
                })?;
            self.write_machine_fact(key, payload, "write tombstone fact")
                .await
        })
    }
}

fn machine_fact_store_error(
    operation: &'static str,
    error: mvp_p2panda_facts::PandaFactError,
) -> MachineRemoveError {
    match error {
        mvp_p2panda_facts::PandaFactError::UnauthorizedWrite {
            island,
            principal,
            key,
        } => MachineRemoveError::UnauthorizedFactWrite {
            island,
            principal,
            key,
        },
        mvp_p2panda_facts::PandaFactError::PrincipalMismatch { session, author } => {
            MachineRemoveError::PrincipalMismatch { session, author }
        }
        mvp_p2panda_facts::PandaFactError::UntrustedAuthorKey { island, principal } => {
            MachineRemoveError::UntrustedAuthorKey { island, principal }
        }
        mvp_p2panda_facts::PandaFactError::AuthorKeyMismatch { island, principal } => {
            MachineRemoveError::AuthorKeyMismatch { island, principal }
        }
        error => MachineRemoveError::FactStore {
            operation,
            message: error.to_string(),
        },
    }
}

fn written_machine_fact_from_outcome(
    outcome: PandaFactWriteOutcome,
) -> MachineRemoveResult<WrittenMachineFact> {
    match outcome {
        PandaFactWriteOutcome::Inserted(metadata)
        | PandaFactWriteOutcome::AlreadyPresent(metadata) => Ok(WrittenMachineFact {
            key: metadata.key().clone(),
        }),
        PandaFactWriteOutcome::Conflict(metadata) => Err(MachineRemoveError::FactConflict {
            key: metadata.key().clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    use mvp_bus::{
        FactKey, FactKeyPattern, FactPayload, Grant, IslandId, PrincipalId, harness::InMemoryBus,
    };
    use mvp_identity::NodeId;
    use mvp_machine::{MachineFactWriter, MachineRemoveError};
    use mvp_mesh::{removal_started_fact_key, tombstone_fact_key};
    use mvp_p2panda_facts::{
        PandaFactAuthor, PandaFactError, PandaFactStore, PandaFactWriteOutcome,
    };
    use mvp_projection::{
        BackendEndpoint, DnsRecordFact, FactSource, NodeJoinedFact, NodeRemovalStartedFact,
        NodeTombstonedFact, ProjectionFactPayload, RouteId,
    };
    use mvp_routing::{
        DnsCommitId, GatewayCommitId, RouteCommitId, ServingCommitId, ServingCommitPlan,
        ServingFactWriteStatus, ServingFactWriter,
    };
    use mvp_routing_p2panda::PandaServingFactWriter;

    use crate::{PandaMachineFactSink, PandaMachineFactStore, PandaMachineFactWriter};

    struct Fixture {
        facts: PandaMachineFactStore,
        machine_a: mvp_bus::BusSession,
        machine_b: mvp_bus::BusSession,
        join_writer: mvp_bus::BusSession,
        routing_writer: mvp_bus::BusSession,
        reader: mvp_bus::BusSession,
        replica: mvp_bus::BusSession,
        machine_author_a: Arc<PandaFactAuthor>,
        machine_author_b: Arc<PandaFactAuthor>,
        join_author: Arc<PandaFactAuthor>,
        routing_author: Arc<PandaFactAuthor>,
    }

    fn fixture() -> Fixture {
        let (raw_bus, authority) = InMemoryBus::new_with_authority();
        let island = IslandId::new("prod");
        let machine_grant = Grant::empty()
            .with_fact_write(pattern("/facts/node/*/removal_started/>"))
            .with_fact_write(pattern("/facts/node/*/tombstoned/>"));
        let join_grant = Grant::empty().with_fact_write(pattern("/facts/node/*/joined/>"));
        let routing_grant = Grant::empty().with_fact_write(pattern("/facts/serving/>"));
        let machine_a = authority.grant_in(
            island.clone(),
            PrincipalId::new("machine-a"),
            machine_grant.clone(),
        );
        let machine_b =
            authority.grant_in(island.clone(), PrincipalId::new("machine-b"), machine_grant);
        let join_writer =
            authority.grant_in(island.clone(), PrincipalId::new("join-writer"), join_grant);
        let routing_writer =
            authority.grant_in(island.clone(), PrincipalId::new("routing"), routing_grant);
        let reader = authority.grant_in(
            island.clone(),
            PrincipalId::new("projection"),
            Grant::empty().with_fact_read(pattern("/facts/>")),
        );
        let replica = authority.grant_in(island, PrincipalId::new("replica"), Grant::empty());
        let machine_author_a = Arc::new(PandaFactAuthor::new(machine_a.principal().clone()));
        let machine_author_b = Arc::new(PandaFactAuthor::new(machine_b.principal().clone()));
        let join_author = Arc::new(PandaFactAuthor::new(join_writer.principal().clone()));
        let routing_author = Arc::new(PandaFactAuthor::new(routing_writer.principal().clone()));
        Fixture {
            facts: PandaMachineFactStore::new(PandaFactStore::new(Arc::new(raw_bus))),
            machine_a,
            machine_b,
            join_writer,
            routing_writer,
            reader,
            replica,
            machine_author_a,
            machine_author_b,
            join_author,
            routing_author,
        }
    }

    fn pattern(value: &str) -> FactKeyPattern {
        FactKeyPattern::parse(value).expect("fact pattern")
    }

    fn machine_writer(fixture: &Fixture) -> PandaMachineFactWriter {
        PandaMachineFactWriter::new(
            fixture.facts.clone(),
            fixture.machine_a.clone(),
            Arc::clone(&fixture.machine_author_a),
        )
    }

    #[derive(Clone)]
    struct FailingSink;

    impl PandaMachineFactSink for FailingSink {
        fn write_fact_payload<'a>(
            &'a self,
            _session: &'a mvp_bus::BusSession,
            _author: &'a PandaFactAuthor,
            _key: FactKey,
            _payload: FactPayload,
        ) -> Pin<
            Box<dyn Future<Output = mvp_p2panda_facts::Result<PandaFactWriteOutcome>> + Send + 'a>,
        > {
            Box::pin(async {
                Err(PandaFactError::Store {
                    message: "simulated store outage".to_string(),
                })
            })
        }
    }

    fn removal_started(reason: &str) -> NodeRemovalStartedFact {
        NodeRemovalStartedFact {
            node_id: NodeId::new("node-old"),
            epoch: 2,
            reason: reason.to_string(),
        }
    }

    fn tombstone(reason: &str) -> NodeTombstonedFact {
        NodeTombstonedFact {
            node_id: NodeId::new("node-old"),
            epoch: 3,
            reason: reason.to_string(),
        }
    }

    fn serving_commit() -> ServingCommitPlan {
        ServingCommitPlan {
            serving_commit_id: ServingCommitId::new("serving-remove-1"),
            route_commit_id: RouteCommitId::new("route-remove-1"),
            gateway_commit_id: GatewayCommitId::new("gateway-remove-1"),
            dns_commit_id: DnsCommitId::new("dns-remove-1"),
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

    fn joined_payload(node_id: &NodeId) -> FactPayload {
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: node_id.clone(),
            epoch: 1,
            overlay_ip: "fd00::1".to_string(),
            iroh_endpoint_id: "iroh-node-old".to_string(),
            wg_public_key: "wg-node-old".to_string(),
        })
        .to_fact_bytes()
        .map(FactPayload::from)
        .expect("joined payload")
    }

    #[tokio::test]
    async fn machine_writer_records_removal_and_tombstone_facts() {
        let fixture = fixture();
        let writer = machine_writer(&fixture);

        let removal = writer
            .write_removal_started(removal_started("remove"))
            .await
            .expect("write removal started");
        let tombstone = writer
            .write_tombstone(tombstone("remove"))
            .await
            .expect("write tombstone");

        assert_eq!(
            removal.key.as_str(),
            "/facts/node/node-old/removal_started/2"
        );
        assert_eq!(tombstone.key.as_str(), "/facts/node/node-old/tombstoned/3");
    }

    #[tokio::test]
    async fn duplicate_machine_fact_is_idempotent_even_from_another_author() {
        let fixture = fixture();
        let writer_a = machine_writer(&fixture);
        let writer_b = PandaMachineFactWriter::new(
            fixture.facts.clone(),
            fixture.machine_b.clone(),
            Arc::clone(&fixture.machine_author_b),
        );
        let fact = removal_started("remove");

        writer_a
            .write_removal_started(fact.clone())
            .await
            .expect("first write");
        let repeated = writer_b
            .write_removal_started(fact)
            .await
            .expect("same payload is cluster-idempotent");

        assert_eq!(
            repeated.key,
            removal_started_fact_key(&NodeId::new("node-old"), 2).expect("removal key")
        );
    }

    #[tokio::test]
    async fn conflicting_machine_fact_is_foreground_failure() {
        let fixture = fixture();
        let writer_a = machine_writer(&fixture);
        let writer_b = PandaMachineFactWriter::new(
            fixture.facts.clone(),
            fixture.machine_b.clone(),
            Arc::clone(&fixture.machine_author_b),
        );

        writer_a
            .write_removal_started(removal_started("first"))
            .await
            .expect("first write");
        let error = writer_b
            .write_removal_started(removal_started("second"))
            .await
            .expect_err("conflicting removal started");

        assert!(matches!(
            error,
            MachineRemoveError::FactConflict { key }
                if key == removal_started_fact_key(&NodeId::new("node-old"), 2)
                    .expect("removal key")
        ));
    }

    #[tokio::test]
    async fn unauthorized_machine_fact_write_is_foreground_failure() {
        let fixture = fixture();
        let writer = PandaMachineFactWriter::new(
            fixture.facts.clone(),
            fixture.join_writer.clone(),
            Arc::clone(&fixture.join_author),
        );

        let error = writer
            .write_tombstone(tombstone("remove"))
            .await
            .expect_err("join writer cannot tombstone");

        assert!(matches!(
            error,
            MachineRemoveError::UnauthorizedFactWrite { key, .. }
                if key == tombstone_fact_key(&NodeId::new("node-old"), 3).expect("tombstone key")
        ));
    }

    #[tokio::test]
    async fn backend_failure_is_foreground_fact_store_error() {
        let fixture = fixture();
        let writer = PandaMachineFactWriter::new(
            FailingSink,
            fixture.machine_a,
            Arc::clone(&fixture.machine_author_a),
        );

        let error = writer
            .write_removal_started(removal_started("remove"))
            .await
            .expect_err("backend failure");

        assert!(matches!(
            error,
            MachineRemoveError::FactStore {
                operation: "write removal-started fact",
                message
            } if message.contains("simulated store outage")
        ));
    }

    #[tokio::test]
    async fn stale_candidate_read_fails_after_conflict_changes_status() {
        let fixture = fixture();
        let writer_a = machine_writer(&fixture);
        let writer_b = PandaMachineFactWriter::new(
            fixture.facts.clone(),
            fixture.machine_b.clone(),
            Arc::clone(&fixture.machine_author_b),
        );
        writer_a
            .write_removal_started(removal_started("first"))
            .await
            .expect("first write");
        let candidates = fixture
            .facts
            .list_candidates(
                fixture.reader.island(),
                &pattern("/facts/node/node-old/removal_started/2"),
                &fixture.reader,
            )
            .expect("list initial candidate");
        assert_eq!(candidates.len(), 1);

        writer_b
            .write_removal_started(removal_started("second"))
            .await
            .expect_err("second write conflicts");
        let error = fixture
            .facts
            .read_payloads(fixture.reader.island(), &candidates, &fixture.reader)
            .expect_err("stale candidate status should fail");

        assert!(
            matches!(error, mvp_projection::FactSourceError::Unavailable { name } if name.contains("changed while projection was reading"))
        );
    }

    #[tokio::test]
    async fn import_replay_requires_trusted_replica_peer() {
        let fixture = fixture();
        machine_writer(&fixture)
            .write_removal_started(removal_started("remove"))
            .await
            .expect("write removal");
        let operations = fixture.facts.export_operations().await;
        let [operation] = operations.as_slice() else {
            panic!("expected one exported operation");
        };
        let fresh = fixture_store_with_existing_authority(&fixture);
        fresh
            .trust_author_key(
                fixture.replica.island(),
                fixture.machine_a.principal().clone(),
                fixture.machine_author_a.author_key(),
            )
            .await
            .expect("trust machine author");

        let untrusted = fresh
            .import_replica_operation(&fixture.replica, operation)
            .await
            .expect_err("replica peer must be trusted");
        assert!(matches!(
            untrusted,
            PandaFactError::UnauthorizedReplicaImport { .. }
        ));

        fresh
            .trust_replica_peer(
                fixture.replica.island(),
                fixture.replica.principal().clone(),
            )
            .await;
        fresh
            .import_replica_operation(&fixture.replica, operation)
            .await
            .expect("trusted replica import");
    }

    #[tokio::test]
    async fn routing_serving_writer_can_use_same_store() {
        let fixture = fixture();
        let writer = PandaServingFactWriter::new(
            fixture.facts,
            fixture.routing_writer,
            fixture.routing_author,
        );

        let inserted = writer
            .write_serving_commit(&serving_commit())
            .await
            .expect("write serving commit");

        assert_eq!(inserted.status(), ServingFactWriteStatus::Inserted);
    }

    fn fixture_store_with_existing_authority(fixture: &Fixture) -> PandaMachineFactStore {
        let (raw_bus, authority) = InMemoryBus::new_with_authority();
        let island = fixture.reader.island().clone();
        let machine_grant = Grant::empty()
            .with_fact_write(pattern("/facts/node/*/removal_started/>"))
            .with_fact_write(pattern("/facts/node/*/tombstoned/>"));
        authority.grant_in(
            island.clone(),
            fixture.machine_a.principal().clone(),
            machine_grant,
        );
        authority.grant_in(island, fixture.replica.principal().clone(), Grant::empty());
        PandaMachineFactStore::new(PandaFactStore::new(Arc::new(raw_bus)))
    }

    #[tokio::test]
    async fn adapter_projects_joined_machine_and_serving_facts() {
        let fixture = fixture();
        let node_id = NodeId::new("node-old");
        fixture
            .facts
            .write_fact_payload(
                &fixture.join_writer,
                fixture.join_author.as_ref(),
                FactKey::parse(format!("/facts/node/{}/joined/1", node_id.as_str()))
                    .expect("joined key"),
                joined_payload(&node_id),
            )
            .await
            .expect("write joined fact");
        machine_writer(&fixture)
            .write_removal_started(removal_started("remove"))
            .await
            .expect("write removal");
        machine_writer(&fixture)
            .write_tombstone(tombstone("remove"))
            .await
            .expect("write tombstone");
        let serving_writer = PandaServingFactWriter::new(
            fixture.facts.clone(),
            fixture.routing_writer.clone(),
            Arc::clone(&fixture.routing_author),
        );
        serving_writer
            .write_serving_commit(&serving_commit())
            .await
            .expect("write serving");

        let candidates = fixture
            .facts
            .list_candidates(
                fixture.reader.island(),
                &pattern("/facts/>"),
                &fixture.reader,
            )
            .expect("list candidates");
        let payloads = fixture
            .facts
            .read_payloads(fixture.reader.island(), &candidates, &fixture.reader)
            .expect("read payloads");

        assert_eq!(candidates.len(), 4);
        assert_eq!(payloads.len(), 4);
        assert!(candidates.iter().any(|candidate| {
            candidate.key() == &tombstone_fact_key(&NodeId::new("node-old"), 3).expect("key")
        }));
    }
}
