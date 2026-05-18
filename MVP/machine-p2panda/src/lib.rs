use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use mvp_bus::{BusSession, FactKey, FactPayload};
use mvp_machine::{
    MachineFactWriter, MachineRemoveCleanupDoneFact, MachineRemoveDecisionFact, MachineRemoveError,
    MachineRemoveId, MachineRemoveResult, WrittenMachineFact, machine_remove_cleanup_done_fact_key,
    machine_remove_cleanup_done_fact_payload, machine_remove_decision_fact_key,
    machine_remove_decision_fact_payload,
};
use mvp_mesh::{removal_started_fact_key, removal_started_fact_payload, tombstone_fact_key};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactError, PandaFactWriteOutcome, SharedPandaFactStore,
};
use mvp_projection::{NodeRemovalStartedFact, NodeTombstonedFact, ProjectionFactPayload};

#[derive(Clone)]
pub struct PandaMachineFactStore {
    inner: SharedPandaFactStore,
}

impl PandaMachineFactStore {
    #[must_use]
    pub fn new(store: mvp_p2panda_facts::PandaFactStore) -> Self {
        Self {
            inner: SharedPandaFactStore::new(store),
        }
    }

    #[must_use]
    pub fn shared(&self) -> SharedPandaFactStore {
        self.inner.clone()
    }

    pub async fn write_fact_payload(
        &self,
        session: &BusSession,
        author: &PandaFactAuthor,
        key: FactKey,
        payload: FactPayload,
    ) -> mvp_p2panda_facts::Result<PandaFactWriteOutcome> {
        self.inner
            .write_fact_payload(session, author, key, payload)
            .await
    }

    pub async fn trust_author_key(
        &self,
        island: &mvp_bus::IslandId,
        principal: mvp_bus::PrincipalId,
        author_key: mvp_p2panda_facts::PandaFactAuthorKey,
    ) -> mvp_p2panda_facts::Result<()> {
        self.inner
            .trust_author_key(island, principal, author_key)
            .await
    }

    pub async fn trust_replica_peer(
        &self,
        island: &mvp_bus::IslandId,
        principal: mvp_bus::PrincipalId,
    ) {
        self.inner.trust_replica_peer(island, principal).await;
    }

    pub async fn import_replica_operation(
        &self,
        session: &BusSession,
        operation: &mvp_p2panda_facts::PandaFactOperation,
    ) -> mvp_p2panda_facts::Result<PandaFactWriteOutcome> {
        self.inner
            .import_replica_operation(session, operation)
            .await
    }

    pub async fn export_operations(&self) -> Vec<mvp_p2panda_facts::PandaFactOperation> {
        self.inner.export_operations().await
    }
}

impl From<PandaMachineFactStore> for SharedPandaFactStore {
    fn from(value: PandaMachineFactStore) -> Self {
        value.inner
    }
}

impl mvp_projection::FactSource for PandaMachineFactStore {
    fn list_candidates(
        &self,
        island: &mvp_bus::IslandId,
        pattern: &mvp_bus::FactKeyPattern,
        session: &BusSession,
    ) -> mvp_projection::FactSourceResult<Vec<mvp_projection::FactCandidate>> {
        self.inner.list_candidates(island, pattern, session)
    }

    fn read_payloads(
        &self,
        island: &mvp_bus::IslandId,
        candidates: &[mvp_projection::FactCandidate],
        session: &BusSession,
    ) -> mvp_projection::FactSourceResult<
        std::collections::BTreeMap<mvp_bus::FactContentHash, FactPayload>,
    > {
        self.inner.read_payloads(island, candidates, session)
    }
}

#[derive(Clone)]
pub struct PandaMachineFactWriter {
    facts: PandaMachineFactStore,
    session: BusSession,
    author: Arc<PandaFactAuthor>,
}

impl PandaMachineFactWriter {
    #[must_use]
    pub fn new(
        facts: PandaMachineFactStore,
        session: BusSession,
        author: Arc<PandaFactAuthor>,
    ) -> Self {
        Self {
            facts,
            session,
            author,
        }
    }

