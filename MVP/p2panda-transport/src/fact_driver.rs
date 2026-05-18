use mvp_bus::{BusSession, FactKey, IslandId, PrincipalId};
use mvp_p2panda_facts::{
    PandaFactError, PandaFactStore, PandaFactWireEnvelope, PandaFactWireEnvelopeError,
    PandaFactWriteOutcome,
};

#[cfg(feature = "harness")]
use crate::{PandaNetNode, PandaNetNodeConfig, PandaNetTopic};
use crate::{PandaNetStream, PandaNetTransportError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(feature = "harness")]
pub struct PandaNetWireTransportConfig {
    network_id: [u8; 32],
    topic: PandaNetTopic,
    receiver_seed: [u8; 32],
    sender_seed: [u8; 32],
}

#[cfg(feature = "harness")]
impl PandaNetWireTransportConfig {
    #[must_use]
    pub fn new(
        network_id: [u8; 32],
        topic: [u8; 32],
        receiver_seed: [u8; 32],
        sender_seed: [u8; 32],
    ) -> Self {
        Self {
            network_id,
            topic: PandaNetTopic::new(topic),
            receiver_seed,
            sender_seed,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
#[cfg(feature = "harness")]
pub struct PandaNetFactTransportReport {
    pub replayed: usize,
    pub imported: usize,
    pub duplicate: usize,
    pub conflict: usize,
    pub deferred: Vec<PandaNetFactImportDeferred>,
    pub rejected: Vec<PandaNetFactImportRejection>,
    pub failed: Vec<PandaNetFactImportFailure>,
}

#[cfg(feature = "harness")]
impl PandaNetFactTransportReport {
    #[must_use]
    pub fn new(replayed: usize) -> Self {
        Self {
            replayed,
            imported: 0,
            duplicate: 0,
            conflict: 0,
            deferred: Vec::new(),
            rejected: Vec::new(),
            failed: Vec::new(),
        }
    }

    fn record(&mut self, outcome: PandaNetFactImportOutcome) {
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

#[derive(Debug, PartialEq, Eq)]
pub enum PandaNetFactImportOutcome {
    Imported,
    Duplicate,
    Conflict,
    Deferred(PandaNetFactImportDeferred),
    Failed(PandaNetFactImportFailure),
    Rejected(PandaNetFactImportRejection),
}

#[derive(Debug, PartialEq, Eq)]
pub enum PandaNetFactImportDeferred {
    OutOfOrder {
        island: IslandId,
        principal: PrincipalId,
        key: FactKey,
        missing_operations: u64,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum PandaNetFactImportFailure {
    LocalIngest { message: String },
    LocalStore { message: String },
    InvalidStorePath { message: String },
    MissingPayload { key: FactKey },
}

#[derive(Debug, PartialEq, Eq)]
pub enum PandaNetFactImportRejection {
    MalformedEnvelope(PandaFactWireEnvelopeError),
    UnauthorizedReplica {
        island: IslandId,
        principal: PrincipalId,
    },
    UntrustedAuthor {
        island: IslandId,
        principal: PrincipalId,
    },
    CrossIsland {
        session: IslandId,
        operation: IslandId,
    },
    InvalidOperation,
    InvalidExtensions,
}

pub async fn import_next_fact(
    stream: &mut PandaNetStream,
    store: &mut PandaFactStore,
    replica_session: &BusSession,
) -> Result<PandaNetFactImportOutcome, PandaNetTransportError> {
    let body = stream.next_body().await?;
    Ok(import_fact_body(&body, store, replica_session).await)
}

pub async fn import_fact_body(
    body: &[u8],
    store: &mut PandaFactStore,
    replica_session: &BusSession,
) -> PandaNetFactImportOutcome {
    let operation = match PandaFactWireEnvelope::decode(body) {
        Ok(operation) => operation,
        Err(error) => {
            return PandaNetFactImportOutcome::Rejected(
                PandaNetFactImportRejection::MalformedEnvelope(error),
            );
        }
    };

    match store
        .import_replica_operation(replica_session, &operation)
        .await
    {
        Ok(PandaFactWriteOutcome::Inserted(_)) => PandaNetFactImportOutcome::Imported,
        Ok(PandaFactWriteOutcome::AlreadyPresent(_)) => PandaNetFactImportOutcome::Duplicate,
        Ok(PandaFactWriteOutcome::Conflict(_)) => PandaNetFactImportOutcome::Conflict,
        Err(error) => classify_fact_error(error),
    }
}

#[cfg(feature = "harness")]
pub async fn transport_exported_facts(
    source: &PandaFactStore,
    target: &mut PandaFactStore,
    replica_session: &BusSession,
    config: PandaNetWireTransportConfig,
) -> Result<PandaNetFactTransportReport, PandaNetTransportError> {
    let wires = source
        .export_operations()
        .map(PandaFactWireEnvelope::encode)
        .collect::<Vec<_>>();
    let bodies = transport_wire_bodies(config, wires).await?;
    let mut report = PandaNetFactTransportReport::new(bodies.len());
    for body in bodies {
        report.record(import_fact_body(&body, target, replica_session).await);
    }
    Ok(report)
}

#[cfg(feature = "harness")]
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

#[cfg(feature = "harness")]
struct PandaNetWireHarness {
    _receiver: PandaNetNode,
    _sender: PandaNetNode,
    _sender_stream: PandaNetStream,
    receiver_stream: PandaNetStream,
}

#[cfg(feature = "harness")]
impl PandaNetWireHarness {
    async fn spawn(
        config: PandaNetWireTransportConfig,
        bodies: Vec<Vec<u8>>,
    ) -> Result<Self, PandaNetTransportError> {
        let receiver = PandaNetNode::spawn(PandaNetNodeConfig::localhost_ephemeral(
            config.network_id,
            config.receiver_seed,
            Vec::new(),
        )?)
        .await?;
        let receiver_info = receiver.node_info();
        let receiver_stream = receiver.open_stream(config.topic, true).await?;
        let mut sender = PandaNetNode::spawn(PandaNetNodeConfig::localhost_ephemeral(
            config.network_id,
            config.sender_seed,
            vec![receiver_info],
        )?)
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

fn classify_fact_error(error: PandaFactError) -> PandaNetFactImportOutcome {
    match error {
        PandaFactError::UnauthorizedReplicaImport { island, principal } => {
            PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::UnauthorizedReplica {
                island,
                principal,
            })
        }
        PandaFactError::UntrustedAuthorKey {
            island, principal, ..
        } => PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::UntrustedAuthor {
            island,
            principal,
        }),
        PandaFactError::ImportIslandMismatch { session, operation } => {
            PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::CrossIsland {
                session,
                operation,
            })
        }
        PandaFactError::InvalidOperation => {
            PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::InvalidOperation)
        }
        PandaFactError::InvalidExtensions { .. } => {
            PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::InvalidExtensions)
        }
        PandaFactError::Ingest(error) => {
            PandaNetFactImportOutcome::Failed(PandaNetFactImportFailure::LocalIngest {
                message: error.to_string(),
            })
        }
        PandaFactError::Store { message } => {
            PandaNetFactImportOutcome::Failed(PandaNetFactImportFailure::LocalStore { message })
        }
        PandaFactError::InvalidStorePath { path, message } => {
            PandaNetFactImportOutcome::Failed(PandaNetFactImportFailure::InvalidStorePath {
                message: format!("{}: {message}", path.display()),
            })
        }
        PandaFactError::MissingPayload { key } => {
            PandaNetFactImportOutcome::Failed(PandaNetFactImportFailure::MissingPayload { key })
        }
        PandaFactError::OutOfOrderOperation {
            island,
            principal,
            key,
            missing_operations,
        } => PandaNetFactImportOutcome::Deferred(PandaNetFactImportDeferred::OutOfOrder {
            island,
            principal,
            key,
            missing_operations,
        }),
        PandaFactError::InvalidAuthorKey { .. }
        | PandaFactError::AuthorKeyMismatch { .. }
        | PandaFactError::OutdatedOperation { .. }
        | PandaFactError::PrincipalMismatch { .. }
        | PandaFactError::UnauthorizedWrite { .. } => {
            PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::InvalidOperation)
        }
    }
}
