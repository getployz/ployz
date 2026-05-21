use super::*;
use store_runtime::PandaBackendIngest;

#[derive(Clone)]
pub(super) enum PandaFactBackend {
    Memory(PandaMemoryStore),
    Sqlite(SqliteStore),
}

#[derive(Clone, Default)]
pub(super) struct PandaMemoryStore {
    inner: Arc<StdMutex<PandaMemoryInner>>,
}

#[derive(Default)]
struct PandaMemoryInner {
    operations: BTreeMap<Hash, Operation<PandaFactExtensions>>,
    logs: BTreeMap<(VerifyingKey, IslandLog), Vec<Hash>>,
    topics: BTreeMap<Topic, BTreeMap<VerifyingKey, Vec<IslandLog>>>,
}

impl PandaMemoryStore {
    async fn ingest_operation(
        &self,
        operation: Operation<PandaFactExtensions>,
        log_id: &IslandLog,
    ) -> Result<PandaBackendIngest> {
        validate_operation(&operation).map_err(|_| PandaFactError::InvalidOperation)?;
        let mut inner = self.inner.lock().expect("memory panda store");
        if inner.operations.contains_key(&operation.hash) {
            return Ok(PandaBackendIngest::AlreadyPresent(operation));
        }
        let log_key = (operation.header.verifying_key, log_id.clone());
        let latest_operation = inner
            .logs
            .get(&log_key)
            .and_then(|hashes| hashes.last())
            .and_then(|hash| inner.operations.get(hash));
        match (operation.header.backlink, latest_operation) {
            (Some(backlink), Some(latest_operation))
                if backlink == latest_operation.hash
                    && operation.header.seq_num == latest_operation.header.seq_num + 1 => {}
            (None, None) if operation.header.seq_num == 0 => {}
            _ => {
                return Err(PandaFactError::OutOfOrderOperation {
                    island: IslandId::new(operation.header.extensions.island.clone()),
                    principal: PrincipalId::new(operation.header.extensions.author.clone()),
                    key: FactKey::parse(operation.header.extensions.key.clone()).map_err(
                        |error| PandaFactError::InvalidExtensions {
                            message: error.to_string(),
                        },
                    )?,
                    missing_operations: 1,
                });
            }
        }
        inner.logs.entry(log_key).or_default().push(operation.hash);
        inner.operations.insert(operation.hash, operation.clone());
        Ok(PandaBackendIngest::Inserted(operation))
    }

