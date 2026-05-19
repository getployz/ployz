use super::*;

impl PandaFactStore {
    async fn ensure_transport_topic_association(
        &mut self,
        topic: &Topic,
        author: &VerifyingKey,
        log_id: &IslandLog,
    ) -> Result<bool> {
        self.backend.associate_topic(topic, author, log_id).await
    }
}

pub(super) enum PandaBackendIngest {
    Inserted(Operation<PandaFactExtensions>),
    AlreadyPresent(Operation<PandaFactExtensions>),
}

impl PandaFactStore {
    #[must_use]
    pub fn new(authorizer: Arc<dyn FactAuthorizer>) -> Self {
        Self::from_backend(
            authorizer,
            PandaFactBackend::Memory(PandaMemoryStore::default()),
        )
    }

    pub async fn open_sqlite(
        authorizer: Arc<dyn FactAuthorizer>,
        config: PandaSqliteOpenConfig,
    ) -> Result<Self> {
        prepare_sqlite_parent(&config.path)?;
        let url = sqlite_url(&config.path);
        let sqlite = SqliteStoreBuilder::new()
            .database_url(&url)
            .max_connections(config.max_connections)
            .build()
            .await
            .map_err(store_error)?;
        let mut store = Self::from_backend(authorizer, PandaFactBackend::Sqlite(sqlite));
        for trusted in config.trusted_author_keys {
            store.bind_author_key(&trusted.island, &trusted.principal, trusted.author_key.0)?;
        }
        for authority in config.authority_snapshots {
            store.install_authority_snapshot(authority);
        }
        store.rebuild_indexes(&config.known_islands).await?;
        Ok(store)
    }

    fn from_backend(authorizer: Arc<dyn FactAuthorizer>, backend: PandaFactBackend) -> Self {
        Self {
            backend,
            authorizer,
            derived_index: DerivedFactIndex::empty(),
            operations: Vec::new(),
            operation_hashes: BTreeSet::new(),
            authority_snapshots: BTreeMap::new(),
            trusted_author_keys: BTreeMap::new(),
            trusted_replica_peers: BTreeSet::new(),
        }
    }

    pub fn install_authority_snapshot(&mut self, authority: IslandAuthoritySnapshot) {
        self.authority_snapshots
            .insert(authority.island().clone(), authority);
    }

    pub fn trust_author_key(
        &mut self,
        island: &IslandId,
        principal: PrincipalId,
        author_key: PandaFactAuthorKey,
    ) -> Result<()> {
        self.bind_author_key(island, &principal, author_key.0)
    }

    pub fn trust_replica_peer(&mut self, island: &IslandId, principal: PrincipalId) {
        self.trusted_replica_peers
            .insert((island.clone(), principal));
    }

    fn fact_authority_for<'a>(&'a self, island: &'a IslandId) -> FactAuthority<'a> {
        match self.authority_snapshots.get(island) {
            Some(authority) => FactAuthority::Snapshot(authority),
            None => FactAuthority::Manual {
                island,
                trusted_author_keys: &self.trusted_author_keys,
                trusted_replica_peers: &self.trusted_replica_peers,
            },
        }
    }

    #[must_use]
    pub fn can_write_fact(&self, session: &BusSession, key: &FactKey) -> bool {
        self.authorizer
            .can_session_access_fact(session, key, FactAccess::Write)
    }

