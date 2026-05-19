use super::*;

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
