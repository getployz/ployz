use mvp_bus::{FactKey, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_p2panda_facts::{PandaFactAuthor, PandaFactStore, SharedPandaFactStore};

use crate::{
    PandaNetConfigError, PandaNetFactImportFailure, PandaNetFactImportOutcome,
    PandaNetFactImportRejection, PandaNetFactNode, PandaNetFactNodeConfig, PandaNetNetworkId,
    PandaNetNode, PandaNetNodeConfig, PandaNetNodeSeed, PandaNetNodeTicket, PandaNetTopic,
    import_fact_body_into_shared_store,
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
async fn node_ticket_round_trips_and_bootstraps_fact_node() {
    let fixture = FactNodeFixture::new("fact-node-ticket");
    let mut receiver = fixture
        .node([45; 32], Vec::new(), ReplicaTrust::Trusted)
        .await
        .expect("spawn receiver fact node");
    let ticket = receiver
        .node_info()
        .to_ticket()
        .expect("encode receiver node ticket");
    let encoded = ticket.as_str().to_string();
    let decoded = PandaNetNodeTicket::parse(&encoded).expect("decode receiver node ticket");
    let mut sender = fixture
        .node(
            [46; 32],
            vec![decoded.into_node_info().expect("ticket has node info")],
            ReplicaTrust::Trusted,
        )
        .await
        .expect("spawn sender from ticket bootstrap");

    sender
        .publish_fact_payload(
            &fixture.writer,
            &fixture.author,
            key("/facts/node/ticket-node/joined/1"),
            "ticket-node".to_string().into(),
        )
        .await
        .expect("publish fact through ticket bootstrap");

    let report = receiver
        .import_until_attempted(1)
        .await
        .expect("receiver imports ticket-bootstrapped fact");
    assert_eq!(report.imported, 1);
}

#[test]
fn hex_config_helpers_round_trip_and_reject_invalid_values() {
    let hex = "11".repeat(32);
    let network = PandaNetNetworkId::parse_hex(&hex).expect("parse network id");
    let seed = PandaNetNodeSeed::parse_hex(&hex).expect("parse node seed");
    let topic = PandaNetTopic::parse_hex(&hex).expect("parse topic");

    assert_eq!(network.as_hex(), hex);
    assert_eq!(seed.as_hex(), hex);
    assert_eq!(topic.as_hex(), hex);
    assert!(matches!(
        PandaNetNetworkId::parse_hex("123"),
        Err(PandaNetConfigError::InvalidHexLength { .. })
    ));
    assert!(matches!(
        PandaNetTopic::parse_hex(&"GG".repeat(32)),
        Err(PandaNetConfigError::InvalidHex { .. })
    ));
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
    sender
        .publish_fact_payload(
            &fixture.writer,
            &fixture.author,
            key("/facts/node/node-1/joined/1"),
            "node-one-conflict".to_string().into(),
        )
        .await
        .expect("publish conflict fact");
    let report = receiver
        .import_until_attempted(2)
        .await
        .expect("receiver imports fact-node stream");

    assert_eq!(report.imported, 1);
    assert_eq!(report.conflict, 1);
    assert_eq!(report.duplicate, 0);
    assert!(report.deferred.is_empty());
    assert!(report.failed.is_empty());
    assert!(report.rejected.is_empty());
    assert_eq!(receiver.store().export_operations().await.len(), 2);
}

#[tokio::test]
async fn fact_node_receives_after_refreshing_stream() {
    let fixture = FactNodeFixture::new("fact-node-refresh");
    let mut receiver = fixture
        .node([25; 32], Vec::new(), ReplicaTrust::Trusted)
        .await
        .expect("spawn receiver fact node");
    let receiver_info = receiver.node_info();
    let mut sender = fixture
        .node([26; 32], vec![receiver_info], ReplicaTrust::Trusted)
        .await
        .expect("spawn sender fact node");

    sender
        .publish_fact_payload(
            &fixture.writer,
            &fixture.author,
            key("/facts/node/before-refresh/joined/1"),
            "before".to_string().into(),
        )
        .await
        .expect("publish first fact");
    assert_eq!(
        receiver
            .import_next_fact()
            .await
            .expect("import before refresh"),
        PandaNetFactImportOutcome::Imported
    );

    receiver.refresh_stream().await.expect("refresh stream");
    sender
        .publish_fact_payload(
            &fixture.writer,
            &fixture.author,
            key("/facts/node/after-refresh/joined/1"),
            "after".to_string().into(),
        )
        .await
        .expect("publish second fact");

    assert_eq!(
        receiver
            .import_next_fact()
            .await
            .expect("import after refresh"),
        PandaNetFactImportOutcome::Imported
    );
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
async fn fact_node_rejects_oversized_operation_before_import() {
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
        .with_max_fact_operation_bytes(4),
    )
    .await
    .expect("spawn capped receiver fact node");
    let receiver_info = receiver.node_info();
    let mut sender = fixture
        .node([36; 32], vec![receiver_info], ReplicaTrust::Trusted)
        .await
        .expect("spawn sender fact node");

    sender
        .publish_fact_payload(
            &fixture.writer,
            &fixture.author,
            key("/facts/node/too-large/joined/1"),
            "too-large".to_string().into(),
        )
        .await
        .expect("publish oversized fact");

    let outcome = receiver
        .import_next_fact()
        .await
        .expect("receiver reads oversized operation");

    assert!(matches!(
        outcome,
        PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::OperationTooLarge {
            size,
            max: 4,
        }) if size > 4
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
    let sender = fixture
        .node([34; 32], vec![receiver_info], ReplicaTrust::Trusted)
        .await
        .expect("spawn sender fact node");
    let operations = fixture
        .linked_operations_from_store(&sender.store(), 2)
        .await
        .expect("create linked operations");

    let child_outcome = receiver
        .import_operation_for_test(operations[1].clone())
        .await;
    let parent_outcome = receiver
        .import_operation_for_test(operations[0].clone())
        .await;
    let retry_outcomes = receiver.retry_pending_imports_for_test().await;

    assert!(matches!(
        child_outcome,
        PandaNetFactImportOutcome::Deferred(_)
    ));
    assert_eq!(parent_outcome, PandaNetFactImportOutcome::Imported);
    assert_eq!(retry_outcomes, vec![PandaNetFactImportOutcome::Imported]);
    assert_eq!(receiver.store().export_operations().await.len(), 2);
}

#[tokio::test]
async fn fact_node_reports_pending_queue_full() {
    let fixture = FactNodeFixture::new("fact-node-pending-cap");
    let receiver_store = fixture.trusted_store(ReplicaTrust::Trusted).await;
    let mut receiver = PandaNetFactNode::spawn(
        PandaNetFactNodeConfig::new(
            PandaNetNodeConfig::localhost_ephemeral(
                fixture.network_id,
                PandaNetNodeSeed::new([43; 32]),
                Vec::new(),
            ),
            fixture.topic,
            receiver_store,
            fixture.replica.clone(),
        )
        .with_max_pending_imports(0),
    )
    .await
    .expect("spawn receiver with no pending capacity");
    let receiver_info = receiver.node_info();
    let sender = fixture
        .node([44; 32], vec![receiver_info], ReplicaTrust::Trusted)
        .await
        .expect("spawn sender fact node");
    let operations = fixture
        .linked_operations_from_store(&sender.store(), 2)
        .await
        .expect("create linked operations");

    let outcome = receiver
        .import_operation_for_test(operations[1].clone())
        .await;

    assert!(matches!(
        outcome,
        PandaNetFactImportOutcome::Failed(PandaNetFactImportFailure::PendingQueueFull { max: 0 })
    ));
    assert!(receiver.store().export_operations().await.is_empty());
}

#[tokio::test]
async fn fact_node_retries_transitive_deferred_imports_until_no_progress() {
    let fixture = FactNodeFixture::new("fact-node-transitive");
    let mut receiver = fixture
        .node([37; 32], Vec::new(), ReplicaTrust::Trusted)
        .await
        .expect("spawn receiver fact node");
    let receiver_info = receiver.node_info();
    let sender = fixture
        .node([38; 32], vec![receiver_info], ReplicaTrust::Trusted)
        .await
        .expect("spawn sender fact node");
    let operations = fixture
        .linked_operations_from_store(&sender.store(), 3)
        .await
        .expect("create linked operations");

    let grandchild_outcome = receiver
        .import_operation_for_test(operations[2].clone())
        .await;
    let child_outcome = receiver
        .import_operation_for_test(operations[1].clone())
        .await;
    let parent_outcome = receiver
        .import_operation_for_test(operations[0].clone())
        .await;
    let retry_outcomes = receiver.retry_pending_imports_for_test().await;

    assert!(matches!(
        grandchild_outcome,
        PandaNetFactImportOutcome::Deferred(_)
    ));
    assert!(matches!(
        child_outcome,
        PandaNetFactImportOutcome::Deferred(_)
    ));
    assert_eq!(parent_outcome, PandaNetFactImportOutcome::Imported);
    assert_eq!(
        retry_outcomes,
        vec![
            PandaNetFactImportOutcome::Imported,
            PandaNetFactImportOutcome::Imported
        ]
    );
    assert_eq!(receiver.store().export_operations().await.len(), 3);
}

#[tokio::test]
async fn fact_node_republishes_already_present_fact_operations() {
    let fixture = FactNodeFixture::new("fact-node-republish");
    let mut receiver = fixture
        .node([39; 32], Vec::new(), ReplicaTrust::Trusted)
        .await
        .expect("spawn receiver fact node");
    let receiver_info = receiver.node_info();
    let mut sender = fixture
        .node([40; 32], vec![receiver_info], ReplicaTrust::Trusted)
        .await
        .expect("spawn sender fact node");

    for _ in 0..2 {
        sender
            .publish_fact_payload(
                &fixture.writer,
                &fixture.author,
                key("/facts/node/node-1/joined/1"),
                "node-one".to_string().into(),
            )
            .await
            .expect("publish idempotent fact");
    }

    let report = receiver
        .import_until_attempted(1)
        .await
        .expect("receiver imports original operation");

    assert_eq!(report.imported, 1);
    assert_eq!(report.duplicate, 0);
    assert_eq!(receiver.store().export_operations().await.len(), 1);
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
        .map(mvp_p2panda_facts::PandaFactWireEnvelope::encode)
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
            network_id: PandaNetNetworkId::new(stable_test_id(label, b'n')),
            topic: PandaNetTopic::new(stable_test_id(label, b't')),
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

    async fn linked_operations_from_store(
        &self,
        store: &SharedPandaFactStore,
        count: usize,
    ) -> mvp_p2panda_facts::Result<Vec<mvp_p2panda_facts::PandaFactOperation>> {
        for index in 0..count {
            store
                .write_fact_payload(
                    &self.writer,
                    &self.author,
                    key(&format!("/facts/node/linked-{index}/joined/1")),
                    format!("linked-{index}").into(),
                )
                .await?;
        }
        Ok(store.export_operations().await)
    }
}

fn key(value: &str) -> FactKey {
    FactKey::parse(value).expect("fact key parses")
}

fn pattern(value: &str) -> mvp_bus::FactKeyPattern {
    mvp_bus::FactKeyPattern::parse(value).expect("fact key pattern parses")
}

fn stable_test_id(label: &str, salt: u8) -> [u8; 32] {
    let mut bytes = [salt; 32];
    for (index, byte) in label.as_bytes().iter().enumerate() {
        let slot = index % bytes.len();
        bytes[slot] = bytes[slot]
            .wrapping_mul(31)
            .wrapping_add(*byte)
            .wrapping_add(index as u8);
    }
    bytes
}
