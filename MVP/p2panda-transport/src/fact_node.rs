use std::collections::VecDeque;
use std::time::Duration;

use futures_util::StreamExt;
use mvp_bus::{BusSession, FactKey, FactPayload};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactExtensions, PandaFactLogId, PandaFactOperation,
    PandaFactWriteOutcome, SharedPandaFactStore,
};
use p2panda_core::cbor::encode_cbor;
use p2panda_core::{Hash, Operation, Topic};
use p2panda_net::{AddressBook, Discovery, Endpoint, Gossip, LogSync};
use p2panda_sync::FromSync;
use p2panda_sync::protocols::TopicLogSyncEvent;
use tokio::time::timeout;

use crate::{
    PandaNetFactImportFailure, PandaNetFactImportOutcome, PandaNetFactImportRejection,
    PandaNetFactImportReport, PandaNetNodeConfig, PandaNetNodeInfo, PandaNetStartupStep,
    PandaNetTopic, PandaNetTransportError,
    fact_driver::import_p2panda_operation_into_shared_store,
    node::{
        DEFAULT_REPLAY_CACHE_CAPACITY, PandaNetReplayCache, PandaNetReplayStatus,
        node_info_from_endpoint, with_startup_timeout,
    },
};

const DEFAULT_MAX_FACT_OPERATION_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_PENDING_IMPORTS: usize = 1024;
const STREAM_TIMEOUT: Duration = Duration::from_secs(10);

type PandaNetFactSyncHandle = p2panda_net::sync::SyncHandle<
    Operation<PandaFactExtensions>,
    TopicLogSyncEvent<PandaFactExtensions>,
>;

pub struct PandaNetFactNode {
    address_book: AddressBook,
    _discovery: Discovery,
    _endpoint: Endpoint,
    _gossip: Gossip,
    log_sync: LogSync<SharedPandaFactStore, PandaFactLogId, PandaFactExtensions>,
    stream: Option<PandaNetFactStream>,
    topic: PandaNetTopic,
    store: SharedPandaFactStore,
    replica_session: BusSession,
    publish_stream: Option<PandaNetFactSyncHandle>,
    pending_imports: VecDeque<Operation<PandaFactExtensions>>,
    replay_cache: PandaNetReplayCache,
    stats: PandaNetFactNodeStats,
    max_fact_operation_bytes: usize,
    max_pending_imports: usize,
    node_info: PandaNetNodeInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PandaNetFactNodeStats {
    pub idle_timeouts: u64,
    pub stream_refreshes: u64,
    pub stream_refresh_failures: u64,
    pub stream_ended: u64,
    pub stream_lagged: u64,
    pub stream_failed: u64,
    pub replayed_operations_skipped: u64,
    pub session_started: u64,
    pub sync_started: u64,
    pub sync_finished: u64,
    pub live_mode_started: u64,
    pub session_finished: u64,
}

impl PandaNetFactNodeStats {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            idle_timeouts: 0,
            stream_refreshes: 0,
            stream_refresh_failures: 0,
            stream_ended: 0,
            stream_lagged: 0,
            stream_failed: 0,
            replayed_operations_skipped: 0,
            session_started: 0,
            sync_started: 0,
            sync_finished: 0,
            live_mode_started: 0,
            session_finished: 0,
        }
    }

    fn record_idle_timeout(&mut self) {
        self.idle_timeouts = self.idle_timeouts.saturating_add(1);
    }

    fn record_stream_refresh(&mut self) {
        self.stream_refreshes = self.stream_refreshes.saturating_add(1);
    }

    fn record_stream_refresh_failure(&mut self) {
        self.stream_refresh_failures = self.stream_refresh_failures.saturating_add(1);
    }

    fn record_replayed_operation(&mut self) {
        self.replayed_operations_skipped = self.replayed_operations_skipped.saturating_add(1);
    }

    fn record_stream_error(&mut self, error: &PandaNetTransportError) {
        match error {
            PandaNetTransportError::StreamEnded { .. } => {
                self.stream_ended = self.stream_ended.saturating_add(1);
            }
            PandaNetTransportError::StreamLagged { .. } => {
                self.stream_lagged = self.stream_lagged.saturating_add(1);
            }
            PandaNetTransportError::StreamFailed { .. } => {
                self.stream_failed = self.stream_failed.saturating_add(1);
            }
            PandaNetTransportError::Timeout { .. }
            | PandaNetTransportError::Startup { .. }
            | PandaNetTransportError::FactStore { .. }
            | PandaNetTransportError::MissingLocalOperation => {}
        }
    }

    fn record_sync_event(&mut self, event: &TopicLogSyncEvent<PandaFactExtensions>) {
        match event {
            TopicLogSyncEvent::SessionStarted => {
                self.session_started = self.session_started.saturating_add(1);
            }
            TopicLogSyncEvent::SyncStarted { .. } => {
                self.sync_started = self.sync_started.saturating_add(1);
            }
            TopicLogSyncEvent::SyncFinished { .. } => {
                self.sync_finished = self.sync_finished.saturating_add(1);
            }
            TopicLogSyncEvent::LiveModeStarted => {
                self.live_mode_started = self.live_mode_started.saturating_add(1);
            }
            TopicLogSyncEvent::SessionFinished { .. } => {
                self.session_finished = self.session_finished.saturating_add(1);
            }
            TopicLogSyncEvent::OperationReceived { .. } | TopicLogSyncEvent::Failed { .. } => {}
        }
    }
}