    async fn write_machine_fact(
        &self,
        key: FactKey,
        payload: FactPayload,
        operation: &'static str,
    ) -> MachineRemoveResult<WrittenMachineFact> {
        let outcome = self
            .facts
            .shared()
            .write_fact_payload(&self.session, self.author.as_ref(), key, payload)
            .await
            .map_err(|error| machine_fact_store_error(operation, error))?;
        written_machine_fact_from_outcome(outcome)
    }
}

impl MachineFactWriter for PandaMachineFactWriter {
    fn write_remove_decision<'a>(
        &'a self,
        fact: MachineRemoveDecisionFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = machine_remove_decision_fact_key(&fact.remove_id())?;
            let payload = machine_remove_decision_fact_payload(&fact)?;
            self.write_machine_fact(key, payload, "write machine-remove decision fact")
                .await
        })
    }

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

    fn write_cleanup_done<'a>(
        &'a self,
        fact: MachineRemoveCleanupDoneFact,
    ) -> Pin<Box<dyn Future<Output = MachineRemoveResult<WrittenMachineFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = machine_remove_cleanup_done_fact_key(&MachineRemoveId::new(
                fact.target_node_id.clone(),
                fact.removal_epoch,
            ))?;
            let payload = machine_remove_cleanup_done_fact_payload(&fact)?;
            self.write_machine_fact(key, payload, "write machine-remove cleanup-done fact")
                .await
        })
    }
}

