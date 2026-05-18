use mvp_bus::{FactKey, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactStore, PandaFactWireEnvelope, SharedPandaFactStore,
};

use crate::{
    PandaNetFactImportOutcome, PandaNetFactImportRejection, PandaNetFactNode,
    PandaNetFactNodeConfig, PandaNetNetworkId, PandaNetNode, PandaNetNodeConfig, PandaNetNodeSeed,
    PandaNetTopic, import_fact_body_into_shared_store,
};

#[tokio::test]
async fn owned_nodes_sync_one_opaque_body_with_explicit_bootstrap() {
    let topic = PandaNetTopic::new([42; 32]);
    let network_id = PandaNetNetworkId::new([7; 32]);
    let receiver = PandaNetNode::spawn(PandaNetNodeConfig::localhost_ephemeral(
        network_id,
        PandaNetNodeSeed::new([1; 32]),
        Vec::new(),
    ))
    .await
    .expect("spawn receiver node");
    let receiver_info = receiver.node_info();
    let mut receiver_stream = receiver
        .open_stream(topic, true)
        .await
        .expect("open receiver stream");

    let mut sender = PandaNetNode::spawn(PandaNetNodeConfig::localhost_ephemeral(
        network_id,
        PandaNetNodeSeed::new([2; 32]),
        vec![receiver_info],
    ))
    .await
    .expect("spawn sender node");
    sender
        .append_to_topic(topic, b"transport-body")
        .await
        .expect("append sender body");
    let _sender_stream = sender
        .open_stream(topic, true)
        .await
        .expect("open sender stream");

    let body = receiver_stream
        .next_body()
        .await
        .expect("receiver gets body");
    assert_eq!(body, b"transport-body");
}

#[tokio::test]
async fn fact_nodes_import_into_shared_store_and_report_status() {
    let fixture = FactNodeFixture::new("fact-node-import");
    let mut receiver = fixture
        .node([21; 32], Vec::new(), ReplicaTrust::Trusted)
        .await
        .expect("spawn receiver fact node");
    let receiver_info = receiver.node_info();
    let mut sender = fixture
        .node([22; 32], vec![receiver_info], ReplicaTrust::Trusted)
        .await
        .expect("spawn sender fact node");

    sender
        .publish_fact_payload(
            &fixture.writer,
            &fixture.author,
            key("/facts/node/node-1/joined/1"),
            "node-one".to_string().into(),
        )
        .await
        .expect("publish inserted fact");
    let duplicate_operation = sender
        .store()
        .export_operations()
        .await
        .last()
        .cloned()
        .expect("sender has inserted operation");
    sender
        .publish_operation(&duplicate_operation)
        .await
        .expect("publish duplicate operation");
    sender
        .publish_fact_payload(
            &fixture.writer,
            &fixture.author,
            key("/facts/node/node-1/joined/1"),
            "node-one-conflict".to_string().into(),
        )
        .await
        .expect("publish conflict fact");
    sender
        .publish_body(b"bad-envelope".to_vec())
        .await
        .expect("publish malformed body");

    let report = receiver
        .import_until_attempted(4)
        .await
        .expect("receiver imports fact-node stream");

    assert_eq!(report.imported, 1);
    assert_eq!(report.duplicate, 1);
    assert_eq!(report.conflict, 1);
    assert!(report.deferred.is_empty());
    assert!(report.failed.is_empty());
    assert!(
        report.rejected.iter().any(|rejection| matches!(
            rejection,
            PandaNetFactImportRejection::MalformedEnvelope(_)
        ))
    );
    assert_eq!(receiver.store().export_operations().await.len(), 2);
}

#[tokio::test]
async fn fact_node_requires_trusted_replica_session() {
    let fixture = FactNodeFixture::new("fact-node-replica-auth");
    let mut receiver = fixture
        .node([31; 32], Vec::new(), ReplicaTrust::Untrusted)
        .await
        .expect("spawn receiver without replica trust");
    let receiver_info = receiver.node_info();
    let mut sender = fixture
        .node([32; 32], vec![receiver_info], ReplicaTrust::Trusted)
        .await
        .expect("spawn sender fact node");

    sender
        .publish_fact_payload(
            &fixture.writer,
            &fixture.author,
            key("/facts/node/node-1/joined/1"),
            "node-one".to_string().into(),
        )
        .await
        .expect("publish fact");

    let outcome = receiver
        .import_next_fact()
        .await
        .expect("receiver reads one fact body");

    assert!(matches!(
        outcome,
        PandaNetFactImportOutcome::Rejected(
            PandaNetFactImportRejection::UnauthorizedReplica { .. }
        )
    ));
    assert!(receiver.store().export_operations().await.is_empty());
}

#[tokio::test]
async fn fact_node_rejects_oversized_envelope_before_import() {
    let fixture = FactNodeFixture::new("fact-node-size-cap");
    let receiver_store = fixture.trusted_store(ReplicaTrust::Trusted).await;
    let mut receiver = PandaNetFactNode::spawn(
        PandaNetFactNodeConfig::new(
            PandaNetNodeConfig::localhost_ephemeral(
                fixture.network_id,
                PandaNetNodeSeed::new([35; 32]),
                Vec::new(),
            ),
            fixture.topic,
            receiver_store,
            fixture.replica.clone(),
        )
        .with_max_fact_envelope_bytes(4),
    )
    .await
    .expect("spawn capped receiver fact node");
    let receiver_info = receiver.node_info();
    let mut sender = fixture
        .node([36; 32], vec![receiver_info], ReplicaTrust::Trusted)
        .await
        .expect("spawn sender fact node");

    sender
        .publish_body(b"too-large".to_vec())
        .await
        .expect("publish oversized body");

    let outcome = receiver
        .import_next_fact()
        .await
        .expect("receiver reads oversized body");

    assert!(matches!(
        outcome,
        PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::EnvelopeTooLarge {
            size: 9,
            max: 4,
        })
    ));
    assert!(receiver.store().export_operations().await.is_empty());
}