    pub(super) async fn latest_operation(
        &self,
        public_key: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<Option<Operation<PandaFactExtensions>>> {
        let inner = self.inner.lock().expect("memory panda store");
        Ok(inner
            .logs
            .get(&(*public_key, log_id.clone()))
            .and_then(|hashes| hashes.last())
            .and_then(|hash| inner.operations.get(hash))
            .cloned())
    }

    pub(super) async fn get_log_heights_for_log(
        &self,
        log_id: &IslandLog,
    ) -> Result<Vec<(VerifyingKey, u64)>> {
        let inner = self.inner.lock().expect("memory panda store");
        Ok(inner
            .logs
            .iter()
            .filter_map(|((author, current_log), hashes)| {
                (current_log == log_id).then_some((*author, hashes.len().saturating_sub(1) as u64))
            })
            .collect())
    }

    pub(super) async fn raw_log(
        &self,
        public_key: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<Option<Vec<RawOperation>>> {
        let inner = self.inner.lock().expect("memory panda store");
        let Some(hashes) = inner.logs.get(&(*public_key, log_id.clone())) else {
            return Ok(None);
        };
        let mut raw = Vec::new();
        for hash in hashes {
            let Some(operation) = inner.operations.get(hash) else {
                continue;
            };
            raw.push((
                encode_cbor(operation.header()).map_err(|error| {
                    PandaFactError::InvalidExtensions {
                        message: error.to_string(),
                    }
                })?,
                operation.body().map(Body::to_bytes),
            ));
        }
        Ok(Some(raw))
    }

    pub(super) async fn associate_topic(
        &self,
        topic: &Topic,
        author: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<bool> {
        let mut inner = self.inner.lock().expect("memory panda store");
        let logs = inner.topics.entry(*topic).or_default();
        let log_ids = logs.entry(*author).or_default();
        if log_ids.contains(log_id) {
            return Ok(false);
        }
        log_ids.push(log_id.clone());
        Ok(true)
    }

    pub(super) async fn remove_topic(
        &self,
        topic: &Topic,
        author: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<bool> {
        let mut inner = self.inner.lock().expect("memory panda store");
        let Some(logs) = inner.topics.get_mut(topic) else {
            return Ok(false);
        };
        let Some(log_ids) = logs.get_mut(author) else {
            return Ok(false);
        };
        let Some(index) = log_ids.iter().position(|current| current == log_id) else {
            return Ok(false);
        };
        log_ids.remove(index);
        Ok(true)
    }

    pub(super) async fn resolve_topic(
        &self,
        topic: &Topic,
    ) -> Result<BTreeMap<VerifyingKey, Vec<IslandLog>>> {
        let inner = self.inner.lock().expect("memory panda store");
        Ok(inner.topics.get(topic).cloned().unwrap_or_default())
    }
}

impl LogStore<Operation<PandaFactExtensions>, VerifyingKey, IslandLog, u64, Hash>
    for PandaMemoryStore
{
    type Error = PandaMemoryStoreError;

    async fn get_latest_entry(
        &self,
        author: &VerifyingKey,
        log_id: &IslandLog,
    ) -> std::result::Result<Option<Operation<PandaFactExtensions>>, Self::Error> {
        Ok(self
            .latest_operation(author, log_id)
            .await
            .expect("memory latest"))
    }

    async fn get_latest_entry_tx(
        &self,
        author: &VerifyingKey,
        log_id: &IslandLog,
    ) -> std::result::Result<Option<Operation<PandaFactExtensions>>, Self::Error> {
        self.get_latest_entry(author, log_id).await
    }

    async fn get_log_heights(
        &self,
        author: &VerifyingKey,
        logs: &[IslandLog],
    ) -> std::result::Result<Option<BTreeMap<IslandLog, u64>>, Self::Error> {
        let inner = self.inner.lock().expect("memory panda store");
        let mut heights = BTreeMap::new();
        for log_id in logs {
            if let Some(hashes) = inner.logs.get(&(*author, log_id.clone())) {
                heights.insert(log_id.clone(), hashes.len().saturating_sub(1) as u64);
            }
        }
        Ok((!heights.is_empty()).then_some(heights))
    }

    async fn get_log_size(
        &self,
        author: &VerifyingKey,
        log_id: &IslandLog,
        after: Option<u64>,
        until: Option<u64>,
    ) -> std::result::Result<Option<(u64, u64)>, Self::Error> {
        let inner = self.inner.lock().expect("memory panda store");
        let Some(hashes) = inner.logs.get(&(*author, log_id.clone())) else {
            return Ok(None);
        };
        let mut operations = 0_u64;
        let mut bytes = 0_u64;
        for hash in hashes {
            let Some(operation) = inner.operations.get(hash) else {
                continue;
            };
            if after.is_some_and(|after| operation.header.seq_num <= after)
                || until.is_some_and(|until| operation.header.seq_num > until)
            {
                continue;
            }
            let header = encode_cbor(operation.header()).map_err(|_| PandaMemoryStoreError)?;
            operations += 1;
            bytes += header.len() as u64 + operation.header.payload_size;
        }
        Ok(Some((operations, bytes)))
    }

    async fn get_log_entries(
        &self,
        author: &VerifyingKey,
        log_id: &IslandLog,
        after: Option<u64>,
        until: Option<u64>,
    ) -> std::result::Result<Option<Vec<(Operation<PandaFactExtensions>, Vec<u8>)>>, Self::Error>
    {
        let inner = self.inner.lock().expect("memory panda store");
        let Some(hashes) = inner.logs.get(&(*author, log_id.clone())) else {
            return Ok(None);
        };
        let mut entries = Vec::new();
        for hash in hashes {
            let Some(operation) = inner.operations.get(hash) else {
                continue;
            };
            if after.is_some_and(|after| operation.header.seq_num <= after) {
                continue;
            }
            if until.is_some_and(|until| operation.header.seq_num > until) {
                continue;
            }
            let header = encode_cbor(operation.header()).map_err(|_| PandaMemoryStoreError)?;
            entries.push((operation.clone(), header));
        }
        Ok(Some(entries))
    }

    async fn prune_entries(
        &self,
        _author: &VerifyingKey,
        _log_id: &IslandLog,
        _until: &u64,
    ) -> std::result::Result<u64, Self::Error> {
        Ok(0)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("memory p2panda store failed")]
pub struct PandaMemoryStoreError;

#[derive(Debug, thiserror::Error)]
#[error("p2panda fact store adapter failed: {message}")]
pub struct PandaFactStoreAdapterError {
    message: String,
}

impl PandaFactStoreAdapterError {
    pub(super) fn new(error: impl ToString) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

impl PandaFactBackend {
    pub(super) async fn ingest_operation(
        &mut self,
        header: Header<PandaFactExtensions>,
        body: Option<Body>,
        log_id: &IslandLog,
    ) -> Result<PandaBackendIngest> {
        let operation = Operation {
            hash: header.hash(),
            header,
            body,
        };
        match self {
            Self::Memory(store) => store.ingest_operation(operation, log_id).await,
            Self::Sqlite(store) => {
                let inserted = ingest_operation(store, &operation, log_id, log_id, false)
                    .await
                    .map_err(|error| classify_p2panda_ingest_error(&operation, error))?;
                Ok(if inserted {
                    PandaBackendIngest::Inserted(operation)
                } else {
                    PandaBackendIngest::AlreadyPresent(operation)
                })
            }
        }
    }

    pub(super) async fn latest_operation(
        &self,
        public_key: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<Option<Operation<PandaFactExtensions>>> {
        match self {
            Self::Memory(store) => store.latest_operation(public_key, log_id).await,
            Self::Sqlite(store) => store
                .get_latest_entry(public_key, log_id)
                .await
                .map_err(store_error),
        }
    }

    pub(super) async fn get_log_heights(
        &self,
        log_id: &IslandLog,
    ) -> Result<Vec<(VerifyingKey, u64)>> {
        match self {
            Self::Memory(store) => store.get_log_heights_for_log(log_id).await,
            Self::Sqlite(store) => {
                let associations =
                    <SqliteStore as TopicStore<IslandLog, VerifyingKey, IslandLog>>::resolve(
                        store, log_id,
                    )
                    .await
                    .map_err(store_error)?;
                let mut heights = Vec::new();
                for (author, logs) in associations {
                    let Some(log_heights) =
                        <SqliteStore as LogStore<
                            Operation<PandaFactExtensions>,
                            VerifyingKey,
                            IslandLog,
                            u64,
                            Hash,
                        >>::get_log_heights(store, &author, &logs)
                        .await
                        .map_err(store_error)?
                    else {
                        continue;
                    };
                    if let Some(height) = log_heights.get(log_id) {
                        heights.push((author, *height));
                    }
                }
                Ok(heights)
            }
        }
    }

    pub(super) async fn get_raw_log(
        &self,
        public_key: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<Option<Vec<RawOperation>>> {
        match self {
            Self::Memory(store) => store.raw_log(public_key, log_id).await,
            Self::Sqlite(store) => {
                let entries: Option<Vec<(Operation<PandaFactExtensions>, Vec<u8>)>> = store
                    .get_log_entries(public_key, log_id, None, None)
                    .await
                    .map_err(store_error)?;
                let Some(entries) = entries else {
                    return Ok(None);
                };
                let mut raw = Vec::new();
                for (operation, header_bytes) in entries {
                    raw.push((header_bytes, operation.body().map(Body::to_bytes)));
                }
                Ok(Some(raw))
            }
        }
    }

    pub(super) async fn associate_topic(
        &self,
        topic: &Topic,
        author: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<bool> {
        match self {
            Self::Memory(store) => store.associate_topic(topic, author, log_id).await,
            Self::Sqlite(store) => {
                let permit = store
                    .begin()
                    .await
                    .map_err(|error| store_error_with("begin topic association", error))?;
                let result =
                    <SqliteStore as TopicStore<Topic, VerifyingKey, IslandLog>>::associate(
                        store, topic, author, log_id,
                    )
                    .await;
                match result {
                    Ok(associated) => {
                        store
                            .commit(permit)
                            .await
                            .map_err(|error| store_error_with("commit topic association", error))?;
                        Ok(associated)
                    }
                    Err(error) => {
                        let _ = store.rollback(permit).await;
                        Err(store_error_with("associate topic", error))
                    }
                }
            }
        }
    }

    pub(super) async fn remove_topic(
        &self,
        topic: &Topic,
        author: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<bool> {
        match self {
            Self::Memory(store) => store.remove_topic(topic, author, log_id).await,
            Self::Sqlite(store) => {
                let permit = store
                    .begin()
                    .await
                    .map_err(|error| store_error_with("begin topic removal", error))?;
                let result = <SqliteStore as TopicStore<Topic, VerifyingKey, IslandLog>>::remove(
                    store, topic, author, log_id,
                )
                .await;
                match result {
                    Ok(removed) => {
                        store
                            .commit(permit)
                            .await
                            .map_err(|error| store_error_with("commit topic removal", error))?;
                        Ok(removed)
                    }
                    Err(error) => {
                        let _ = store.rollback(permit).await;
                        Err(store_error_with("remove topic", error))
                    }
                }
            }
        }
    }

    pub(super) async fn resolve_topic(
        &self,
        topic: &Topic,
    ) -> Result<BTreeMap<VerifyingKey, Vec<IslandLog>>> {
        match self {
            Self::Memory(store) => store.resolve_topic(topic).await,
            Self::Sqlite(store) => {
                <SqliteStore as TopicStore<Topic, VerifyingKey, IslandLog>>::resolve(store, topic)
                    .await
                    .map_err(store_error)
            }
        }
    }
}

fn classify_p2panda_ingest_error(
    operation: &Operation<PandaFactExtensions>,
    error: IngestError,
) -> PandaFactError {
    match error {
        IngestError::InvalidOperation(
            OperationError::BacklinkMissing
            | OperationError::BacklinkMismatch
            | OperationError::SeqNumNonIncremental(_, _),
        ) => operation_out_of_order_error(operation),
        other => PandaFactError::Ingest(other),
    }
}

fn operation_out_of_order_error(operation: &Operation<PandaFactExtensions>) -> PandaFactError {
    PandaFactError::OutOfOrderOperation {
        island: IslandId::new(operation.header.extensions.island.clone()),
        principal: PrincipalId::new(operation.header.extensions.author.clone()),
        key: FactKey::parse(operation.header.extensions.key.clone()).unwrap_or_else(|_| {
            FactKey::parse("/facts/invalid/out-of-order").expect("fallback fact key parses")
        }),
        missing_operations: 1,
    }
}