    pub async fn write_fact_payload(
        &mut self,
        session: &BusSession,
        author: &PandaFactAuthor,
        key: FactKey,
        payload: FactPayload,
    ) -> Result<PandaFactWriteOutcome> {
        if session.principal() != author.principal() {
            return Err(PandaFactError::PrincipalMismatch {
                session: session.principal().clone(),
                author: author.principal().clone(),
            });
        }
        if !self
            .authorizer
            .can_session_access_fact(session, &key, FactAccess::Write)
        {
            return Err(PandaFactError::UnauthorizedWrite {
                island: session.island().clone(),
                principal: session.principal().clone(),
                key,
            });
        }
        let authority_epoch = self.authorize_local_author(session, author)?;

        let body = Body::new(payload.as_bytes());
        let content_hash = FactContentHash::for_payload(&payload);
        let metadata = PandaFactMetadata::new(
            session.island().clone(),
            key.clone(),
            author.principal().clone(),
            authority_epoch,
            content_hash.clone(),
        );
        let pre_ingest = self.pre_ingest_status(&metadata);
        if pre_ingest == FactPreIngestStatus::AlreadyPresent {
            return Ok(PandaFactWriteOutcome::AlreadyPresent(metadata));
        }
        let log_position = self.next_log_position(session.island(), author).await?;
        let raw_body = payload.as_bytes().to_vec();

        let mut header = Header {
            version: 1,
            verifying_key: author.public_key(),
            signature: None,
            payload_size: body.size(),
            payload_hash: Some(body.hash()),
            timestamp: p2panda_core::Timestamp::now(),
            seq_num: log_position.seq_num,
            backlink: log_position.backlink,
            extensions: PandaFactExtensions::new(
                session.island(),
                &key,
                author.principal(),
                authority_epoch,
            ),
        };
        header.sign(&author.key);
        let header_bytes =
            encode_cbor(&header).map_err(|error| PandaFactError::InvalidExtensions {
                message: error.to_string(),
            })?;
        let raw_operation = PandaFactOperation::new(header_bytes.clone(), raw_body);
        let result = self
            .backend
            .ingest_operation(header, Some(body), &IslandLog::from(session.island()))
            .await?;

        let operation = match result {
            PandaBackendIngest::Inserted(operation)
            | PandaBackendIngest::AlreadyPresent(operation) => operation,
        };
        validate_operation(&operation).map_err(|_| PandaFactError::InvalidOperation)?;
        let metadata = metadata_from_operation(&operation, content_hash)?;

        self.record_operation(operation.hash, raw_operation);
        Ok(self.record_fact_operation(metadata, payload, pre_ingest))
    }

    /// Export stored operations for deterministic harnesses and debug tooling.
    ///
    /// Product replication should use [`sync_panda_fact_stores`] or a network
    /// transport that carries these opaque operation envelopes back through
    /// [`PandaFactStore::import_replica_operation`].
    pub fn export_operations(&self) -> impl Iterator<Item = &PandaFactOperation> {
        self.operations.iter()
    }

    pub async fn import_operation(
        &mut self,
        session: &BusSession,
        operation: &PandaFactOperation,
    ) -> Result<PandaFactWriteOutcome> {
        let header: Header<PandaFactExtensions> =
            decode_cbor(operation.header()).map_err(|error| PandaFactError::InvalidExtensions {
                message: error.to_string(),
            })?;
        let body_bytes = operation.body().to_vec();
        let body = Body::new(&body_bytes);
        self.import_decoded_operation(session, header, body, operation.header_bytes(), body_bytes)
            .await
    }

    pub async fn import_replica_operation(
        &mut self,
        session: &BusSession,
        operation: &PandaFactOperation,
    ) -> Result<PandaFactWriteOutcome> {
        self.require_trusted_replica_importer(session)?;
        self.import_operation(session, operation).await
    }

    pub(super) async fn import_decoded_operation(
        &mut self,
        session: &BusSession,
        header: Header<PandaFactExtensions>,
        body: Body,
        header_bytes: Vec<u8>,
        body_bytes: Vec<u8>,
    ) -> Result<PandaFactWriteOutcome> {
        let payload = FactPayload::from(body_bytes.clone());
        let content_hash = FactContentHash::for_payload(&payload);
        let metadata = metadata_from_header(&header, content_hash)?;
        let operation_hash = header.hash();
        let public_key = header.verifying_key;
        self.authorize_import(session, &metadata, public_key)?;
        let candidate = Operation {
            hash: operation_hash,
            header: header.clone(),
            body: Some(body.clone()),
        };
        validate_operation(&candidate).map_err(|_| PandaFactError::InvalidOperation)?;
        if self.operation_hashes.contains(&operation_hash) {
            return Ok(PandaFactWriteOutcome::AlreadyPresent(metadata));
        }
        let result = self
            .backend
            .ingest_operation(header, Some(body), &IslandLog::from(&metadata.island))
            .await?;
        let imported = match result {
            PandaBackendIngest::Inserted(imported) => imported,
            PandaBackendIngest::AlreadyPresent(_) => {
                return Ok(PandaFactWriteOutcome::AlreadyPresent(metadata));
            }
        };
        validate_operation(&imported).map_err(|_| PandaFactError::InvalidOperation)?;

        let pre_ingest = self.pre_ingest_status(&metadata);
        self.record_operation(
            operation_hash,
            PandaFactOperation::new(header_bytes, body_bytes),
        );
        Ok(self.record_fact_operation(metadata, payload, pre_ingest))
    }

