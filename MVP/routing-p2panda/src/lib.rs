use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use mvp_bus::{BusSession, FactKey, FactPayload};
use mvp_p2panda_facts::{PandaFactAuthor, PandaFactWriteOutcome};
use mvp_routing::{
    RoutingError, RoutingResult, ServingCommitPlan, ServingFactWriter, WrittenServingFact,
    serving_commit_fact_key, serving_commit_fact_payload,
};

pub trait PandaServingFactSink: Send + Sync {
    fn write_fact_payload<'a>(
        &'a self,
        session: &'a BusSession,
        author: &'a PandaFactAuthor,
        key: FactKey,
        payload: FactPayload,
    ) -> Pin<Box<dyn Future<Output = mvp_p2panda_facts::Result<PandaFactWriteOutcome>> + Send + 'a>>;
}

#[derive(Clone)]
pub struct PandaServingFactWriter<S> {
    sink: S,
    session: BusSession,
    author: Arc<PandaFactAuthor>,
}

impl<S> PandaServingFactWriter<S> {
    #[must_use]
    pub fn new(sink: S, session: BusSession, author: Arc<PandaFactAuthor>) -> Self {
        Self {
            sink,
            session,
            author,
        }
    }
}

impl<S> ServingFactWriter for PandaServingFactWriter<S>
where
    S: PandaServingFactSink,
{
    fn write_serving_commit<'a>(
        &'a self,
        commit: &'a ServingCommitPlan,
    ) -> Pin<Box<dyn Future<Output = RoutingResult<WrittenServingFact>> + Send + 'a>> {
        Box::pin(async move {
            let key = serving_commit_fact_key(&commit.serving_commit_id)?;
            let payload = serving_commit_fact_payload(commit)?;
            let outcome = self
                .sink
                .write_fact_payload(&self.session, self.author.as_ref(), key, payload)
                .await
                .map_err(|error| RoutingError::FactSource(error.into()))?;
            serving_fact_outcome(outcome)
        })
    }
}

fn serving_fact_outcome(outcome: PandaFactWriteOutcome) -> RoutingResult<WrittenServingFact> {
    match outcome {
        PandaFactWriteOutcome::Inserted(metadata) => Ok(WrittenServingFact::inserted(
            metadata.key().clone(),
            metadata.content_hash().clone(),
        )),
        PandaFactWriteOutcome::AlreadyPresent(metadata) => Ok(WrittenServingFact::already_present(
            metadata.key().clone(),
            metadata.content_hash().clone(),
        )),
        PandaFactWriteOutcome::Conflict(metadata) => Err(RoutingError::ServingFactConflict {
            key: metadata.key().clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    use mvp_bus::{FactKeyPattern, Grant, IslandId, PrincipalId};
    use mvp_identity::NodeId;
    use mvp_p2panda_facts::{PandaFactAuthor, PandaFactStore, PandaFactWriteOutcome};
    use mvp_projection::{BackendEndpoint, DnsRecordFact, RouteId};
    use mvp_routing::{
        DnsCommitId, GatewayCommitId, RouteCommitId, RoutingError, ServingCommitId,
        ServingCommitPlan, ServingFactWriteStatus, ServingFactWriter,
    };
    use tokio::sync::Mutex;

    use crate::{PandaServingFactSink, PandaServingFactWriter};

    #[derive(Clone)]
    struct SharedStore {
        store: Arc<Mutex<PandaFactStore>>,
    }

    impl SharedStore {
        fn new(store: PandaFactStore) -> Self {
            Self {
                store: Arc::new(Mutex::new(store)),
            }
        }
    }

    impl PandaServingFactSink for SharedStore {
        fn write_fact_payload<'a>(
            &'a self,
            session: &'a mvp_bus::BusSession,
            author: &'a PandaFactAuthor,
            key: mvp_bus::FactKey,
            payload: mvp_bus::FactPayload,
        ) -> Pin<
            Box<dyn Future<Output = mvp_p2panda_facts::Result<PandaFactWriteOutcome>> + Send + 'a>,
        > {
            Box::pin(async move {
                self.store
                    .lock()
                    .await
                    .write_fact_payload(session, author, key, payload)
                    .await
            })
        }
    }

    fn fixture() -> (
        SharedStore,
        mvp_bus::BusSession,
        mvp_bus::BusSession,
        Arc<PandaFactAuthor>,
        Arc<PandaFactAuthor>,
    ) {
        let (raw_bus, authority) = mvp_bus::harness::InMemoryBus::new_with_authority();
        let grant = Grant::empty()
            .with_fact_write(FactKeyPattern::parse("/facts/serving/>").expect("serving pattern"));
        let session_a = authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("routing-panda-a"),
            grant.clone(),
        );
        let session_b = authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("routing-panda-b"),
            grant,
        );
        let author_a = Arc::new(PandaFactAuthor::new(session_a.principal().clone()));
        let author_b = Arc::new(PandaFactAuthor::new(session_b.principal().clone()));
        (
            SharedStore::new(PandaFactStore::new(Arc::new(raw_bus))),
            session_a,
            session_b,
            author_a,
            author_b,
        )
    }

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

    #[tokio::test]
    async fn serving_writer_records_serving_outcomes() {
        let (store, session, _, author, _) = fixture();
        let writer = PandaServingFactWriter::new(store, session, author);
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
    async fn serving_writer_maps_conflict_to_serving_fact_conflict() {
        let (store, session_a, session_b, author_a, author_b) = fixture();
        let writer_a = PandaServingFactWriter::new(store.clone(), session_a, author_a);
        let writer_b = PandaServingFactWriter::new(store, session_b, author_b);
        let original = serving_commit();
        writer_a
            .write_serving_commit(&original)
            .await
            .expect("insert serving");
        let mut conflicting = original.clone();
        conflicting.epoch += 1;

        let error = writer_b
            .write_serving_commit(&conflicting)
            .await
            .expect_err("conflict serving");

        assert!(matches!(
            error,
            RoutingError::ServingFactConflict { key }
                if key == mvp_routing::serving_commit_fact_key(&original.serving_commit_id)
                    .expect("serving key")
        ));
    }
}
