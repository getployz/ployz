use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

use p2panda_core::{Body, Header, Operation, PrivateKey, PublicKey};
use p2panda_net::TopicId;
use p2panda_store::{LogStore, MemoryStore, OperationStore};
use p2panda_sync::protocols::Logs;
use p2panda_sync::traits::TopicMap;
use tokio::sync::RwLock;

use crate::PandaNetTransportError;

pub(crate) type PandaNetLogId = u64;
pub(crate) type PandaNetStore = MemoryStore<PandaNetLogId, ()>;

#[derive(Clone)]
pub(crate) struct PandaNetTopicMap(Arc<RwLock<HashMap<TopicId, Logs<PandaNetLogId>>>>);

impl PandaNetTopicMap {
    fn new() -> Self {
        Self(Arc::new(RwLock::new(HashMap::new())))
    }

    async fn associate(&self, topic: TopicId, public_key: PublicKey, log_id: PandaNetLogId) {
        let mut topics = self.0.write().await;
        let logs = topics.entry(topic).or_default();
        let log_ids = logs.entry(public_key).or_default();
        if !log_ids.contains(&log_id) {
            log_ids.push(log_id);
        }
    }
}

impl TopicMap<TopicId, Logs<PandaNetLogId>> for PandaNetTopicMap {
    type Error = Infallible;

    async fn get(&self, topic: &TopicId) -> Result<Logs<PandaNetLogId>, Self::Error> {
        let topics = self.0.read().await;
        Ok(topics.get(topic).cloned().unwrap_or_default())
    }
}

#[derive(Clone)]
pub(crate) struct PandaNetQuarantineLog {
    store: PandaNetStore,
    topic_map: PandaNetTopicMap,
    private_key: PrivateKey,
}

impl PandaNetQuarantineLog {
    pub(crate) fn new(private_key: PrivateKey) -> Self {
        Self {
            store: MemoryStore::new(),
            topic_map: PandaNetTopicMap::new(),
            private_key,
        }
    }

    pub(crate) fn store(&self) -> PandaNetStore {
        self.store.clone()
    }

    pub(crate) fn topic_map(&self) -> PandaNetTopicMap {
        self.topic_map.clone()
    }

    pub(crate) async fn append_to_topic(
        &mut self,
        topic: TopicId,
        body: &[u8],
        log_id: PandaNetLogId,
    ) -> Result<Operation<()>, PandaNetTransportError> {
        let (header, header_bytes, body) = self.create_operation(body, log_id).await?;
        let hash = header.hash();
        self.store
            .insert_operation(hash, &header, Some(&body), &header_bytes, &log_id)
            .await
            .map_err(quarantine_store_error)?;
        self.topic_map
            .associate(topic, self.private_key.public_key(), log_id)
            .await;
        Ok(Operation {
            hash,
            header,
            body: Some(body),
        })
    }

    pub(crate) async fn store_received_operation(
        &mut self,
        topic: TopicId,
        operation: &Operation<()>,
        log_id: PandaNetLogId,
    ) -> Result<(), PandaNetTransportError> {
        let header_bytes = operation.header.to_bytes();
        self.store
            .insert_operation(
                operation.hash,
                &operation.header,
                operation.body.as_ref(),
                &header_bytes,
                &log_id,
            )
            .await
            .map_err(quarantine_store_error)?;
        self.topic_map
            .associate(topic, operation.header.public_key, log_id)
            .await;
        Ok(())
    }

    async fn create_operation(
        &self,
        bytes: &[u8],
        log_id: PandaNetLogId,
    ) -> Result<(Header<()>, Vec<u8>, Body), PandaNetTransportError> {
        let body = Body::new(bytes);
        let (seq_num, backlink) = self
            .store
            .latest_operation(&self.private_key.public_key(), &log_id)
            .await
            .map_err(quarantine_store_error)?
            .map(|(header, _)| (header.seq_num + 1, Some(header.hash())))
            .unwrap_or((0, None));

        let mut header = Header::<()> {
            version: 1,
            public_key: self.private_key.public_key(),
            signature: None,
            payload_size: body.size(),
            payload_hash: Some(body.hash()),
            timestamp: seq_num,
            seq_num,
            backlink,
            previous: vec![],
            extensions: (),
        };
        header.sign(&self.private_key);
        let header_bytes = header.to_bytes();
        Ok((header, header_bytes, body))
    }
}

fn quarantine_store_error(error: impl ToString) -> PandaNetTransportError {
    PandaNetTransportError::QuarantineLog {
        message: error.to_string(),
    }
}
