use std::collections::VecDeque;

use mvp_bus::{BusSession, FactKey, FactPayload};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactOperation, PandaFactWireEnvelope, PandaFactWriteOutcome,
    SharedPandaFactStore,
};

use crate::{
    PandaNetFactImportOutcome, PandaNetFactImportRejection, PandaNetFactImportReport, PandaNetNode,
    PandaNetNodeConfig, PandaNetNodeInfo, PandaNetStream, PandaNetTopic, PandaNetTransportError,
    import_fact_body_into_shared_store,
};

const DEFAULT_MAX_FACT_ENVELOPE_BYTES: usize = 8 * 1024 * 1024;

pub struct PandaNetFactNode {
    node: PandaNetNode,
    stream: PandaNetStream,
    topic: PandaNetTopic,
    store: SharedPandaFactStore,
    replica_session: BusSession,
    pending_imports: VecDeque<Vec<u8>>,
    max_fact_envelope_bytes: usize,
}

impl PandaNetFactNode {
    pub async fn spawn(config: PandaNetFactNodeConfig) -> Result<Self, PandaNetTransportError> {
        let node = PandaNetNode::spawn(config.node).await?;
        let stream = node.open_stream(config.topic, true).await?;
        Ok(Self {
            node,
            stream,
            topic: config.topic,
            store: config.store,
            replica_session: config.replica_session,
            pending_imports: VecDeque::new(),
            max_fact_envelope_bytes: config.max_fact_envelope_bytes,
        })
    }

    #[must_use]
    pub fn node_info(&self) -> PandaNetNodeInfo {
        self.node.node_info()
    }

    #[must_use]
    pub fn store(&self) -> SharedPandaFactStore {
        self.store.clone()
    }

    pub async fn publish_fact_payload(
        &mut self,
        session: &BusSession,
        author: &PandaFactAuthor,
        key: FactKey,
        payload: FactPayload,
    ) -> Result<PandaFactWriteOutcome, PandaNetTransportError> {
        let write = self
            .store
            .write_fact_payload_with_operation(session, author, key, payload)
            .await
            .map_err(|error| PandaNetTransportError::FactStore {
                message: error.to_string(),
            })?;
        if let Some(operation) = write.operation() {
            self.publish_operation(operation).await?;
        } else if !matches!(write.outcome(), PandaFactWriteOutcome::AlreadyPresent(_)) {
            return Err(PandaNetTransportError::MissingLocalOperation);
        }
        Ok(write.into_outcome())
    }

    pub async fn publish_operation(
        &mut self,
        operation: &PandaFactOperation,
    ) -> Result<(), PandaNetTransportError> {
        self.publish_body(PandaFactWireEnvelope::encode(operation))
            .await
    }

    pub async fn publish_body(&mut self, body: Vec<u8>) -> Result<(), PandaNetTransportError> {
        self.node.append_to_topic(self.topic, &body).await
    }

    pub async fn import_next_fact(
        &mut self,
    ) -> Result<PandaNetFactImportOutcome, PandaNetTransportError> {
        let body = self.stream.next_body().await?;
        let outcome = self.import_body(body).await;
        if import_can_unblock_pending(&outcome) {
            self.retry_pending_imports().await;
        }
        Ok(outcome)
    }

    pub async fn import_until_attempted(
        &mut self,
        attempted: usize,
    ) -> Result<PandaNetFactImportReport, PandaNetTransportError> {
        let mut report = PandaNetFactImportReport::new(attempted);
        for _ in 0..attempted {
            let outcome = self.import_next_fact().await?;
            report.record(outcome);
        }
        Ok(report)
    }

    async fn import_body(&mut self, body: Vec<u8>) -> PandaNetFactImportOutcome {
        if body.len() > self.max_fact_envelope_bytes {
            return PandaNetFactImportOutcome::Rejected(
                PandaNetFactImportRejection::EnvelopeTooLarge {
                    size: body.len(),
                    max: self.max_fact_envelope_bytes,
                },
            );
        }
        let outcome =
            import_fact_body_into_shared_store(&body, &self.store, &self.replica_session).await;
        if matches!(outcome, PandaNetFactImportOutcome::Deferred(_)) {
            self.pending_imports.push_back(body);
        }
        outcome
    }

    async fn retry_pending_imports(&mut self) {
        let pending_count = self.pending_imports.len();
        for _ in 0..pending_count {
            let Some(body) = self.pending_imports.pop_front() else {
                return;
            };
            let outcome =
                import_fact_body_into_shared_store(&body, &self.store, &self.replica_session).await;
            if matches!(outcome, PandaNetFactImportOutcome::Deferred(_)) {
                self.pending_imports.push_back(body);
            }
        }
    }
}

fn import_can_unblock_pending(outcome: &PandaNetFactImportOutcome) -> bool {
    matches!(
        outcome,
        PandaNetFactImportOutcome::Imported
            | PandaNetFactImportOutcome::Duplicate
            | PandaNetFactImportOutcome::Conflict
    )
}

pub struct PandaNetFactNodeConfig {
    node: PandaNetNodeConfig,
    topic: PandaNetTopic,
    store: SharedPandaFactStore,
    replica_session: BusSession,
    max_fact_envelope_bytes: usize,
}

impl PandaNetFactNodeConfig {
    #[must_use]
    pub fn new(
        node: PandaNetNodeConfig,
        topic: PandaNetTopic,
        store: SharedPandaFactStore,
        replica_session: BusSession,
    ) -> Self {
        Self {
            node,
            topic,
            store,
            replica_session,
            max_fact_envelope_bytes: DEFAULT_MAX_FACT_ENVELOPE_BYTES,
        }
    }

    #[must_use]
    pub fn with_max_fact_envelope_bytes(mut self, max_fact_envelope_bytes: usize) -> Self {
        self.max_fact_envelope_bytes = max_fact_envelope_bytes;
        self
    }
}