    fn authorize_import(
        &self,
        session: &BusSession,
        metadata: &PandaFactMetadata,
        public_key: VerifyingKey,
    ) -> Result<()> {
        if session.island() != &metadata.island {
            return Err(PandaFactError::ImportIslandMismatch {
                session: session.island().clone(),
                operation: metadata.island.clone(),
            });
        }
        self.fact_authority_for(&metadata.island)
            .require_active_writer(&metadata.author, public_key)?;
        if !self.authorizer.can_principal_access_fact(
            &metadata.island,
            &metadata.author,
            &metadata.key,
            FactAccess::Write,
        ) {
            return Err(PandaFactError::UnauthorizedWrite {
                island: metadata.island.clone(),
                principal: metadata.author.clone(),
                key: metadata.key.clone(),
            });
        }
        Ok(())
    }

    fn require_trusted_replica_importer(&self, session: &BusSession) -> Result<()> {
        self.fact_authority_for(session.island())
            .require_replica_importer(session.principal())
    }

    fn authorize_local_author(
        &mut self,
        session: &BusSession,
        author: &PandaFactAuthor,
    ) -> Result<Option<IslandMemberEpoch>> {
        match self.fact_authority_for(session.island()) {
            authority @ FactAuthority::Snapshot(_) => {
                authority.active_writer_epoch(author.principal(), author.public_key())
            }
            FactAuthority::Manual { .. } => {
                self.bind_author_key(session.island(), author.principal(), author.public_key())?;
                Ok(None)
            }
        }
    }

    fn bind_author_key(
        &mut self,
        island: &IslandId,
        principal: &PrincipalId,
        public_key: VerifyingKey,
    ) -> Result<()> {
        match self
            .trusted_author_keys
            .get(&(island.clone(), principal.clone()))
        {
            Some(existing) if *existing == public_key => Ok(()),
            Some(_) => Err(PandaFactError::AuthorKeyMismatch {
                island: island.clone(),
                principal: principal.clone(),
            }),
            None => {
                self.trusted_author_keys
                    .insert((island.clone(), principal.clone()), public_key);
                Ok(())
            }
        }
    }

    fn require_author_key(
        &self,
        metadata: &PandaFactMetadata,
        public_key: VerifyingKey,
    ) -> Result<()> {
        self.fact_authority_for(&metadata.island)
            .require_active_writer(&metadata.author, public_key)
    }

    pub(super) fn validate_sync_scope(
        &self,
        side: PandaFactSyncSide,
        session: &BusSession,
        scope: &PandaFactSyncScope,
    ) -> SyncResult<()> {
        if session.island() != scope.island() {
            return Err(PandaFactSyncError::ReplicaIslandMismatch {
                side,
                session: session.island().clone(),
                scope: scope.island().clone(),
            });
        }
        if self
            .fact_authority_for(scope.island())
            .require_replica_importer(session.principal())
            .is_err()
        {
            return Err(PandaFactSyncError::UnauthorizedReplica {
                side,
                island: scope.island().clone(),
                principal: session.principal().clone(),
            });
        }
        for (principal, author_key) in &scope.trusted_authors {
            if let Err(error) = self.require_sync_scope_author_key(
                scope.island(),
                principal,
                author_key.public_key(),
            ) {
                return Err(match error {
                    PandaFactError::AuthorKeyMismatch { .. } => {
                        PandaFactSyncError::ScopeAuthorKeyMismatch {
                            side,
                            island: scope.island().clone(),
                            principal: principal.clone(),
                        }
                    }
                    PandaFactError::UntrustedAuthorKey { .. } => {
                        PandaFactSyncError::ScopeAuthorKeyMissing {
                            side,
                            island: scope.island().clone(),
                            principal: principal.clone(),
                        }
                    }
                    source @ (PandaFactError::Ingest(_)
                    | PandaFactError::InvalidOperation
                    | PandaFactError::Store { .. }
                    | PandaFactError::InvalidStorePath { .. }
                    | PandaFactError::MissingPayload { .. }
                    | PandaFactError::InvalidAuthorKey { .. }
                    | PandaFactError::InvalidAuthorPrivateKey { .. }
                    | PandaFactError::InvalidExtensions { .. }
                    | PandaFactError::ImportIslandMismatch { .. }
                    | PandaFactError::UnauthorizedReplicaImport { .. }
                    | PandaFactError::OutOfOrderOperation { .. }
                    | PandaFactError::PrincipalMismatch { .. }
                    | PandaFactError::UnauthorizedWrite { .. }) => {
                        PandaFactSyncError::Import { side, source }
                    }
                });
            }
        }
        Ok(())
    }

