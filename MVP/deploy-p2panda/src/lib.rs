use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use mvp_bus::{BusSession, FactContentHash, FactKey, FactKeyPattern, FactPayload, IslandId};
use mvp_deploy::{
    DeployCleanupDoneFact, DeployDecisionFact, DeployError, DeployFactWriter, DeployResult,
    WrittenDeployFact, deploy_cleanup_done_fact_key, deploy_cleanup_done_fact_payload,
    deploy_decision_fact_key, deploy_decision_fact_payload,
};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactOperation, PandaFactStore, PandaFactWriteOutcome,
};
use mvp_projection::{FactCandidate, FactSource, FactSourceError, FactSourceResult};
use mvp_routing_p2panda::PandaServingFactSink;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct PandaDeployFactStore {
    store: Arc<Mutex<PandaFactStore>>,
}

impl PandaDeployFactStore {
    #[must_use]
    pub fn new(store: PandaFactStore) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
        }
    }

    pub async fn export_operations(&self) -> Vec<PandaFactOperation> {
        self.store
            .lock()
            .await
            .export_operations()
            .cloned()
            .collect()
    }

    async fn write_panda_fact_payload(
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

    fn unavailable() -> FactSourceError {
        FactSourceError::Unavailable {
            name: "p2panda deploy fact store is locked by a writer".to_string(),
        }
    }
}

impl PandaServingFactSink for PandaDeployFactStore {
    fn write_fact_payload<'a>(
        &'a self,
        session: &'a BusSession,
        author: &'a PandaFactAuthor,
        key: FactKey,
        payload: FactPayload,
    ) -> Pin<Box<dyn Future<Output = mvp_p2panda_facts::Result<PandaFactWriteOutcome>> + Send + 'a>>
    {
        Box::pin(async move {
            self.write_panda_fact_payload(session, author, key, payload)
                .await
        })
    }
}

impl FactSource for PandaDeployFactStore {
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
        self.store
            .try_lock()
            .map_err(|_| Self::unavailable())?
            .read_payloads(island, candidates, session)
    }
}

#[derive(Clone)]
pub struct PandaDeployFactWriter {
    facts: PandaDeployFactStore,
    session: BusSession,
    author: Arc<PandaFactAuthor>,
}

impl PandaDeployFactWriter {
    #[must_use]
    pub fn new(
        facts: PandaDeployFactStore,
        session: BusSession,
        author: Arc<PandaFactAuthor>,
    ) -> Self {
        Self {
            facts,
            session,
            author,
        }
    }
}

impl DeployFactWriter for PandaDeployFactWriter {
    fn write_decision<'a>(
        &'a self,
        fact: DeployDecisionFact,
    ) -> Pin<Box<dyn Future<Output = DeployResult<WrittenDeployFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = deploy_decision_fact_key(&fact.deploy_id)?;
            let payload = deploy_decision_fact_payload(&fact)?;
            let outcome = self
                .facts
                .write_panda_fact_payload(&self.session, self.author.as_ref(), key, payload)
                .await
                .map_err(|error| DeployError::FactSource(error.into()))?;
            deploy_fact_outcome(outcome)
        })
    }

    fn write_cleanup_done<'a>(
        &'a self,
        fact: DeployCleanupDoneFact,
    ) -> Pin<Box<dyn Future<Output = DeployResult<WrittenDeployFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = deploy_cleanup_done_fact_key(&fact.deploy_id)?;
            let payload = deploy_cleanup_done_fact_payload(&fact)?;
            let outcome = self
                .facts
                .write_panda_fact_payload(&self.session, self.author.as_ref(), key, payload)
                .await
                .map_err(|error| DeployError::FactSource(error.into()))?;
            deploy_fact_outcome(outcome)
        })
    }
}

