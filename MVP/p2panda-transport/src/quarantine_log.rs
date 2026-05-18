use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use p2panda_core::cbor::encode_cbor;
use p2panda_core::{Body, Hash, Header, Operation, SigningKey, Topic, VerifyingKey};
use p2panda_store::logs::LogStore;
use p2panda_store::topics::TopicStore;
use p2panda_sync::protocols::Logs;
use tokio::sync::Mutex;

use crate::PandaNetTransportError;

pub(crate) type PandaNetLogId = u64;
pub(crate) type PandaNetStore = PandaNetQuarantineStore;

#[derive(Clone)]
pub(crate) struct PandaNetQuarantineStore {
    inner: Arc<Mutex<PandaNetQuarantineState>>,
}

#[derive(Default)]
struct PandaNetQuarantineState {
    operations: BTreeMap<Hash, StoredPandaNetOperation>,
    logs: BTreeMap<(VerifyingKey, PandaNetLogId), Vec<Hash>>,
    topics: BTreeMap<Topic, Logs<PandaNetLogId>>,
}

struct StoredPandaNetOperation {
    operation: Operation<()>,
    header_bytes: Vec<u8>,
}

impl Default for PandaNetQuarantineStore {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PandaNetQuarantineState::default())),
        }
    }
}

#[derive(Clone)]
pub(crate) struct PandaNetQuarantineLog {
    store: PandaNetStore,
    signing_key: SigningKey,
}

impl PandaNetQuarantineLog {
    pub(crate) fn new(signing_key: SigningKey) -> Self {
        Self {
            store: PandaNetQuarantineStore::default(),
            signing_key,
        }
    }

    pub(crate) fn store(&self) -> PandaNetStore {
        self.store.clone()
    }

    pub(crate) async fn append_to_topic(
        &mut self,
        topic: Topic,
        body: &[u8],
        log_id: PandaNetLogId,
    ) -> Result<Operation<()>, PandaNetTransportError> {
        let (header, body) = self.create_operation(body, log_id).await?;
        self.store
            .store_operation(header.clone(), Some(body.clone()), log_id)
            .await
            .map_err(quarantine_store_error)?;
        self.store
            .associate(&topic, &self.signing_key.verifying_key(), &log_id)
            .await
            .map_err(quarantine_store_error)?;
        let hash = header.hash();
        Ok(Operation {
            hash,
            header,
            body: Some(body),
        })
    }

    pub(crate) async fn store_received_operation(
        &mut self,
        topic: Topic,
        operation: &Operation<()>,
        log_id: PandaNetLogId,
    ) -> Result<(), PandaNetTransportError> {
        self.store
            .store_operation(operation.header.clone(), operation.body.clone(), log_id)
            .await
            .map_err(quarantine_store_error)?;
        self.store
            .associate(&topic, &operation.header.verifying_key, &log_id)
            .await
            .map_err(quarantine_store_error)?;
        Ok(())
    }

    async fn create_operation(
        &self,
        bytes: &[u8],
        log_id: PandaNetLogId,
    ) -> Result<(Header<()>, Body), PandaNetTransportError> {
        let body = Body::new(bytes);
        let (seq_num, backlink) = self
            .store
            .get_latest_entry(&self.signing_key.verifying_key(), &log_id)
            .await
            .map_err(quarantine_store_error)?
            .map(|operation| (operation.header.seq_num + 1, Some(operation.hash)))
            .unwrap_or((0, None));

        let mut header = Header::<()> {
            version: 1,
            verifying_key: self.signing_key.verifying_key(),
            signature: None,
            payload_size: body.size(),
            payload_hash: Some(body.hash()),
            timestamp: p2panda_core::Timestamp::now(),
            seq_num,
            backlink,
            extensions: (),
        };
        header.sign(&self.signing_key);
        Ok((header, body))
    }
}

impl PandaNetQuarantineStore {
    async fn store_operation(
        &self,
        header: Header<()>,
        body: Option<Body>,
        log_id: PandaNetLogId,
    ) -> Result<bool, PandaNetQuarantineStoreError> {
        let hash = header.hash();
        let author = header.verifying_key;
        let header_bytes = encode_cbor(&header).map_err(|error| PandaNetQuarantineStoreError {
            message: error.to_string(),
        })?;
        let operation = Operation { hash, header, body };
        let mut inner = self.inner.lock().await;
        let inserted = inner
            .operations
            .insert(
                hash,
                StoredPandaNetOperation {
                    operation,
                    header_bytes,
                },
            )
            .is_none();
        if inserted {
            inner.logs.entry((author, log_id)).or_default().push(hash);
        }
        Ok(inserted)
    }
}

impl LogStore<Operation<()>, VerifyingKey, PandaNetLogId, u64, Hash> for PandaNetQuarantineStore {
    type Error = PandaNetQuarantineStoreError;

