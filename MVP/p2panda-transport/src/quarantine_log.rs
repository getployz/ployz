use p2panda_core_git::{Body, Hash, Header, Operation, SigningKey, Topic, VerifyingKey};
use p2panda_store_git::logs::LogStore;
use p2panda_store_git::operations::OperationStore;
use p2panda_store_git::topics::TopicStore;
use p2panda_store_git::{SqliteStore, SqliteStoreBuilder, Transaction};

use crate::PandaNetTransportError;

pub type PandaNetLogId = u64;

#[derive(Clone)]
pub struct PandaNetQuarantineLog {
    store: SqliteStore,
    signing_key: SigningKey,
}

impl PandaNetQuarantineLog {
    pub async fn new(signing_key: SigningKey) -> Result<Self, PandaNetTransportError> {
        let store = SqliteStoreBuilder::new()
            .max_connections(1)
            .build()
            .await
            .map_err(|error| PandaNetTransportError::QuarantineLog {
                message: error.to_string(),
            })?;
        Ok(Self { store, signing_key })
    }

    pub fn store(&self) -> SqliteStore {
        self.store.clone()
    }

    pub fn author(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    pub async fn append_to_topic(
        &mut self,
        topic: &Topic,
        body: &[u8],
        log_id: PandaNetLogId,
    ) -> Result<Operation<()>, PandaNetTransportError> {
        let (header, body) = self.create_operation(body, log_id).await?;
        let id = header.hash();
        let operation = Operation {
            hash: id,
            header,
            body: Some(body),
        };

        let permit =
            self.store
                .begin()
                .await
                .map_err(|error| PandaNetTransportError::QuarantineLog {
                    message: error.to_string(),
                })?;
        self.store
            .insert_operation(&id, &operation, &log_id)
            .await
            .map_err(|error| PandaNetTransportError::QuarantineLog {
                message: error.to_string(),
            })?;
        self.store
            .commit(permit)
            .await
            .map_err(|error| PandaNetTransportError::QuarantineLog {
                message: error.to_string(),
            })?;

        let permit =
            self.store
                .begin()
                .await
                .map_err(|error| PandaNetTransportError::QuarantineLog {
                    message: error.to_string(),
                })?;
        self.store
            .associate(topic, &self.signing_key.verifying_key(), &log_id)
            .await
            .map_err(|error| PandaNetTransportError::QuarantineLog {
                message: error.to_string(),
            })?;
        self.store
            .commit(permit)
            .await
            .map_err(|error| PandaNetTransportError::QuarantineLog {
                message: error.to_string(),
            })?;

        Ok(operation)
    }

    async fn create_operation(
        &self,
        bytes: &[u8],
        log_id: PandaNetLogId,
    ) -> Result<(Header<()>, Body), PandaNetTransportError> {
        let body = Body::new(bytes);
        let (seq_num, backlink) = <SqliteStore as LogStore<
            Operation<()>,
            VerifyingKey,
            u64,
            u64,
            Hash,
        >>::get_latest_entry(
            &self.store, &self.signing_key.verifying_key(), &log_id
        )
        .await
        .map_err(|error| PandaNetTransportError::QuarantineLog {
            message: error.to_string(),
        })?
        .map(|operation| (operation.header.seq_num + 1, Some(operation.hash)))
        .unwrap_or((0, None));

        let mut header = Header::<()> {
            version: 1,
            verifying_key: self.signing_key.verifying_key(),
            signature: None,
            payload_size: body.size(),
            payload_hash: Some(body.hash()),
            timestamp: seq_num.into(),
            seq_num,
            backlink,
            extensions: (),
        };
        header.sign(&self.signing_key);
        Ok((header, body))
    }
}