fn deploy_fact_outcome(outcome: PandaFactWriteOutcome) -> DeployResult<WrittenDeployFact> {
    match outcome {
        PandaFactWriteOutcome::Inserted(metadata) => Ok(WrittenDeployFact::inserted(
            metadata.key().clone(),
            metadata.content_hash().clone(),
        )),
        PandaFactWriteOutcome::AlreadyPresent(metadata) => Ok(WrittenDeployFact::already_present(
            metadata.key().clone(),
            metadata.content_hash().clone(),
        )),
        PandaFactWriteOutcome::Conflict(metadata) => Err(DeployError::DeployFactConflict {
            key: metadata.key().clone(),
            principal: metadata.author().clone(),
            content_hash: metadata.content_hash().clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mvp_bus::{FactContentHash, FactKeyPattern, Grant, IslandId, PrincipalId};
    use mvp_deploy::{
        DeployError, DeployFactWriteStatus, DeployFactWriter, DeployId, DeployManifest,
        deploy_cleanup_done_fact_payload, deploy_decision_fact_payload,
    };
    use mvp_identity::{NodeId, VisibleNodes};
    use mvp_p2panda_facts::{PandaFactAuthor, PandaFactStore};
    use mvp_projection::{BackendEndpoint, DnsRecordFact, RouteId};
    use mvp_routing::{
        DnsCommitId, GatewayCommitId, RouteCommitId, ServingCommitId, ServingCommitPlan,
        ServingFactWriteStatus, ServingFactWriter,
    };

    use mvp_routing_p2panda::PandaServingFactWriter;

    use crate::{PandaDeployFactStore, PandaDeployFactWriter};

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

    fn decision_fact(deploy_id: &str, serving_epoch: u64) -> mvp_deploy::DeployDecisionFact {
        mvp_deploy::DeployDecisionFact::new(
            manifest(deploy_id, serving_epoch),
            VisibleNodes::new([NodeId::new("node-new"), NodeId::new("node-old")]),
        )
    }

    struct PandaWriterFixture {
        facts: PandaDeployFactStore,
        session_a: mvp_bus::BusSession,
        session_b: mvp_bus::BusSession,
        author_a: Arc<PandaFactAuthor>,
        author_b: Arc<PandaFactAuthor>,
    }

    fn panda_writer_fixture() -> PandaWriterFixture {
        let (raw_bus, authority) = mvp_bus::harness::InMemoryBus::new_with_authority();
        let grant = Grant::empty()
            .with_fact_write(FactKeyPattern::parse("/facts/deploy/>").expect("deploy pattern"))
            .with_fact_write(FactKeyPattern::parse("/facts/serving/>").expect("serving pattern"));
        let session_a = authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("panda-deploy-a"),
            grant.clone(),
        );
        let session_b = authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("panda-deploy-b"),
            grant,
        );
        let author_a = Arc::new(PandaFactAuthor::new(session_a.principal().clone()));
        let author_b = Arc::new(PandaFactAuthor::new(session_b.principal().clone()));
        let store = PandaFactStore::new(Arc::new(raw_bus));
        PandaWriterFixture {
            facts: PandaDeployFactStore::new(store),
            session_a,
            session_b,
            author_a,
            author_b,
        }
    }

    #[tokio::test]
    async fn deploy_writer_records_decision_and_cleanup_outcomes() {
        let fixture = panda_writer_fixture();
        let writer = PandaDeployFactWriter::new(
            fixture.facts,
            fixture.session_a,
            Arc::clone(&fixture.author_a),
        );
        let decision = decision_fact("panda-deploy", 1);

        let inserted = writer
            .write_decision(decision.clone())
            .await
            .expect("insert decision");
        let repeated = writer
            .write_decision(decision)
            .await
            .expect("repeat decision");
        let cleanup = mvp_deploy::DeployCleanupDoneFact::new(&manifest("panda-deploy", 1));
        let cleanup_inserted = writer
            .write_cleanup_done(cleanup.clone())
            .await
            .expect("insert cleanup");
        let cleanup_repeated = writer
            .write_cleanup_done(cleanup)
            .await
            .expect("repeat cleanup");

        assert_eq!(inserted.status(), DeployFactWriteStatus::Inserted);
        assert_eq!(repeated.status(), DeployFactWriteStatus::AlreadyPresent);
        assert_eq!(cleanup_inserted.status(), DeployFactWriteStatus::Inserted);
        assert_eq!(
            cleanup_repeated.status(),
            DeployFactWriteStatus::AlreadyPresent
        );
    }

    #[tokio::test]
    async fn serving_writer_records_serving_outcomes() {
        let fixture = panda_writer_fixture();
        let writer = PandaServingFactWriter::new(
            fixture.facts,
            fixture.session_a,
            Arc::clone(&fixture.author_a),
        );
        let commit = serving_commit();

        let inserted = writer
            .write_serving_commit(&commit)
            .await
            .expect("insert serving");
        let repeated = writer
            .write_serving_commit(&commit)
            .await
            .expect("repeat serving");

        assert_eq!(inserted.status(), ServingFactWriteStatus::Inserted);
        assert_eq!(repeated.status(), ServingFactWriteStatus::AlreadyPresent);
    }

    #[tokio::test]
    async fn deploy_writer_preserves_conflict_detail() {
        let fixture = panda_writer_fixture();
        let writer_a = PandaDeployFactWriter::new(
            fixture.facts.clone(),
            fixture.session_a,
            Arc::clone(&fixture.author_a),
        );
        let writer_b = PandaDeployFactWriter::new(
            fixture.facts,
            fixture.session_b,
            Arc::clone(&fixture.author_b),
        );
        writer_a
            .write_decision(decision_fact("panda-conflict", 1))
            .await
            .expect("insert first decision");

        let conflicting = decision_fact("panda-conflict", 2);
        let expected_key =
            mvp_deploy::deploy_decision_fact_key(&conflicting.deploy_id).expect("decision key");
        let expected_hash = FactContentHash::for_payload(
            &deploy_decision_fact_payload(&conflicting).expect("decision payload"),
        );
        let error = writer_b
            .write_decision(conflicting)
            .await
            .expect_err("conflict decision");

        match error {
            DeployError::DeployFactConflict {
                key,
                principal,
                content_hash,
            } => {
                assert_eq!(key, expected_key);
                assert_eq!(principal, *fixture.author_b.principal());
                assert_eq!(content_hash, expected_hash);
            }
            other => panic!("unexpected deploy conflict error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn deploy_writer_preserves_cleanup_conflict_detail() {
        let fixture = panda_writer_fixture();
        let writer_a = PandaDeployFactWriter::new(
            fixture.facts.clone(),
            fixture.session_a,
            Arc::clone(&fixture.author_a),
        );
        let writer_b = PandaDeployFactWriter::new(
            fixture.facts,
            fixture.session_b,
            Arc::clone(&fixture.author_b),
        );
        writer_a
            .write_cleanup_done(mvp_deploy::DeployCleanupDoneFact::new(&manifest(
                "panda-cleanup-conflict",
                1,
            )))
            .await
            .expect("insert first cleanup");

        let conflicting =
            mvp_deploy::DeployCleanupDoneFact::new(&manifest("panda-cleanup-conflict", 2));
        let expected_key =
            mvp_deploy::deploy_cleanup_done_fact_key(&conflicting.deploy_id).expect("cleanup key");
        let expected_hash = FactContentHash::for_payload(
            &deploy_cleanup_done_fact_payload(&conflicting).expect("cleanup payload"),
        );
        let error = writer_b
            .write_cleanup_done(conflicting)
            .await
            .expect_err("conflict cleanup");

        match error {
            DeployError::DeployFactConflict {
                key,
                principal,
                content_hash,
            } => {
                assert_eq!(key, expected_key);
                assert_eq!(principal, *fixture.author_b.principal());
                assert_eq!(content_hash, expected_hash);
            }
            other => panic!("unexpected cleanup conflict error: {other:?}"),
        }
    }
}
