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
            fact_index: BTreeMap::new(),
            operations: Vec::new(),
            operation_hashes: BTreeSet::new(),
            facts: Vec::new(),
            facts_by_identity: BTreeMap::new(),
            facts_by_key_hash: BTreeMap::new(),
            payloads: BTreeMap::new(),
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

    async fn import_decoded_operation(
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

    fn validate_sync_scope(
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
        self.fact_index.clear();
        self.operations.clear();
        self.operation_hashes.clear();
        self.facts.clear();
        self.facts_by_identity.clear();
        self.facts_by_key_hash.clear();
        self.payloads.clear();
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
        let Some(existing) = self
            .fact_index
            .get(&(metadata.island.clone(), metadata.key.clone()))
        else {
            return FactPreIngestStatus::Inserted;
        };
        if existing.contains(&metadata.content_hash) {
            FactPreIngestStatus::AlreadyPresent
        } else {
            FactPreIngestStatus::Conflict
        }
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
        if status == FactPreIngestStatus::AlreadyPresent {
            return PandaFactWriteOutcome::AlreadyPresent(metadata);
        }
        self.fact_index
            .entry((metadata.island.clone(), metadata.key.clone()))
            .or_default()
            .insert(metadata.content_hash.clone());
        self.payloads
            .entry(metadata.content_hash.clone())
            .or_insert(payload);
        let identity = StoredFactIdentity::from_metadata(&metadata);
        let key_hash = StoredFactKeyHash::from_metadata(&metadata);
        self.facts_by_identity.entry(identity).or_insert_with(|| {
            let index = self.facts.len();
            self.facts.push(StoredFactOperation::new(metadata.clone()));
            self.facts_by_key_hash.insert(key_hash, index);
            index
        });
        match status {
            FactPreIngestStatus::Inserted => PandaFactWriteOutcome::Inserted(metadata),
            FactPreIngestStatus::Conflict => PandaFactWriteOutcome::Conflict(metadata),
            FactPreIngestStatus::AlreadyPresent => PandaFactWriteOutcome::AlreadyPresent(metadata),
        }
    }
}

type SyncEvent = LogSyncEvent<PandaFactExtensions>;
type SyncMessage = LogSyncMessage<IslandLog>;

/// Synchronize two canonical fact stores while preserving Ployz import checks.
///
/// This remains the deterministic same-process proof path. Network transport
/// may replace the message carrier, but received operations must still enter
/// through the canonical import path before becoming projection-visible truth.
pub async fn sync_panda_fact_stores(
    left: &mut PandaFactStore,
    left_session: &BusSession,
    right: &mut PandaFactStore,
    right_session: &BusSession,
    scope: &PandaFactSyncScope,
) -> SyncResult<PandaFactSyncReport> {
    left.validate_sync_scope(PandaFactSyncSide::Left, left_session, scope)?;
    right.validate_sync_scope(PandaFactSyncSide::Right, right_session, scope)?;

    let logs = scope.logs();
    match (left.backend.clone(), right.backend.clone()) {
        (PandaFactBackend::Memory(left_backend), PandaFactBackend::Memory(right_backend)) => {
            run_log_sync_pair(
                left_backend,
                right_backend,
                logs,
                (left, left_session),
                (right, right_session),
            )
            .await
        }
        (PandaFactBackend::Memory(left_backend), PandaFactBackend::Sqlite(right_backend)) => {
            run_log_sync_pair(
                left_backend,
                right_backend,
                logs,
                (left, left_session),
                (right, right_session),
            )
            .await
        }
        (PandaFactBackend::Sqlite(left_backend), PandaFactBackend::Memory(right_backend)) => {
            run_log_sync_pair(
                left_backend,
                right_backend,
                logs,
                (left, left_session),
                (right, right_session),
            )
            .await
        }
        (PandaFactBackend::Sqlite(left_backend), PandaFactBackend::Sqlite(right_backend)) => {
            run_log_sync_pair(
                left_backend,
                right_backend,
                logs,
                (left, left_session),
                (right, right_session),
            )
            .await
        }
    }
}