#[tokio::test]
async fn fact_node_retries_deferred_import_after_predecessor_arrives() {
    let fixture = FactNodeFixture::new("fact-node-out-of-order");
    let mut receiver = fixture
        .node([33; 32], Vec::new(), ReplicaTrust::Trusted)
        .await
        .expect("spawn receiver fact node");
    let receiver_info = receiver.node_info();
    let mut sender = fixture
        .node([34; 32], vec![receiver_info], ReplicaTrust::Trusted)
        .await
        .expect("spawn sender fact node");
    let operations = fixture
        .two_linked_operations()
        .await
        .expect("create linked operations");

    sender
        .publish_operation(&operations[1])
        .await
        .expect("publish child operation first");
    sender
        .publish_operation(&operations[0])
        .await
        .expect("publish predecessor second");

    let report = receiver
        .import_until_attempted(2)
        .await
        .expect("receiver imports out-of-order stream");

    assert_eq!(report.deferred.len(), 1);
    assert_eq!(report.imported, 1);
    assert_eq!(receiver.store().export_operations().await.len(), 2);
}

#[tokio::test]
async fn import_reports_author_key_mismatch_as_authorization_rejection() {
    let fixture = FactNodeFixture::new("fact-node-author-mismatch");
    let trusted_store = fixture.trusted_store(ReplicaTrust::Trusted).await;
    let impostor = PandaFactAuthor::new(fixture.writer.principal().clone());
    let mut source = fixture.raw_store();
    source
        .write_fact_payload(
            &fixture.writer,
            &impostor,
            key("/facts/node/mismatch/joined/1"),
            "mismatch".to_string().into(),
        )
        .await
        .expect("write mismatched author operation");
    let body = source
        .export_operations()
        .last()
        .map(PandaFactWireEnvelope::encode)
        .expect("source operation exists");

    let outcome = import_fact_body_into_shared_store(&body, &trusted_store, &fixture.replica).await;

    assert!(matches!(
        outcome,
        PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::AuthorKeyMismatch {
            principal,
            ..
        }) if principal == *fixture.writer.principal()
    ));
}

#[derive(Clone, Copy)]
enum ReplicaTrust {
    Trusted,
    Untrusted,
}

struct FactNodeFixture {
    island: IslandId,
    writer: mvp_bus::BusSession,
    replica: mvp_bus::BusSession,
    author: PandaFactAuthor,
    network_id: PandaNetNetworkId,
    topic: PandaNetTopic,
    bus: InMemoryBus,
}

impl FactNodeFixture {
    fn new(label: &str) -> Self {
        let island = IslandId::new(format!("prod-{label}"));
        let (bus, authority) = InMemoryBus::new_with_authority();
        let writer = authority.grant_in(
            island.clone(),
            PrincipalId::new(format!("{label}-writer")),
            Grant::empty().with_fact_write(pattern("/facts/>")),
        );
        let replica = authority.grant_in(
            island.clone(),
            PrincipalId::new(format!("{label}-replica")),
            Grant::empty(),
        );
        Self {
            island,
            author: PandaFactAuthor::new(writer.principal().clone()),
            writer,
            replica,
            network_id: PandaNetNetworkId::new([41; 32]),
            topic: PandaNetTopic::new([42; 32]),
            bus,
        }
    }

    async fn node(
        &self,
        seed: [u8; 32],
        bootstrap: Vec<crate::PandaNetNodeInfo>,
        trust: ReplicaTrust,
    ) -> Result<PandaNetFactNode, crate::PandaNetTransportError> {
        let store = self.trusted_store(trust).await;
        PandaNetFactNode::spawn(PandaNetFactNodeConfig::new(
            PandaNetNodeConfig::localhost_ephemeral(
                self.network_id,
                PandaNetNodeSeed::new(seed),
                bootstrap,
            ),
            self.topic,
            store,
            self.replica.clone(),
        ))
        .await
    }

    async fn trusted_store(&self, trust: ReplicaTrust) -> SharedPandaFactStore {
        let store = SharedPandaFactStore::new(self.raw_store());
        store
            .trust_author_key(
                &self.island,
                self.writer.principal().clone(),
                self.author.author_key(),
            )
            .await
            .expect("trust fact-node writer key");
        match trust {
            ReplicaTrust::Trusted => {
                store
                    .trust_replica_peer(&self.island, self.replica.principal().clone())
                    .await;
            }
            ReplicaTrust::Untrusted => {}
        }
        store
    }

    fn raw_store(&self) -> PandaFactStore {
        PandaFactStore::new(std::sync::Arc::new(self.bus.clone()))
    }

    async fn two_linked_operations(
        &self,
    ) -> mvp_p2panda_facts::Result<Vec<mvp_p2panda_facts::PandaFactOperation>> {
        let mut store = self.raw_store();
        store
            .write_fact_payload(
                &self.writer,
                &self.author,
                key("/facts/node/first/joined/1"),
                "first".to_string().into(),
            )
            .await?;
        store
            .write_fact_payload(
                &self.writer,
                &self.author,
                key("/facts/node/second/joined/1"),
                "second".to_string().into(),
            )
            .await?;
        Ok(store.export_operations().cloned().collect())
    }
}

fn key(value: &str) -> FactKey {
    FactKey::parse(value).expect("fact key parses")
}

fn pattern(value: &str) -> mvp_bus::FactKeyPattern {
    mvp_bus::FactKeyPattern::parse(value).expect("fact key pattern parses")
}