fn machine_fact_store_error(operation: &'static str, error: PandaFactError) -> MachineRemoveError {
    match error {
        PandaFactError::UnauthorizedWrite {
            island,
            principal,
            key,
        } => MachineRemoveError::UnauthorizedFactWrite {
            island,
            principal,
            key,
        },
        PandaFactError::PrincipalMismatch { session, author } => {
            MachineRemoveError::PrincipalMismatch { session, author }
        }
        PandaFactError::UntrustedAuthorKey { island, principal } => {
            MachineRemoveError::UntrustedAuthorKey { island, principal }
        }
        PandaFactError::AuthorKeyMismatch { island, principal } => {
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
    use std::sync::Arc;

    use mvp_bus::{
        FactKey, FactKeyPattern, FactPayload, Grant, IslandId, PrincipalId, harness::InMemoryBus,
    };
    use mvp_identity::{NodeId, VisibleNodes};
    use mvp_machine::{
        MachineFactWriter, MachineRemoveCleanupDoneFact, MachineRemoveDecisionFact,
        MachineRemoveError, machine_remove_decision_fact_key, read_machine_remove_decision,
    };
    use mvp_mesh::{removal_started_fact_key, tombstone_fact_key};
    use mvp_p2panda_facts::{PandaFactAuthor, PandaFactError, PandaFactStore};
    use mvp_projection::{
        BackendEndpoint, DnsRecordFact, FactSource, NodeJoinedFact, NodeRemovalStartedFact,
        NodeTombstonedFact, ProjectionFactPayload, RouteId,
    };
    use mvp_routing::{
        DnsCommitId, GatewayCommitId, RouteCommitId, ServingCommitId, ServingCommitPlan,
        ServingFactWriteStatus, ServingFactWriter,
    };
    use mvp_routing_p2panda::PandaServingFactWriter;

    use crate::{PandaMachineFactStore, PandaMachineFactWriter, machine_fact_store_error};

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
            .with_fact_write(pattern("/facts/machine-remove/>"))
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

    fn decision_fact() -> MachineRemoveDecisionFact {
        MachineRemoveDecisionFact::new(
            NodeId::new("node-old"),
            2,
            3,
            "remove".to_string(),
            VisibleNodes::new([NodeId::new("node-old"), NodeId::new("node-new")]),
            serving_commit(),
        )
    }

    fn cleanup_done_fact() -> MachineRemoveCleanupDoneFact {
        let decision = decision_fact();
        let tombstone_key = tombstone_fact_key(&decision.target_node_id, decision.tombstone_epoch)
            .expect("tombstone key");
        MachineRemoveCleanupDoneFact::new(&decision, tombstone_key).expect("cleanup done")
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

        let decision = writer
            .write_remove_decision(decision_fact())
            .await
            .expect("write decision");
        let removal = writer
            .write_removal_started(removal_started("remove"))
            .await
            .expect("write removal started");
        let tombstone = writer
            .write_tombstone(tombstone("remove"))
            .await
            .expect("write tombstone");
        let cleanup_done = writer
            .write_cleanup_done(cleanup_done_fact())
            .await
            .expect("write cleanup done");

        assert_eq!(
            decision.key.as_str(),
            "/facts/machine-remove/node-old/2/decision"
        );
        assert_eq!(
            removal.key.as_str(),
            "/facts/node/node-old/removal_started/2"
        );
        assert_eq!(tombstone.key.as_str(), "/facts/node/node-old/tombstoned/3");
        assert_eq!(
            cleanup_done.key.as_str(),
            "/facts/machine-remove/node-old/2/cleanup/done"
        );
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
    async fn conflicting_command_fact_is_foreground_failure() {
        let fixture = fixture();
        let writer_a = machine_writer(&fixture);
        let writer_b = PandaMachineFactWriter::new(
            fixture.facts.clone(),
            fixture.machine_b.clone(),
            Arc::clone(&fixture.machine_author_b),
        );
        let mut conflicting = decision_fact();
        conflicting.reason = "other-remove".to_string();

        writer_a
            .write_remove_decision(decision_fact())
            .await
            .expect("first write");
        let error = writer_b
            .write_remove_decision(conflicting)
            .await
            .expect_err("conflicting decision");

        assert!(matches!(
            error,
            MachineRemoveError::FactConflict { key }
                if key == machine_remove_decision_fact_key(&decision_fact().remove_id())
                    .expect("decision key")
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
    async fn node_fact_only_writer_cannot_write_command_facts() {
        let (raw_bus, authority) = InMemoryBus::new_with_authority();
        let island = IslandId::new("prod");
        let session = authority.grant_in(
            island,
            PrincipalId::new("node-only"),
            Grant::empty().with_fact_write(pattern("/facts/node/*/tombstoned/>")),
        );
        let facts = PandaMachineFactStore::new(PandaFactStore::new(Arc::new(raw_bus)));
        let author = Arc::new(PandaFactAuthor::new(session.principal().clone()));
        let writer = PandaMachineFactWriter::new(facts, session, author);

        let error = writer
            .write_remove_decision(decision_fact())
            .await
            .expect_err("node-only writer cannot write decision");

        assert!(matches!(
            error,
            MachineRemoveError::UnauthorizedFactWrite { key, .. }
                if key == machine_remove_decision_fact_key(&decision_fact().remove_id())
                    .expect("decision key")
        ));
    }

    #[tokio::test]
    async fn backend_failure_is_foreground_fact_store_error() {
        let error = machine_fact_store_error(
            "write removal-started fact",
            PandaFactError::Store {
                message: "simulated store outage".to_string(),
            },
        );

        assert!(matches!(
            error,
            MachineRemoveError::FactStore {
                operation: "write removal-started fact",
                message
            } if message.contains("simulated store outage")
        ));
    }

    #[tokio::test]
    async fn stale_candidate_read_filters_candidate_after_conflict_changes_status() {
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
        let payloads = fixture
            .facts
            .read_payloads(fixture.reader.island(), &candidates, &fixture.reader)
            .expect("stale candidate status is filtered");

        assert!(payloads.is_empty());
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
    async fn imported_command_fact_is_readable_for_recovery() {
        let fixture = fixture();
        machine_writer(&fixture)
            .write_remove_decision(decision_fact())
            .await
            .expect("write decision");
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
        let recovered = read_machine_remove_decision(
            &fresh,
            fixture.reader.island(),
            &fixture.reader,
            &decision_fact().remove_id(),
        )
        .expect("read imported decision");

        assert_eq!(recovered.fact, decision_fact());
    }

    #[tokio::test]
    async fn routing_serving_writer_can_use_same_store() {
        let fixture = fixture();
        let writer = PandaServingFactWriter::new(
            fixture.facts.shared(),
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
            .with_fact_write(pattern("/facts/machine-remove/>"))
            .with_fact_write(pattern("/facts/node/*/removal_started/>"))
            .with_fact_write(pattern("/facts/node/*/tombstoned/>"));
        authority.grant_in(
            island.clone(),
            fixture.machine_a.principal().clone(),
            machine_grant,
        );
        authority.grant_in(
            island.clone(),
            fixture.reader.principal().clone(),
            Grant::empty().with_fact_read(pattern("/facts/>")),
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
            fixture.facts.shared(),
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
