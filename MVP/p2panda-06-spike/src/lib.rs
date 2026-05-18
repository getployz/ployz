use p2panda_auth::Access;
use p2panda_auth::group::{GroupAction, GroupMember};
use p2panda_auth::processor::GroupsArgs;
use p2panda_core::cbor::{decode_cbor, encode_cbor};
use p2panda_core::{
    Body, Extension, Hash, Header, Operation, RawOperation, SigningKey, Timestamp, Topic,
    VerifyingKey, validate_operation,
};
use p2panda_store::logs::LogStore;
use p2panda_store::operations::OperationStore;
use p2panda_store::topics::TopicStore;
use p2panda_store::{SqliteStore, Transaction};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Serialize, Deserialize)]
pub enum IslandMemberCondition {
    ReplicaImporter,
    FactWriter,
}

impl p2panda_auth::traits::Conditions for IslandMemberCondition {}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Serialize, Deserialize)]
pub enum FactKind {
    Node,
    Deploy,
    Serving,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PloyzLogId(String);

impl PloyzLogId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PloyzFactExtensions {
    pub island_id: String,
    pub fact_key: String,
    pub principal_id: String,
    pub authority_epoch: u64,
    pub content_hash: Hash,
    pub kind: FactKind,
    pub log_id: PloyzLogId,
}

impl Extension<PloyzLogId> for PloyzFactExtensions {
    fn extract(header: &Header<Self>) -> Option<PloyzLogId> {
        Some(header.extensions.log_id.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthzExtensions {
    pub log_id: PloyzLogId,
    pub groups: Option<GroupsArgs<IslandMemberCondition>>,
}

impl Extension<PloyzLogId> for AuthzExtensions {
    fn extract(header: &Header<Self>) -> Option<PloyzLogId> {
        Some(header.extensions.log_id.clone())
    }
}

impl Extension<GroupsArgs<IslandMemberCondition>> for AuthzExtensions {
    fn extract(header: &Header<Self>) -> Option<GroupsArgs<IslandMemberCondition>> {
        header.extensions.groups.clone()
    }
}

pub type CanonicalFactLogSync =
    p2panda_net::sync::LogSync<SqliteStore, PloyzLogId, PloyzFactExtensions>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedPloyzFact {
    pub operation_id: Hash,
    pub body_hash: Hash,
    pub content_hash: Hash,
    pub author: VerifyingKey,
    pub fact_key: String,
    pub payload: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum SpikeError {
    #[error("operation had no body")]
    MissingBody,

    #[error("declared content hash did not match operation body hash")]
    ContentHashMismatch,

    #[error(transparent)]
    Operation(#[from] p2panda_core::OperationError),

    #[error(transparent)]
    Decode(#[from] p2panda_core::cbor::DecodeError),

    #[error(transparent)]
    Encode(#[from] p2panda_core::cbor::EncodeError),
}

pub fn canonical_fact_operation(
    signing_key: &SigningKey,
    log_id: PloyzLogId,
    fact_key: impl Into<String>,
    payload: impl AsRef<[u8]>,
) -> Operation<PloyzFactExtensions> {
    let body = Body::new(payload.as_ref());
    let content_hash = body.hash();
    let extensions = PloyzFactExtensions {
        island_id: "default".to_string(),
        fact_key: fact_key.into(),
        principal_id: "node-a".to_string(),
        authority_epoch: 1,
        content_hash,
        kind: FactKind::Serving,
        log_id,
    };

    let mut header = Header {
        version: 1,
        verifying_key: signing_key.verifying_key(),
        signature: None,
        payload_size: body.size(),
        payload_hash: Some(content_hash),
        timestamp: Timestamp::now(),
        seq_num: 0,
        backlink: None,
        extensions,
    };
    header.sign(signing_key);

    Operation {
        hash: header.hash(),
        header,
        body: Some(body),
    }
}

pub fn to_raw_operation(
    operation: &Operation<PloyzFactExtensions>,
) -> Result<RawOperation, SpikeError> {
    Ok((
        encode_cbor(operation.header())?,
        operation.body().map(Body::to_bytes),
    ))
}

pub fn decode_fact(raw: RawOperation) -> Result<DecodedPloyzFact, SpikeError> {
    let header: Header<PloyzFactExtensions> = decode_cbor(&raw.0[..])?;
    let body = raw.1.map(Body::from).ok_or(SpikeError::MissingBody)?;
    let operation = Operation {
        hash: header.hash(),
        header,
        body: Some(body),
    };
    validate_operation(&operation)?;

    let body = operation.body().ok_or(SpikeError::MissingBody)?;
    let body_hash = body.hash();
    let payload = body.to_bytes();
    let content_hash = operation.header.extensions.content_hash;
    let fact_key = operation.header.extensions.fact_key.clone();
    if body_hash != content_hash {
        return Err(SpikeError::ContentHashMismatch);
    }

    Ok(DecodedPloyzFact {
        operation_id: operation.hash,
        body_hash,
        content_hash,
        author: operation.header.verifying_key,
        fact_key,
        payload,
    })
}

pub fn accepts_canonical_log_sync(_sync: &CanonicalFactLogSync) {}

pub async fn store_round_trip(
    store: &SqliteStore,
    topic: &Topic,
    operation: &Operation<PloyzFactExtensions>,
) -> Result<Option<Operation<PloyzFactExtensions>>, p2panda_store::SqliteError> {
    let log_id = operation.header.extensions.log_id.clone();
    let author = operation.header.verifying_key;
    let permit = store.begin().await?;
    <SqliteStore as TopicStore<Topic, VerifyingKey, PloyzLogId>>::associate(
        store, topic, &author, &log_id,
    )
    .await?;
    <SqliteStore as OperationStore<Operation<PloyzFactExtensions>, Hash, PloyzLogId>>::insert_operation(
        store,
        &operation.hash,
        operation,
        &log_id,
    )
    .await?;
    store.commit(permit).await?;

    <SqliteStore as OperationStore<Operation<PloyzFactExtensions>, Hash, PloyzLogId>>::get_operation(
        store,
        &operation.hash,
    )
    .await
}

pub async fn latest_from_log(
    store: &SqliteStore,
    author: &VerifyingKey,
    log_id: &PloyzLogId,
) -> Result<Option<Operation<PloyzFactExtensions>>, p2panda_store::SqliteError> {
    <SqliteStore as LogStore<
        Operation<PloyzFactExtensions>,
        VerifyingKey,
        PloyzLogId,
        u64,
        Hash,
    >>::get_latest_entry(store, author, log_id)
    .await
}

pub async fn topic_associations(
    store: &SqliteStore,
    topic: &Topic,
) -> Result<Vec<(VerifyingKey, Vec<PloyzLogId>)>, p2panda_store::SqliteError> {
    let associations =
        <SqliteStore as TopicStore<Topic, VerifyingKey, PloyzLogId>>::resolve(store, topic).await?;
    Ok(associations.into_iter().collect())
}

pub type AuthzGroupsProcessor = p2panda_auth::processor::GroupsProcessor<
    Topic,
    AuthzExtensions,
    PloyzLogId,
    IslandMemberCondition,
>;

pub async fn process_authz_create_group(
    store: &SqliteStore,
    state_id: &String,
    topic: &Topic,
    manager_log: &p2panda_core::test_utils::TestLog,
) -> Result<VerifyingKey, Box<dyn std::error::Error>> {
    let group_id = SigningKey::generate().verifying_key();
    let args = GroupsArgs {
        group_id,
        action: GroupAction::Create {
            initial_members: vec![(
                GroupMember::Individual(manager_log.author()),
                Access::manage().with_conditions(IslandMemberCondition::FactWriter),
            )],
        },
        dependencies: vec![],
    };
    let operation = manager_log.operation(
        &[],
        AuthzExtensions {
            log_id: PloyzLogId::new("authz"),
            groups: Some(args),
        },
    );

    AuthzGroupsProcessor::new(store.clone())
        .process(state_id, topic, &operation)
        .await?;

    Ok(group_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2panda_auth::group::GroupCrdtState;
    use p2panda_auth::processor::GroupsOperation;
    use p2panda_store::groups::GroupsStore;

    type GroupsState = GroupCrdtState<
        VerifyingKey,
        Hash,
        GroupsOperation<IslandMemberCondition>,
        IslandMemberCondition,
    >;

    #[test]
    fn canonical_fact_operation_round_trips_as_raw_operation() {
        let signing_key = SigningKey::generate();
        let operation = canonical_fact_operation(
            &signing_key,
            PloyzLogId::new("facts"),
            "/facts/serving/commit-1",
            b"route snapshot",
        );

        let raw = to_raw_operation(&operation).unwrap();
        let decoded = decode_fact(raw).unwrap();

        assert_eq!(decoded.operation_id, operation.hash);
        assert_eq!(decoded.author, signing_key.verifying_key());
        assert_eq!(decoded.fact_key, "/facts/serving/commit-1");
        assert_eq!(decoded.payload, b"route snapshot");
        assert_eq!(decoded.body_hash, decoded.content_hash);
    }

    #[test]
    fn canonical_fact_rejects_content_hash_tampering() {
        let signing_key = SigningKey::generate();
        let mut operation = canonical_fact_operation(
            &signing_key,
            PloyzLogId::new("facts"),
            "/facts/serving/commit-1",
            b"route snapshot",
        );
        operation.header.extensions.content_hash = Hash::digest(b"different payload");
        operation.header.sign(&signing_key);

        let raw = to_raw_operation(&operation).unwrap();
        assert!(matches!(
            decode_fact(raw),
            Err(SpikeError::ContentHashMismatch)
        ));
    }

    #[tokio::test]
    async fn sqlite_store_supports_operation_log_and_topic_traits_for_canonical_facts() {
        let store = SqliteStore::temporary().await;
        let topic = Topic::random();
        let signing_key = SigningKey::generate();
        let log_id = PloyzLogId::new("facts");
        let operation = canonical_fact_operation(
            &signing_key,
            log_id.clone(),
            "/facts/serving/commit-1",
            b"route snapshot",
        );

        let stored = store_round_trip(&store, &topic, &operation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.hash, operation.hash);

        let latest = latest_from_log(&store, &signing_key.verifying_key(), &log_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(latest.hash, operation.hash);

        let associations = topic_associations(&store, &topic).await.unwrap();
        assert_eq!(
            associations,
            vec![(signing_key.verifying_key(), vec![log_id])]
        );
    }

    #[test]
    fn p2panda_net_log_sync_accepts_canonical_fact_store_shape() {
        fn assert_log_sync_shape<S, L, E>()
        where
            S: LogStore<Operation<E>, VerifyingKey, L, u64, Hash>
                + TopicStore<Topic, VerifyingKey, L>
                + Clone
                + Send
                + 'static,
            L: p2panda_core::LogId + std::fmt::Debug + Send + 'static,
            E: p2panda_core::Extensions + Send + 'static,
        {
            let _ = std::any::type_name::<p2panda_net::sync::LogSync<S, L, E>>();
        }

        assert_log_sync_shape::<SqliteStore, PloyzLogId, PloyzFactExtensions>();
    }

    #[tokio::test]
    async fn p2panda_auth_processor_persists_group_state_with_conditions() {
        let store = SqliteStore::temporary().await;
        let topic = Topic::random();
        let manager_log = p2panda_core::test_utils::TestLog::new();

        let state_id = "island-default".to_string();
        let group_id = process_authz_create_group(&store, &state_id, &topic, &manager_log)
            .await
            .unwrap();

        let permit = store.begin().await.unwrap();
        let state: GroupsState = store.get_groups_state(&state_id).await.unwrap().unwrap();
        store.commit(permit).await.unwrap();

        let members = state.members(group_id);
        assert_eq!(members.len(), 1);
        assert_eq!(
            members,
            vec![(
                manager_log.author(),
                Access::manage().with_conditions(IslandMemberCondition::FactWriter)
            )]
        );
    }
}