async fn run_log_sync_pair<LeftStore, RightStore>(
    left_store: LeftStore,
    right_store: RightStore,
    logs: Logs<IslandLog>,
    left_replica: (&mut PandaFactStore, &BusSession),
    right_replica: (&mut PandaFactStore, &BusSession),
) -> SyncResult<PandaFactSyncReport>
where
    LeftStore: LogStore<Operation<PandaFactExtensions>, VerifyingKey, IslandLog, u64, Hash>
        + Clone
        + Send
        + 'static,
    RightStore: LogStore<Operation<PandaFactExtensions>, VerifyingKey, IslandLog, u64, Hash>
        + Clone
        + Send
        + 'static,
{
    let (left_tx, right_rx) = mpsc::channel::<SyncMessage>(LOG_SYNC_MESSAGE_CAPACITY);
    let (right_tx, left_rx) = mpsc::channel::<SyncMessage>(LOG_SYNC_MESSAGE_CAPACITY);
    let (left_event_tx, left_event_rx) = broadcast::channel(LOG_SYNC_EVENT_CAPACITY);
    let (right_event_tx, right_event_rx) = broadcast::channel(LOG_SYNC_EVENT_CAPACITY);

    let (left, left_session) = left_replica;
    let (right, right_session) = right_replica;
    let left_events =
        collect_and_import_sync_events(PandaFactSyncSide::Left, left_event_rx, left, left_session);
    let right_events = collect_and_import_sync_events(
        PandaFactSyncSide::Right,
        right_event_rx,
        right,
        right_session,
    );

    let mut left_sink = left_tx;
    let mut right_sink = right_tx;
    let mut left_stream = left_rx.map(Ok::<_, Infallible>);
    let mut right_stream = right_rx.map(Ok::<_, Infallible>);
    let left_sync = LogSync::new(left_store, logs.clone(), left_event_tx);
    let right_sync = LogSync::new(right_store, logs, right_event_tx);

    let (left_result, right_result, left_report, right_report) = tokio::join!(
        left_sync.run(&mut left_sink, &mut left_stream),
        right_sync.run(&mut right_sink, &mut right_stream),
        left_events,
        right_events,
    );
    let mut left_report = left_report?;
    let mut right_report = right_report?;
    let (_, left_metrics) = left_result.map_err(|source| PandaFactSyncError::Protocol {
        side: PandaFactSyncSide::Left,
        source,
    })?;
    let (_, right_metrics) = right_result.map_err(|source| PandaFactSyncError::Protocol {
        side: PandaFactSyncSide::Right,
        source,
    })?;

    apply_metrics(&mut left_report, &left_metrics);
    apply_metrics(&mut right_report, &right_metrics);
    Ok(PandaFactSyncReport {
        left: left_report,
        right: right_report,
    })
}

async fn collect_and_import_sync_events(
    side: PandaFactSyncSide,
    mut events: broadcast::Receiver<SyncEvent>,
    store: &mut PandaFactStore,
    session: &BusSession,
) -> SyncResult<PandaFactSyncPeerReport> {
    let mut report = PandaFactSyncPeerReport::default();
    loop {
        match events.recv().await {
            Ok(LogSyncEvent::OperationReceived { operation, .. }) => {
                import_synced_operation(side, store, session, *operation, &mut report).await?;
            }
            Ok(LogSyncEvent::MetricsExchanged { .. }) => {}
            Err(broadcast::error::RecvError::Closed) => return Ok(report),
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                return Err(PandaFactSyncError::EventReceiverLagged { side, skipped });
            }
        }
    }
}

async fn import_synced_operation(
    side: PandaFactSyncSide,
    store: &mut PandaFactStore,
    session: &BusSession,
    operation: Operation<PandaFactExtensions>,
    report: &mut PandaFactSyncPeerReport,
) -> SyncResult<()> {
    let Operation { header, body, .. } = operation;
    let body = body.ok_or(PandaFactSyncError::MissingOperationBody { side })?;
    report.received += 1;
    let header_bytes = encode_cbor(&header).map_err(|error| PandaFactSyncError::Import {
        side,
        source: PandaFactError::InvalidExtensions {
            message: error.to_string(),
        },
    })?;
    let body_bytes = body.to_bytes();
    match store
        .import_decoded_operation(session, header, body, header_bytes, body_bytes)
        .await
    {
        Ok(PandaFactWriteOutcome::Inserted(_)) => report.imported += 1,
        Ok(PandaFactWriteOutcome::AlreadyPresent(_)) => report.duplicate += 1,
        Ok(PandaFactWriteOutcome::Conflict(_)) => report.conflict += 1,
        Err(source) => {
            return Err(PandaFactSyncError::Import { side, source });
        }
    }
    Ok(())
}

