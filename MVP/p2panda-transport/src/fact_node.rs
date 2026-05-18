use std::collections::VecDeque;

use mvp_bus::{BusSession, FactKey, FactPayload};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactOperation, PandaFactWireEnvelope, PandaFactWriteOutcome,
    SharedPandaFactStore,
};

use crate::{
    PandaNetFactImportFailure, PandaNetFactImportOutcome, PandaNetFactImportRejection,
    PandaNetFactImportReport, PandaNetNode, PandaNetNodeConfig, PandaNetNodeInfo, PandaNetStream,
    PandaNetTopic, PandaNetTransportError, import_fact_body_into_shared_store,
    node::PandaNetStreamBody,
};

const DEFAULT_MAX_FACT_ENVELOPE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_PENDING_IMPORTS: usize = 1024;

pub struct PandaNetFactNode {
    node: PandaNetNode,
    stream: PandaNetStream,
    topic: PandaNetTopic,
    store: SharedPandaFactStore,
    replica_session: BusSession,
    pending_imports: VecDeque<Vec<u8>>,
    max_fact_envelope_bytes: usize,
    max_pending_imports: usize,
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
            max_pending_imports: config.max_pending_imports,
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

    pub async fn refresh_stream(&mut self) -> Result<(), PandaNetTransportError> {
        self.stream = self.node.open_stream(self.topic, true).await?;
        Ok(())
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
            self.append_operation(operation).await?;
        } else if !matches!(write.outcome(), PandaFactWriteOutcome::AlreadyPresent(_)) {
            return Err(PandaNetTransportError::MissingLocalOperation);
        }
        Ok(write.into_outcome())
    }

    #[cfg(any(test, feature = "harness"))]
    pub async fn publish_operation(
        &mut self,
        operation: &PandaFactOperation,
    ) -> Result<(), PandaNetTransportError> {
        self.append_operation(operation).await
    }

    #[cfg(any(test, feature = "harness"))]
    pub async fn publish_body(&mut self, body: Vec<u8>) -> Result<(), PandaNetTransportError> {
        self.append_body(body).await
    }

    pub async fn import_next_fact(
        &mut self,
    ) -> Result<PandaNetFactImportOutcome, PandaNetTransportError> {
        let mut outcomes = self.import_next_fact_batch().await?;
        Ok(outcomes.remove(0))
    }

    pub async fn import_next_fact_batch(
        &mut self,
    ) -> Result<Vec<PandaNetFactImportOutcome>, PandaNetTransportError> {
        let outcome =
            match self
                .stream
                .next_body_limited(self.max_fact_envelope_bytes)
                .await?
            {
                PandaNetStreamBody::Body(body) => self.import_body(body).await,
                PandaNetStreamBody::TooLarge { size, max } => PandaNetFactImportOutcome::Rejected(
                    PandaNetFactImportRejection::EnvelopeTooLarge { size, max },
                ),
            };
        let can_unblock_pending = import_can_unblock_pending(&outcome);
        let mut outcomes = vec![outcome];
        if can_unblock_pending {
            outcomes.extend(self.retry_pending_imports().await);
        }
        Ok(outcomes)
    }

    pub async fn import_until_attempted(
        &mut self,
        attempted: usize,
    ) -> Result<PandaNetFactImportReport, PandaNetTransportError> {
        let mut report = PandaNetFactImportReport::new(attempted);
        for _ in 0..attempted {
            for outcome in self.import_next_fact_batch().await? {
                report.record(outcome);
            }
        }
        Ok(report)
    }

    async fn append_operation(
        &mut self,
        operation: &PandaFactOperation,
    ) -> Result<(), PandaNetTransportError> {
        self.append_body(PandaFactWireEnvelope::encode(operation))
            .await
    }

    async fn append_body(&mut self, body: Vec<u8>) -> Result<(), PandaNetTransportError> {
        self.node.append_to_topic(self.topic, &body).await
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
            if self.pending_imports.iter().any(|pending| pending == &body) {
                return outcome;
            }
            if self.pending_imports.len() >= self.max_pending_imports {
                return PandaNetFactImportOutcome::Failed(
                    PandaNetFactImportFailure::PendingQueueFull {
                        max: self.max_pending_imports,
                    },
                );
            }
            self.pending_imports.push_back(body);
        }
        outcome
    }

    async fn retry_pending_imports(&mut self) -> Vec<PandaNetFactImportOutcome> {
        let mut outcomes = Vec::new();
        loop {
            let pending_count = self.pending_imports.len();
            let mut made_progress = false;
            for _ in 0..pending_count {
                let Some(body) = self.pending_imports.pop_front() else {
                    return outcomes;
                };
                let outcome =
                    import_fact_body_into_shared_store(&body, &self.store, &self.replica_session)
                        .await;
                if matches!(outcome, PandaNetFactImportOutcome::Deferred(_)) {
                    self.pending_imports.push_back(body);
                    continue;
                }
                if import_can_unblock_pending(&outcome) {
                    made_progress = true;
                }
                outcomes.push(outcome);
            }
            if !made_progress || self.pending_imports.is_empty() {
                return outcomes;
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
    max_pending_imports: usize,
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
            max_pending_imports: DEFAULT_MAX_PENDING_IMPORTS,
        }
    }

    #[must_use]
    pub fn with_max_fact_envelope_bytes(mut self, max_fact_envelope_bytes: usize) -> Self {
        self.max_fact_envelope_bytes = max_fact_envelope_bytes;
        self
    }

    #[must_use]
    pub fn with_max_pending_imports(mut self, max_pending_imports: usize) -> Self {
        self.max_pending_imports = max_pending_imports;
        self
    }
}