impl Default for PandaNetFactNodeStats {
    fn default() -> Self {
        Self::new()
    }
}

impl PandaNetFactNode {
    pub async fn spawn(config: PandaNetFactNodeConfig) -> Result<Self, PandaNetTransportError> {
        let (network_id, signing_key, bind, bootstrap_nodes) = config.node.clone_parts();
        let address_book = with_startup_timeout(
            PandaNetStartupStep::AddressBook,
            AddressBook::builder().spawn(),
        )
        .await?;

        for bootstrap in bootstrap_nodes {
            with_startup_timeout(
                PandaNetStartupStep::BootstrapNode,
                address_book.insert_node_info(bootstrap.into_inner()),
            )
            .await?;
        }

        let endpoint = with_startup_timeout(
            PandaNetStartupStep::Endpoint,
            Endpoint::builder(address_book.clone())
                .network_id(network_id.into_inner())
                .config(bind.iroh_config())
                .signing_key(signing_key.clone())
                .spawn(),
        )
        .await?;
        let node_info = node_info_from_endpoint(&signing_key, &endpoint).await?;

        let discovery = with_startup_timeout(
            PandaNetStartupStep::Discovery,
            Discovery::builder(address_book.clone(), endpoint.clone()).spawn(),
        )
        .await?;

        let gossip = with_startup_timeout(
            PandaNetStartupStep::Gossip,
            Gossip::builder(address_book.clone(), endpoint.clone()).spawn(),
        )
        .await?;

        let log_sync = with_startup_timeout(
            PandaNetStartupStep::LogSync,
            LogSync::builder(config.store.clone(), endpoint.clone(), gossip.clone()).spawn(),
        )
        .await?;
        let stream = open_fact_stream(&log_sync, config.topic).await?;
        Ok(Self {
            address_book,
            _discovery: discovery,
            _endpoint: endpoint,
            _gossip: gossip,
            log_sync,
            stream: Some(stream),
            topic: config.topic,
            store: config.store,
            replica_session: config.replica_session,
            publish_stream: None,
            pending_imports: VecDeque::new(),
            replay_cache: PandaNetReplayCache::new(DEFAULT_REPLAY_CACHE_CAPACITY),
            stats: PandaNetFactNodeStats::new(),
            max_fact_operation_bytes: config.max_fact_operation_bytes,
            max_pending_imports: config.max_pending_imports,
            node_info,
        })
    }