    fn require_sync_scope_author_key(
        &self,
        island: &IslandId,
        principal: &PrincipalId,
        public_key: VerifyingKey,
    ) -> Result<()> {
        self.fact_authority_for(island)
            .require_active_writer(principal, public_key)
    }

    async fn next_log_position(
        &self,
        island: &IslandId,
        author: &PandaFactAuthor,
    ) -> Result<LogPosition> {
        let latest = self
            .backend
            .latest_operation(&author.public_key(), &IslandLog::from(island))
            .await?;
        Ok(match latest {
            Some(operation) => LogPosition {
                seq_num: operation.header.seq_num + 1,
                backlink: Some(operation.hash),
            },
            None => LogPosition {
                seq_num: 0,
                backlink: None,
            },
        })
    }

    async fn rebuild_indexes(&mut self, islands: &[IslandId]) -> Result<()> {
        self.clear_derived_indexes();
        for island in islands {
            let log_id = IslandLog::from(island);
            for (public_key, _height) in self.backend.get_log_heights(&log_id).await? {
                let Some(raw_operations) = self.backend.get_raw_log(&public_key, &log_id).await?
                else {
                    continue;
                };
                for raw_operation in raw_operations {
                    self.record_rebuilt_operation(raw_operation)?;
                }
            }
        }
        Ok(())
    }
    fn clear_derived_indexes(&mut self) {
        self.derived_index.clear();
        self.operations.clear();
        self.operation_hashes.clear();
    }

    fn record_rebuilt_operation(&mut self, raw_operation: RawOperation) -> Result<()> {
        let (header_bytes, body_bytes) = raw_operation;
        let header: Header<PandaFactExtensions> =
            decode_cbor(header_bytes.as_slice()).map_err(|error| {
                PandaFactError::InvalidExtensions {
                    message: error.to_string(),
                }
            })?;
        let missing_payload_key =
            FactKey::parse(header.extensions.key.clone()).map_err(|error| {
                PandaFactError::InvalidExtensions {
                    message: error.to_string(),
                }
            })?;
        let body_bytes = body_bytes.ok_or(PandaFactError::MissingPayload {
            key: missing_payload_key,
        })?;
        let body = Body::new(&body_bytes);
        let payload = FactPayload::from(body_bytes.clone());
        let content_hash = FactContentHash::for_payload(&payload);
        let metadata = metadata_from_header(&header, content_hash)?;
        self.require_author_key(&metadata, header.verifying_key)?;
        let operation = Operation {
            hash: header.hash(),
            header,
            body: Some(body),
        };
        validate_operation(&operation).map_err(|_| PandaFactError::InvalidOperation)?;
        let pre_ingest = self.pre_ingest_status(&metadata);
        self.record_operation(
            operation.hash,
            PandaFactOperation::new(header_bytes, body_bytes.clone()),
        );
        self.record_fact_operation(metadata, payload, pre_ingest);
        Ok(())
    }

    fn pre_ingest_status(&self, metadata: &PandaFactMetadata) -> FactPreIngestStatus {
        self.derived_index.pre_ingest_status(metadata)
    }

    fn record_operation(&mut self, operation_hash: Hash, operation: PandaFactOperation) {
        if self.operation_hashes.insert(operation_hash) {
            self.operations.push(operation);
        }
    }

    fn record_fact_operation(
        &mut self,
        metadata: PandaFactMetadata,
        payload: FactPayload,
        status: FactPreIngestStatus,
    ) -> PandaFactWriteOutcome {
        self.derived_index
            .record_fact_operation(metadata, payload, status)
    }
}

#[derive(Clone)]
pub struct SharedPandaFactStore {
    pub(crate) store: Arc<Mutex<PandaFactStore>>,
}

