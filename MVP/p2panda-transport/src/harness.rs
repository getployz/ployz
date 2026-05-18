use mvp_bus::BusSession;
use mvp_p2panda_facts::PandaFactStore;

use crate::{
    PandaNetFactImportDeferred, PandaNetFactImportFailure, PandaNetFactImportOutcome,
    PandaNetFactImportRejection, PandaNetNetworkId, PandaNetNode, PandaNetNodeConfig,
    PandaNetNodeSeed, PandaNetStream, PandaNetTopic, PandaNetTransportError, import_fact_body,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PandaNetWireTransportConfig {
    network_id: PandaNetNetworkId,
    topic: PandaNetTopic,
    receiver_seed: PandaNetNodeSeed,
    sender_seed: PandaNetNodeSeed,
}

impl PandaNetWireTransportConfig {
    #[must_use]
    pub const fn new(
        network_id: PandaNetNetworkId,
        topic: PandaNetTopic,
        receiver_seed: PandaNetNodeSeed,
        sender_seed: PandaNetNodeSeed,
    ) -> Self {
        Self {
            network_id,
            topic,
            receiver_seed,
            sender_seed,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct PandaNetFactImportReport {
    pub attempted: usize,
    pub imported: usize,
    pub duplicate: usize,
    pub conflict: usize,
    pub deferred: Vec<PandaNetFactImportDeferred>,
    pub rejected: Vec<PandaNetFactImportRejection>,
    pub failed: Vec<PandaNetFactImportFailure>,
}

impl PandaNetFactImportReport {
    #[must_use]
    pub fn new(attempted: usize) -> Self {
        Self {
            attempted,
            imported: 0,
            duplicate: 0,
            conflict: 0,
            deferred: Vec::new(),
            rejected: Vec::new(),
            failed: Vec::new(),
        }
    }

    pub fn record(&mut self, outcome: PandaNetFactImportOutcome) {
        match outcome {
            PandaNetFactImportOutcome::Imported => {
                self.imported += 1;
            }
            PandaNetFactImportOutcome::Duplicate => {
                self.duplicate += 1;
            }
            PandaNetFactImportOutcome::Conflict => {
                self.conflict += 1;
            }
            PandaNetFactImportOutcome::Deferred(deferred) => {
                self.deferred.push(deferred);
            }
            PandaNetFactImportOutcome::Rejected(rejected) => {
                self.rejected.push(rejected);
            }
            PandaNetFactImportOutcome::Failed(failed) => {
                self.failed.push(failed);
            }
        }
    }
}

pub async fn import_fact_bodies(
    bodies: Vec<Vec<u8>>,
    store: &mut PandaFactStore,
    replica_session: &BusSession,
) -> PandaNetFactImportReport {
    let mut report = PandaNetFactImportReport::new(bodies.len());
    for body in bodies {
        report.record(import_fact_body(&body, store, replica_session).await);
    }
    report
}

pub async fn transport_wire_bodies(
    config: PandaNetWireTransportConfig,
    bodies: Vec<Vec<u8>>,
) -> Result<Vec<Vec<u8>>, PandaNetTransportError> {
    let expected = bodies.len();
    let mut harness = PandaNetWireHarness::spawn(config, bodies).await?;
    let mut received = Vec::with_capacity(expected);
    for _ in 0..expected {
        received.push(harness.receiver_stream.next_body().await?);
    }
    Ok(received)
}

struct PandaNetWireHarness {
    _receiver: PandaNetNode,
    _sender: PandaNetNode,
    _sender_stream: PandaNetStream,
    receiver_stream: PandaNetStream,
}

impl PandaNetWireHarness {
    async fn spawn(
        config: PandaNetWireTransportConfig,
        bodies: Vec<Vec<u8>>,
    ) -> Result<Self, PandaNetTransportError> {
        let receiver = PandaNetNode::spawn(PandaNetNodeConfig::localhost_ephemeral(
            config.network_id,
            config.receiver_seed,
            Vec::new(),
        ))
        .await?;
        let receiver_info = receiver.node_info();
        let receiver_stream = receiver.open_stream(config.topic, true).await?;
        let mut sender = PandaNetNode::spawn(PandaNetNodeConfig::localhost_ephemeral(
            config.network_id,
            config.sender_seed,
            vec![receiver_info],
        ))
        .await?;
        for body in bodies {
            sender.append_to_topic(config.topic, &body).await?;
        }
        let sender_stream = sender.open_stream(config.topic, true).await?;
        Ok(Self {
            _receiver: receiver,
            _sender: sender,
            _sender_stream: sender_stream,
            receiver_stream,
        })
    }
}