    #[must_use]
    pub fn node_info(&self) -> PandaNetNodeInfo {
        self.node_info.clone()
    }

    #[must_use]
    pub fn store(&self) -> SharedPandaFactStore {
        self.store.clone()
    }

    pub async fn add_node_info(
        &self,
        node_info: PandaNetNodeInfo,
    ) -> Result<(), PandaNetTransportError> {
        with_startup_timeout(
            PandaNetStartupStep::BootstrapNode,
            self.address_book.insert_node_info(node_info.into_inner()),
        )
        .await
        .map(|_inserted| ())
    }

    pub async fn refresh_stream(&mut self) -> Result<(), PandaNetTransportError> {
        self.stats.record_stream_refresh();
        drop(self.stream.take());
        match open_fact_stream(&self.log_sync, self.topic).await {
            Ok(stream) => {
                self.stream = Some(stream);
                Ok(())
            }
            Err(error) => {
                self.stats.record_stream_refresh_failure();
                Err(error)
            }
        }
    }

    pub async fn refresh_publish_stream(&mut self) -> Result<(), PandaNetTransportError> {
        drop(self.publish_stream.take());
        self.ensure_publish_stream().await
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

    pub async fn import_next_fact(
        &mut self,
    ) -> Result<PandaNetFactImportOutcome, PandaNetTransportError> {
        let mut outcomes = self.import_next_fact_batch().await?;
        Ok(outcomes.remove(0))
    }

    pub async fn import_next_fact_batch(
        &mut self,
    ) -> Result<Vec<PandaNetFactImportOutcome>, PandaNetTransportError> {
        let stream_operation = self.next_unique_stream_operation().await?;
        Ok(self.import_stream_operation(stream_operation).await)
    }

    pub async fn import_next_fact_batch_with_idle_timeout(
        &mut self,
        idle_timeout: Duration,
    ) -> Result<Option<Vec<PandaNetFactImportOutcome>>, PandaNetTransportError> {
        let Some(stream_operation) = self
            .next_unique_stream_operation_with_idle_timeout(idle_timeout)
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(self.import_stream_operation(stream_operation).await))
    }

    async fn next_unique_stream_operation(
        &mut self,
    ) -> Result<PandaNetStreamOperation, PandaNetTransportError> {
        let max_fact_operation_bytes = self.max_fact_operation_bytes;
        let Some(stream) = self.stream.as_mut() else {
            let error = PandaNetTransportError::StreamEnded {
                topic: self.topic.into_inner(),
            };
            self.stats.record_stream_error(&error);
            return Err(error);
        };
        let result = stream
            .next_unseen_operation_limited(
                max_fact_operation_bytes,
                &mut self.replay_cache,
                &mut self.stats,
            )
            .await;
        if let Err(error) = &result {
            self.stats.record_stream_error(error);
        }
        result
    }

    async fn next_unique_stream_operation_with_idle_timeout(
        &mut self,
        idle_timeout: Duration,
    ) -> Result<Option<PandaNetStreamOperation>, PandaNetTransportError> {
        let max_fact_operation_bytes = self.max_fact_operation_bytes;
        let Some(stream) = self.stream.as_mut() else {
            let error = PandaNetTransportError::StreamEnded {
                topic: self.topic.into_inner(),
            };
            self.stats.record_stream_error(&error);
            return Err(error);
        };
        let result = stream
            .next_unseen_operation_limited_with_idle_timeout(
                max_fact_operation_bytes,
                &mut self.replay_cache,
                &mut self.stats,
                idle_timeout,
            )
            .await;
        match &result {
            Ok(None) => self.stats.record_idle_timeout(),
            Err(error) => self.stats.record_stream_error(error),
            Ok(Some(_)) => {}
        }
        result
    }