fn apply_metrics(report: &mut PandaFactSyncPeerReport, metrics: &LogSyncMetrics) {
    report.bytes_received = metrics.received_bytes;
    report.bytes_sent = metrics.sent_bytes;
}

impl FactSource for PandaFactStore {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        if island != session.island() {
            return Ok(Vec::new());
        }
        if let Ok(exact_key) = FactKey::parse(pattern.as_str()) {
            return Ok(self.exact_candidates(island, &exact_key, pattern, session));
        }
        let candidates = self
            .facts
            .iter()
            .filter(|stored| stored.metadata.island == *island)
            .filter_map(|stored| self.candidate_for(stored, island, pattern, session))
            .collect::<Vec<_>>();
        Ok(candidates)
    }

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        if island != session.island() {
            return Ok(BTreeMap::new());
        }
        let mut payloads = BTreeMap::new();
        for candidate in candidates {
            if candidate.island() != island {
                continue;
            }
            let identity = StoredFactIdentity::from_candidate(candidate);
            let Some(stored) = self
                .facts_by_identity
                .get(&identity)
                .and_then(|index| self.facts.get(*index))
            else {
                continue;
            };
            let Some(current) =
                self.candidate_for(stored, island, &exact_pattern(candidate), session)
            else {
                continue;
            };
            if current != *candidate || !candidate_payload_is_readable(current.status()) {
                continue;
            }
            if let Some(payload) = self.payloads.get(&stored.metadata.content_hash) {
                payloads.insert(stored.metadata.content_hash.clone(), payload.clone());
            }
        }
        Ok(payloads)
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

    fn unavailable(&self) -> FactSourceError {
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

impl FactSource for SharedPandaFactStore {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        self.store
            .try_lock()
            .map_err(|_| self.unavailable())?
            .list_candidates(island, pattern, session)
    }

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<BTreeMap<FactContentHash, FactPayload>> {
        self.store
            .try_lock()
            .map_err(|_| self.unavailable())?
            .read_payloads(island, candidates, session)
    }
}

fn exact_pattern(candidate: &FactCandidate) -> FactKeyPattern {
    FactKeyPattern::parse(candidate.key().as_str())
        .expect("stored fact candidate key is always a valid exact fact pattern")
}

fn candidate_payload_is_readable(status: CandidateStatus) -> bool {
    matches!(
        status,
        CandidateStatus::Verified | CandidateStatus::Conflict
    )
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

    fn exact_candidates(
        &self,
        island: &IslandId,
        key: &FactKey,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> Vec<FactCandidate> {
        let Some(content_hashes) = self.fact_index.get(&(island.clone(), key.clone())) else {
            return Vec::new();
        };
        content_hashes
            .iter()
            .filter_map(|content_hash| {
                let identity =
                    StoredFactKeyHash::new(island.clone(), key.clone(), content_hash.clone());
                self.facts_by_key_hash
                    .get(&identity)
                    .and_then(|index| self.facts.get(*index))
                    .and_then(|stored| self.candidate_for(stored, island, pattern, session))
            })
            .collect()
    }

    fn candidate_for(
        &self,
        stored: &StoredFactOperation,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> Option<FactCandidate> {
        if !pattern.matches(&stored.metadata.key) {
            return None;
        }
        let status = if stored.metadata.island != *island {
            CandidateStatus::CrossIsland
        } else if !self.authorizer.can_session_access_fact(
            session,
            &stored.metadata.key,
            FactAccess::Read,
        ) {
            CandidateStatus::Unauthorized
        } else if !self.authorizer.can_principal_access_fact(
            &stored.metadata.island,
            &stored.metadata.author,
            &stored.metadata.key,
            FactAccess::Write,
        ) {
            CandidateStatus::Unverified
        } else if self
            .fact_index
            .get(&(stored.metadata.island.clone(), stored.metadata.key.clone()))
            .is_some_and(|hashes| hashes.len() > 1)
        {
            CandidateStatus::Conflict
        } else {
            CandidateStatus::Verified
        };
        let classification = classify_fact_key(&stored.metadata.key);
        Some(FactCandidate::new(
            stored.metadata.island.clone(),
            stored.metadata.key.clone(),
            stored.metadata.author.clone(),
            stored.metadata.content_hash.clone(),
            classification.kind(),
            classification.epoch(),
            status,
        ))
    }
}