impl SharedPandaFactStore {
    #[must_use]
    pub fn new(store: PandaFactStore) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
        }
    }

    pub async fn write_fact_payload(
        &self,
        session: &BusSession,
        author: &PandaFactAuthor,
        key: FactKey,
        payload: FactPayload,
    ) -> Result<PandaFactWriteOutcome> {
        self.store
            .lock()
            .await
            .write_fact_payload(session, author, key, payload)
            .await
    }

    pub async fn write_fact_payload_with_operation(
        &self,
        session: &BusSession,
        author: &PandaFactAuthor,
        key: FactKey,
        payload: FactPayload,
    ) -> Result<PandaFactWrite> {
        let mut store = self.store.lock().await;
        let operation_index = store.operations.len();
        let outcome = store
            .write_fact_payload(session, author, key, payload)
            .await?;
        let operation = match outcome {
            PandaFactWriteOutcome::Inserted(_) | PandaFactWriteOutcome::Conflict(_) => {
                store.operations.get(operation_index).cloned()
            }
            PandaFactWriteOutcome::AlreadyPresent(ref metadata) => {
                store.operation_for_metadata(metadata)?
            }
        };
        Ok(PandaFactWrite::new(outcome, operation))
    }

    pub async fn trust_author_key(
        &self,
        island: &IslandId,
        principal: PrincipalId,
        author_key: PandaFactAuthorKey,
    ) -> Result<()> {
        self.store
            .lock()
            .await
            .trust_author_key(island, principal, author_key)
    }

    pub async fn trust_replica_peer(&self, island: &IslandId, principal: PrincipalId) {
        self.store
            .lock()
            .await
            .trust_replica_peer(island, principal);
    }

    pub async fn install_authority_snapshot(&self, authority: IslandAuthoritySnapshot) {
        self.store
            .lock()
            .await
            .install_authority_snapshot(authority);
    }

    pub async fn import_operation(
        &self,
        session: &BusSession,
        operation: &PandaFactOperation,
    ) -> Result<PandaFactWriteOutcome> {
        self.store
            .lock()
            .await
            .import_operation(session, operation)
            .await
    }

    pub async fn import_replica_operation(
        &self,
        session: &BusSession,
        operation: &PandaFactOperation,
    ) -> Result<PandaFactWriteOutcome> {
        self.store
            .lock()
            .await
            .import_replica_operation(session, operation)
            .await
    }

    pub async fn export_operations(&self) -> Vec<PandaFactOperation> {
        self.store
            .lock()
            .await
            .export_operations()
            .cloned()
            .collect()
    }

    pub async fn associate_transport_topic(
        &self,
        topic: Topic,
        operation: &PandaFactOperation,
    ) -> Result<bool> {
        let operation = operation.to_p2panda_operation()?;
        let log_id = IslandLog::from(&IslandId::new(operation.header.extensions.island.clone()));
        self.store
            .lock()
            .await
            .ensure_transport_topic_association(&topic, &operation.header.verifying_key, &log_id)
            .await
    }

    pub async fn import_replica_p2panda_operation(
        &self,
        session: &BusSession,
        topic: Topic,
        operation: Operation<PandaFactExtensions>,
    ) -> Result<PandaFactWriteOutcome> {
        let fact_operation = PandaFactOperation::from_p2panda_operation(operation.clone())?;
        let outcome = self
            .import_replica_operation(session, &fact_operation)
            .await?;
        let log_id = IslandLog::from(&IslandId::new(operation.header.extensions.island.clone()));
        self.store
            .lock()
            .await
            .ensure_transport_topic_association(&topic, &operation.header.verifying_key, &log_id)
            .await?;
        Ok(outcome)
    }

    pub fn try_can_write_fact(
        &self,
        session: &BusSession,
        key: &FactKey,
    ) -> FactSourceResult<bool> {
        Ok(self
            .store
            .try_lock()
            .map_err(|_| self.unavailable())?
            .can_write_fact(session, key))
    }

    pub async fn list_fact_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        self.store
            .lock()
            .await
            .list_candidates(island, pattern, session)
    }

    pub async fn read_fact_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        self.store
            .lock()
            .await
            .read_payloads(island, candidates, session)
    }

    pub(crate) fn unavailable(&self) -> FactSourceError {
        FactSourceError::Unavailable {
            name: "p2panda fact store".to_string(),
        }
    }
}