    async fn import_stream_operation(
        &mut self,
        stream_operation: PandaNetStreamOperation,
    ) -> Vec<PandaNetFactImportOutcome> {
        let operation_hash = stream_operation.operation_hash();
        let outcome = match stream_operation {
            PandaNetStreamOperation::Operation { operation, .. } => {
                self.import_operation(*operation).await
            }
            PandaNetStreamOperation::TooLarge { size, max, .. } => {
                PandaNetFactImportOutcome::Rejected(
                    PandaNetFactImportRejection::OperationTooLarge { size, max },
                )
            }
            PandaNetStreamOperation::Malformed { message, .. } => {
                PandaNetFactImportOutcome::Rejected(
                    PandaNetFactImportRejection::MalformedOperation { message },
                )
            }
        };
        if import_outcome_is_terminal_or_queued(&outcome) {
            self.replay_cache.insert(operation_hash);
        }
        let can_unblock_pending = import_can_unblock_pending(&outcome);
        let mut outcomes = vec![outcome];
        if can_unblock_pending {
            outcomes.extend(self.retry_pending_imports().await);
        }
        outcomes
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
        self.store
            .associate_transport_topic(self.topic.into_inner(), operation)
            .await
            .map_err(|error| PandaNetTransportError::FactStore {
                message: error.to_string(),
            })?;
        self.publish_p2panda_operation(operation.to_p2panda_operation().map_err(|error| {
            PandaNetTransportError::FactStore {
                message: error.to_string(),
            }
        })?)
        .await
    }