    async fn get_latest_entry(
        &self,
        author: &VerifyingKey,
        log_id: &PandaNetLogId,
    ) -> Result<Option<Operation<()>>, Self::Error> {
        let inner = self.inner.lock().await;
        let Some(hash) = inner
            .logs
            .get(&(*author, *log_id))
            .and_then(|hashes| hashes.last())
        else {
            return Ok(None);
        };
        Ok(inner
            .operations
            .get(hash)
            .map(|stored| stored.operation.clone()))
    }

    async fn get_latest_entry_tx(
        &self,
        author: &VerifyingKey,
        log_id: &PandaNetLogId,
    ) -> Result<Option<Operation<()>>, Self::Error> {
        self.get_latest_entry(author, log_id).await
    }

    async fn get_log_heights(
        &self,
        author: &VerifyingKey,
        logs: &[PandaNetLogId],
    ) -> Result<Option<BTreeMap<PandaNetLogId, u64>>, Self::Error> {
        let inner = self.inner.lock().await;
        let mut heights = BTreeMap::new();
        for log_id in logs {
            let Some(last_hash) = inner
                .logs
                .get(&(*author, *log_id))
                .and_then(|hashes| hashes.last())
            else {
                continue;
            };
            let Some(stored) = inner.operations.get(last_hash) else {
                continue;
            };
            heights.insert(*log_id, stored.operation.header.seq_num);
        }
        Ok(Some(heights))
    }

    async fn get_log_size(
        &self,
        author: &VerifyingKey,
        log_id: &PandaNetLogId,
        after: Option<u64>,
        until: Option<u64>,
    ) -> Result<Option<(u64, u64)>, Self::Error> {
        let inner = self.inner.lock().await;
        let Some(hashes) = inner.logs.get(&(*author, *log_id)) else {
            return Ok(None);
        };
        let mut operations = 0_u64;
        let mut bytes = 0_u64;
        for hash in hashes {
            let Some(stored) = inner.operations.get(hash) else {
                continue;
            };
            if after.is_some_and(|after| stored.operation.header.seq_num <= after)
                || until.is_some_and(|until| stored.operation.header.seq_num > until)
            {
                continue;
            }
            operations += 1;
            bytes += stored.header_bytes.len() as u64 + stored.operation.header.payload_size;
        }
        Ok(Some((operations, bytes)))
    }

    async fn get_log_entries(
        &self,
        author: &VerifyingKey,
        log_id: &PandaNetLogId,
        after: Option<u64>,
        until: Option<u64>,
    ) -> Result<Option<Vec<(Operation<()>, Vec<u8>)>>, Self::Error> {
        let inner = self.inner.lock().await;
        let Some(hashes) = inner.logs.get(&(*author, *log_id)) else {
            return Ok(None);
        };
        let mut entries = Vec::new();
        for hash in hashes {
            let Some(stored) = inner.operations.get(hash) else {
                continue;
            };
            if after.is_some_and(|after| stored.operation.header.seq_num <= after)
                || until.is_some_and(|until| stored.operation.header.seq_num > until)
            {
                continue;
            }
            entries.push((stored.operation.clone(), stored.header_bytes.clone()));
        }
        Ok(Some(entries))
    }

    async fn prune_entries(
        &self,
        _author: &VerifyingKey,
        _log_id: &PandaNetLogId,
        _until: &u64,
    ) -> Result<u64, Self::Error> {
        Ok(0)
    }
}

impl TopicStore<Topic, VerifyingKey, PandaNetLogId> for PandaNetQuarantineStore {
    type Error = PandaNetQuarantineStoreError;

    async fn associate(
        &self,
        topic: &Topic,
        author: &VerifyingKey,
        data_id: &PandaNetLogId,
    ) -> Result<bool, Self::Error> {
        let mut inner = self.inner.lock().await;
        let logs = inner.topics.entry(*topic).or_default();
        let log_ids = logs.entry(*author).or_default();
        if log_ids.contains(data_id) {
            return Ok(false);
        }
        log_ids.push(*data_id);
        Ok(true)
    }

    async fn remove(
        &self,
        topic: &Topic,
        author: &VerifyingKey,
        data_id: &PandaNetLogId,
    ) -> Result<bool, Self::Error> {
        let mut inner = self.inner.lock().await;
        let Some(logs) = inner.topics.get_mut(topic) else {
            return Ok(false);
        };
        let Some(log_ids) = logs.get_mut(author) else {
            return Ok(false);
        };
        let Some(index) = log_ids.iter().position(|log_id| log_id == data_id) else {
            return Ok(false);
        };
        log_ids.remove(index);
        Ok(true)
    }

    async fn resolve(&self, topic: &Topic) -> Result<Logs<PandaNetLogId>, Self::Error> {
        let inner = self.inner.lock().await;
        Ok(inner.topics.get(topic).cloned().unwrap_or_default())
    }
}

#[derive(Debug)]
pub(crate) struct PandaNetQuarantineStoreError {
    message: String,
}

impl Display for PandaNetQuarantineStoreError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for PandaNetQuarantineStoreError {}

fn quarantine_store_error(error: impl ToString) -> PandaNetTransportError {
    PandaNetTransportError::QuarantineLog {
        message: error.to_string(),
    }
}