impl LogStore<Operation<PandaFactExtensions>, VerifyingKey, PandaFactLogId, u64, Hash>
    for SharedPandaFactStore
{
    type Error = PandaFactStoreAdapterError;

    async fn get_latest_entry(
        &self,
        author: &VerifyingKey,
        log_id: &PandaFactLogId,
    ) -> std::result::Result<Option<Operation<PandaFactExtensions>>, Self::Error> {
        self.store
            .lock()
            .await
            .backend
            .latest_operation(author, log_id)
            .await
            .map_err(PandaFactStoreAdapterError::new)
    }

    async fn get_latest_entry_tx(
        &self,
        author: &VerifyingKey,
        log_id: &PandaFactLogId,
    ) -> std::result::Result<Option<Operation<PandaFactExtensions>>, Self::Error> {
        self.get_latest_entry(author, log_id).await
    }

    async fn get_log_heights(
        &self,
        author: &VerifyingKey,
        logs: &[PandaFactLogId],
    ) -> std::result::Result<Option<BTreeMap<PandaFactLogId, u64>>, Self::Error> {
        let store = self.store.lock().await;
        let mut heights = BTreeMap::new();
        for log_id in logs {
            let Some(operation) = store
                .backend
                .latest_operation(author, log_id)
                .await
                .map_err(PandaFactStoreAdapterError::new)?
            else {
                continue;
            };
            heights.insert(log_id.clone(), operation.header.seq_num);
        }
        Ok((!heights.is_empty()).then_some(heights))
    }

    async fn get_log_size(
        &self,
        author: &VerifyingKey,
        log_id: &PandaFactLogId,
        after: Option<u64>,
        until: Option<u64>,
    ) -> std::result::Result<Option<(u64, u64)>, Self::Error> {
        let entries = self.get_log_entries(author, log_id, after, until).await?;
        Ok(entries.map(|entries| {
            let bytes = entries
                .iter()
                .map(|(operation, header)| header.len() as u64 + operation.header.payload_size)
                .sum();
            (entries.len() as u64, bytes)
        }))
    }

    async fn get_log_entries(
        &self,
        author: &VerifyingKey,
        log_id: &PandaFactLogId,
        after: Option<u64>,
        until: Option<u64>,
    ) -> std::result::Result<Option<Vec<(Operation<PandaFactExtensions>, Vec<u8>)>>, Self::Error>
    {
        let store = self.store.lock().await;
        match &store.backend {
            PandaFactBackend::Memory(memory) => memory
                .get_log_entries(author, log_id, after, until)
                .await
                .map_err(PandaFactStoreAdapterError::new),
            PandaFactBackend::Sqlite(sqlite) => {
                <SqliteStore as LogStore<
                    Operation<PandaFactExtensions>,
                    VerifyingKey,
                    IslandLog,
                    u64,
                    Hash,
                >>::get_log_entries(sqlite, author, log_id, after, until)
                .await
                .map_err(PandaFactStoreAdapterError::new)
            }
        }
    }

    async fn prune_entries(
        &self,
        _author: &VerifyingKey,
        _log_id: &PandaFactLogId,
        _until: &u64,
    ) -> std::result::Result<u64, Self::Error> {
        Ok(0)
    }
}

impl TopicStore<Topic, VerifyingKey, PandaFactLogId> for SharedPandaFactStore {
    type Error = PandaFactStoreAdapterError;

    async fn associate(
        &self,
        topic: &Topic,
        author: &VerifyingKey,
        data_id: &PandaFactLogId,
    ) -> std::result::Result<bool, Self::Error> {
        self.store
            .lock()
            .await
            .ensure_transport_topic_association(topic, author, data_id)
            .await
            .map_err(PandaFactStoreAdapterError::new)
    }

    async fn remove(
        &self,
        topic: &Topic,
        author: &VerifyingKey,
        data_id: &PandaFactLogId,
    ) -> std::result::Result<bool, Self::Error> {
        self.store
            .lock()
            .await
            .backend
            .remove_topic(topic, author, data_id)
            .await
            .map_err(PandaFactStoreAdapterError::new)
    }

    async fn resolve(
        &self,
        topic: &Topic,
    ) -> std::result::Result<BTreeMap<VerifyingKey, Vec<PandaFactLogId>>, Self::Error> {
        self.store
            .lock()
            .await
            .backend
            .resolve_topic(topic)
            .await
            .map_err(PandaFactStoreAdapterError::new)
    }
}

impl PandaFactStore {
    fn operation_for_metadata(
        &self,
        metadata: &PandaFactMetadata,
    ) -> Result<Option<PandaFactOperation>> {
        for operation in self.operations.iter().rev() {
            let header: Header<PandaFactExtensions> =
                decode_cbor(operation.header()).map_err(|error| {
                    PandaFactError::InvalidExtensions {
                        message: error.to_string(),
                    }
                })?;
            let content_hash =
                FactContentHash::for_payload(&FactPayload::from(operation.body().to_vec()));
            if metadata_from_header(&header, content_hash)? == *metadata {
                return Ok(Some(operation.clone()));
            }
        }
        Ok(None)
    }
}