    async fn publish_p2panda_operation(
        &mut self,
        operation: Operation<PandaFactExtensions>,
    ) -> Result<(), PandaNetTransportError> {
        self.ensure_publish_stream().await?;
        let handle = self
            .publish_stream
            .as_ref()
            .expect("publish stream was inserted before lookup");
        match timeout(STREAM_TIMEOUT, handle.publish(operation.clone())).await {
            Err(_elapsed) => {
                drop(self.publish_stream.take());
                Err(PandaNetTransportError::startup(
                    PandaNetStartupStep::Publish,
                    format!("publish timed out after {}ms", STREAM_TIMEOUT.as_millis()),
                ))
            }
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                drop(self.publish_stream.take());
                self.ensure_publish_stream().await?;
                let handle = self
                    .publish_stream
                    .as_ref()
                    .expect("publish stream was inserted before retry lookup");
                match timeout(STREAM_TIMEOUT, handle.publish(operation)).await {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(retry_error)) => Err(PandaNetTransportError::startup(
                        PandaNetStartupStep::Publish,
                        format!("{error}; retry failed: {retry_error}"),
                    )),
                    Err(_elapsed) => Err(PandaNetTransportError::startup(
                        PandaNetStartupStep::Publish,
                        format!(
                            "{error}; retry timed out after {}ms",
                            STREAM_TIMEOUT.as_millis()
                        ),
                    )),
                }
            }
        }
    }

    async fn ensure_publish_stream(&mut self) -> Result<(), PandaNetTransportError> {
        if self.publish_stream.is_none() {
            let topic = self.topic.into_inner();
            let handle = with_startup_timeout(
                PandaNetStartupStep::Stream,
                self.log_sync.stream(topic, true),
            )
            .await?;
            self.publish_stream = Some(handle);
        }
        Ok(())
    }

    async fn import_operation(
        &mut self,
        operation: Operation<PandaFactExtensions>,
    ) -> PandaNetFactImportOutcome {
        let operation_size = match p2panda_operation_size(&operation) {
            Ok(size) => size,
            Err(message) => {
                return PandaNetFactImportOutcome::Rejected(
                    PandaNetFactImportRejection::MalformedOperation { message },
                );
            }
        };
        if operation_size > self.max_fact_operation_bytes {
            return PandaNetFactImportOutcome::Rejected(
                PandaNetFactImportRejection::OperationTooLarge {
                    size: operation_size,
                    max: self.max_fact_operation_bytes,
                },
            );
        }
        let outcome = import_p2panda_operation_into_shared_store(
            operation.clone(),
            &self.store,
            &self.replica_session,
            self.topic.into_inner(),
        )
        .await;
        if matches!(outcome, PandaNetFactImportOutcome::Deferred(_)) {
            if self
                .pending_imports
                .iter()
                .any(|pending| pending == &operation)
            {
                return outcome;
            }
            if self.pending_imports.len() >= self.max_pending_imports {
                return PandaNetFactImportOutcome::Failed(
                    PandaNetFactImportFailure::PendingQueueFull {
                        max: self.max_pending_imports,
                    },
                );
            }
            self.pending_imports.push_back(operation);
        }
        outcome
    }

    #[cfg(test)]
    pub(crate) async fn import_operation_for_test(
        &mut self,
        operation: PandaFactOperation,
    ) -> PandaNetFactImportOutcome {
        match operation.to_p2panda_operation() {
            Ok(operation) => self.import_operation(operation).await,
            Err(error) => PandaNetFactImportOutcome::Rejected(
                PandaNetFactImportRejection::MalformedOperation {
                    message: error.to_string(),
                },
            ),
        }
    }

    #[cfg(test)]
    pub(crate) async fn retry_pending_imports_for_test(
        &mut self,
    ) -> Vec<PandaNetFactImportOutcome> {
        self.retry_pending_imports().await
    }

    async fn retry_pending_imports(&mut self) -> Vec<PandaNetFactImportOutcome> {
        let mut outcomes = Vec::new();
        loop {
            let pending_count = self.pending_imports.len();
            let mut made_progress = false;
            for _ in 0..pending_count {
                let Some(operation) = self.pending_imports.pop_front() else {
                    return outcomes;
                };
                let outcome = import_p2panda_operation_into_shared_store(
                    operation.clone(),
                    &self.store,
                    &self.replica_session,
                    self.topic.into_inner(),
                )
                .await;
                if matches!(outcome, PandaNetFactImportOutcome::Deferred(_)) {
                    self.pending_imports.push_back(operation);
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

    #[must_use]
    pub fn stats(&self) -> PandaNetFactNodeStats {
        self.stats
    }
}

struct PandaNetFactStream {
    topic: Topic,
    _handle: PandaNetFactSyncHandle,
    subscription: p2panda_net::sync::SyncSubscription<TopicLogSyncEvent<PandaFactExtensions>>,
}

enum PandaNetStreamOperation {
    Operation {
        operation_hash: Hash,
        operation: Box<Operation<PandaFactExtensions>>,
    },
    TooLarge {
        operation_hash: Hash,
        size: usize,
        max: usize,
    },
    Malformed {
        operation_hash: Hash,
        message: String,
    },
}

impl PandaNetStreamOperation {
    fn operation_hash(&self) -> Hash {
        match self {
            Self::Operation { operation_hash, .. }
            | Self::TooLarge { operation_hash, .. }
            | Self::Malformed { operation_hash, .. } => *operation_hash,
        }
    }
}

impl PandaNetFactStream {
    async fn next_unseen_operation_limited(
        &mut self,
        max_operation_bytes: usize,
        replay_cache: &mut PandaNetReplayCache,
        stats: &mut PandaNetFactNodeStats,
    ) -> Result<PandaNetStreamOperation, PandaNetTransportError> {
        loop {
            let stream_operation = self
                .next_operation_limited(max_operation_bytes, stats)
                .await?;
            match replay_cache.check_seen(stream_operation.operation_hash()) {
                PandaNetReplayStatus::Fresh => return Ok(stream_operation),
                PandaNetReplayStatus::Replayed => stats.record_replayed_operation(),
            }
        }
    }

    async fn next_unseen_operation_limited_with_idle_timeout(
        &mut self,
        max_operation_bytes: usize,
        replay_cache: &mut PandaNetReplayCache,
        stats: &mut PandaNetFactNodeStats,
        idle_timeout: Duration,
    ) -> Result<Option<PandaNetStreamOperation>, PandaNetTransportError> {
        match timeout(
            idle_timeout,
            self.next_unseen_operation_limited(max_operation_bytes, replay_cache, stats),
        )
        .await
        {
            Ok(stream_operation) => stream_operation.map(Some),
            Err(_) => Ok(None),
        }
    }

    async fn next_operation_limited(
        &mut self,
        max_operation_bytes: usize,
        stats: &mut PandaNetFactNodeStats,
    ) -> Result<PandaNetStreamOperation, PandaNetTransportError> {
        loop {
            let event = timeout(STREAM_TIMEOUT, self.subscription.next())
                .await
                .map_err(|_| PandaNetTransportError::Timeout {
                    step: PandaNetStartupStep::Stream,
                    timeout_ms: STREAM_TIMEOUT.as_millis() as u64,
                })?
                .ok_or(PandaNetTransportError::StreamEnded { topic: self.topic })?
                .map_err(|error| PandaNetTransportError::StreamLagged {
                    topic: self.topic,
                    message: error.to_string(),
                })?;

            match event {
                FromSync {
                    event: TopicLogSyncEvent::OperationReceived { operation, .. },
                    ..
                } => {
                    let operation = *operation;
                    let operation_hash = operation.hash;
                    let operation_size = match p2panda_operation_size(&operation) {
                        Ok(size) => size,
                        Err(message) => {
                            return Ok(PandaNetStreamOperation::Malformed {
                                operation_hash,
                                message,
                            });
                        }
                    };
                    if operation_size > max_operation_bytes {
                        return Ok(PandaNetStreamOperation::TooLarge {
                            operation_hash,
                            size: operation_size,
                            max: max_operation_bytes,
                        });
                    }
                    return Ok(PandaNetStreamOperation::Operation {
                        operation_hash,
                        operation: Box::new(operation),
                    });
                }
                FromSync {
                    event: TopicLogSyncEvent::Failed { error },
                    ..
                } => {
                    return Err(PandaNetTransportError::StreamFailed {
                        topic: self.topic,
                        message: error.to_string(),
                    });
                }
                FromSync { event, .. } => stats.record_sync_event(&event),
            }
        }
    }
}

async fn open_fact_stream(
    log_sync: &LogSync<SharedPandaFactStore, PandaFactLogId, PandaFactExtensions>,
    topic: PandaNetTopic,
) -> Result<PandaNetFactStream, PandaNetTransportError> {
    let topic = topic.into_inner();
    let handle =
        with_startup_timeout(PandaNetStartupStep::Stream, log_sync.stream(topic, true)).await?;
    let subscription =
        with_startup_timeout(PandaNetStartupStep::Stream, handle.subscribe()).await?;
    Ok(PandaNetFactStream {
        topic,
        _handle: handle,
        subscription,
    })
}

fn p2panda_operation_size(operation: &Operation<PandaFactExtensions>) -> Result<usize, String> {
    let header = encode_cbor(operation.header())
        .map_err(|error| format!("encode p2panda operation header: {error}"))?;
    let body_size = usize::try_from(operation.header.payload_size).unwrap_or(usize::MAX);
    Ok(header.len().saturating_add(body_size))
}

fn import_outcome_is_terminal_or_queued(outcome: &PandaNetFactImportOutcome) -> bool {
    matches!(
        outcome,
        PandaNetFactImportOutcome::Imported
            | PandaNetFactImportOutcome::Duplicate
            | PandaNetFactImportOutcome::Conflict
            | PandaNetFactImportOutcome::Deferred(_)
    )
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
    max_fact_operation_bytes: usize,
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
            max_fact_operation_bytes: DEFAULT_MAX_FACT_OPERATION_BYTES,
            max_pending_imports: DEFAULT_MAX_PENDING_IMPORTS,
        }
    }

    #[must_use]
    pub fn with_max_fact_operation_bytes(mut self, max_fact_operation_bytes: usize) -> Self {
        self.max_fact_operation_bytes = max_fact_operation_bytes;
        self
    }

    #[must_use]
    pub fn with_max_pending_imports(mut self, max_pending_imports: usize) -> Self {
        self.max_pending_imports = max_pending_imports;
        self
    }
}
