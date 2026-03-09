use crate::client::{CorrClient, SubscriptionStream};
use crate::store::tables::{instance_status, service_heads, service_revisions, service_slots};
use corro_api_types::{RowId, SqliteValue, Statement, TypedQueryEvent, sqlite::ChangeType};
use futures_util::StreamExt;
use ployz_sdk::error::{Error, Result};
use ployz_sdk::model::{
    InstanceId, InstanceStatusRecord, RoutingState, ServiceHeadRecord, ServiceRevisionRecord,
    ServiceSlotRecord, SlotId,
};
use ployz_sdk::spec::Namespace;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::warn;

const DEBOUNCE_WINDOW: Duration = Duration::from_millis(100);

struct LiveRoutingState {
    revisions: HashMap<(Namespace, String, String), ServiceRevisionRecord>,
    revision_rows: HashMap<u64, (Namespace, String, String)>,
    heads: HashMap<(Namespace, String), ServiceHeadRecord>,
    head_rows: HashMap<u64, (Namespace, String)>,
    slots: HashMap<(Namespace, String, SlotId), ServiceSlotRecord>,
    slot_rows: HashMap<u64, (Namespace, String, SlotId)>,
    instances: HashMap<InstanceId, InstanceStatusRecord>,
    instance_rows: HashMap<u64, InstanceId>,
}

impl LiveRoutingState {
    fn new() -> Self {
        Self {
            revisions: HashMap::new(),
            revision_rows: HashMap::new(),
            heads: HashMap::new(),
            head_rows: HashMap::new(),
            slots: HashMap::new(),
            slot_rows: HashMap::new(),
            instances: HashMap::new(),
            instance_rows: HashMap::new(),
        }
    }

    fn to_routing_state(&self) -> RoutingState {
        RoutingState {
            revisions: self.revisions.values().cloned().collect(),
            heads: self.heads.values().cloned().collect(),
            slots: self.slots.values().cloned().collect(),
            instances: self.instances.values().cloned().collect(),
        }
    }
}

pub(crate) async fn load_routing_state(client: &CorrClient) -> Result<RoutingState> {
    let (revisions, heads, slots, instances) = tokio::join!(
        service_revisions::load_all_service_revisions(client),
        service_heads::load_all_service_heads(client),
        service_slots::load_all_service_slots(client),
        instance_status::load_all_instance_status(client),
    );
    Ok(RoutingState {
        revisions: revisions?,
        heads: heads?,
        slots: slots?,
        instances: instances?,
    })
}

pub(crate) async fn subscribe_routing_invalidations(
    client: &CorrClient,
) -> Result<mpsc::Receiver<()>> {
    let (tx, rx) = mpsc::channel(64);
    for (label, statement) in routing_subscription_statements() {
        tokio::spawn(run_routing_invalidator(
            label,
            client.clone(),
            statement,
            tx.clone(),
        ));
    }
    Ok(rx)
}

fn routing_subscription_statements() -> [(&'static str, Statement); 4] {
    [
        ("service_revisions", service_revisions::all_statement()),
        ("service_heads", service_heads::all_statement()),
        ("service_slots", service_slots::all_statement()),
        ("instance_status", instance_status::all_statement()),
    ]
}

async fn run_routing_invalidator(
    label: &'static str,
    client: CorrClient,
    statement: Statement,
    refresh_tx: mpsc::Sender<()>,
) {
    let mut stream = match client.subscribe(&statement, false, None).await {
        Ok(stream) => stream,
        Err(err) => {
            warn!(%label, ?err, "failed to subscribe routing invalidator");
            return;
        }
    };

    while let Some(event) = stream.next().await {
        match event {
            Ok(TypedQueryEvent::Columns(_) | TypedQueryEvent::EndOfQuery { .. }) => {}
            Ok(TypedQueryEvent::Row(_, _) | TypedQueryEvent::Change(_, _, _, _)) => {
                if refresh_tx.send(()).await.is_err() {
                    return;
                }
            }
            Ok(TypedQueryEvent::Error(err)) => {
                warn!(%label, ?err, "routing subscription error");
            }
            Err(err) => {
                warn!(%label, ?err, "routing invalidator stream error");
            }
        }
    }
}

async fn collect_initial_rows<T, K>(
    stream: &mut SubscriptionStream<Vec<SqliteValue>>,
    parse: &impl Fn(&[SqliteValue]) -> Result<T>,
    key: &impl Fn(&T) -> K,
    map: &mut HashMap<K, T>,
    row_index: &mut HashMap<u64, K>,
    label: &'static str,
) -> Result<()>
where
    K: Eq + std::hash::Hash + Clone,
{
    loop {
        let event = stream
            .next()
            .await
            .ok_or_else(|| {
                Error::operation(
                    "subscribe_routing_state",
                    format!("{label}: stream ended during initial snapshot"),
                )
            })?
            .map_err(|e| Error::operation("subscribe_routing_state", format!("{label}: {e}")))?;

        match event {
            TypedQueryEvent::Columns(_) => {}
            TypedQueryEvent::EndOfQuery { .. } => return Ok(()),
            TypedQueryEvent::Error(e) => {
                return Err(Error::operation(
                    "subscribe_routing_state",
                    format!("{label}: {e}"),
                ));
            }
            TypedQueryEvent::Row(rowid, cells) => {
                let record = parse(&cells)?;
                let record_key = key(&record);
                row_index.insert(rowid.0, record_key.clone());
                map.insert(record_key, record);
            }
            TypedQueryEvent::Change(change_type, rowid, cells, _) => {
                apply_routing_change(change_type, rowid, &cells, parse, key, map, row_index)?;
            }
        }
    }
}

fn apply_routing_change<T, K>(
    change_type: ChangeType,
    rowid: RowId,
    cells: &[SqliteValue],
    parse: &impl Fn(&[SqliteValue]) -> Result<T>,
    key: &impl Fn(&T) -> K,
    map: &mut HashMap<K, T>,
    row_index: &mut HashMap<u64, K>,
) -> Result<()>
where
    K: Eq + std::hash::Hash + Clone,
{
    match change_type {
        ChangeType::Insert | ChangeType::Update => {
            let record = parse(cells)?;
            let record_key = key(&record);
            row_index.insert(rowid.0, record_key.clone());
            map.insert(record_key, record);
        }
        ChangeType::Delete => {
            if let Some(record_key) = row_index.remove(&rowid.0) {
                map.remove(&record_key);
            }
        }
    }
    Ok(())
}

pub(crate) async fn subscribe_routing_state_inner(
    client: &CorrClient,
) -> Result<(RoutingState, mpsc::Receiver<RoutingState>)> {
    let mut state = LiveRoutingState::new();

    let mut revision_stream = client
        .subscribe(&service_revisions::all_statement(), false, None)
        .await
        .map_err(|e| Error::operation("subscribe_routing_state", format!("revisions: {e}")))?;
    let mut head_stream = client
        .subscribe(&service_heads::all_statement(), false, None)
        .await
        .map_err(|e| Error::operation("subscribe_routing_state", format!("heads: {e}")))?;
    let mut slot_stream = client
        .subscribe(&service_slots::all_statement(), false, None)
        .await
        .map_err(|e| Error::operation("subscribe_routing_state", format!("slots: {e}")))?;
    let mut instance_stream = client
        .subscribe(&instance_status::all_statement(), false, None)
        .await
        .map_err(|e| Error::operation("subscribe_routing_state", format!("instances: {e}")))?;

    collect_initial_rows(
        &mut revision_stream,
        &service_revisions::parse_service_revision,
        &|record: &ServiceRevisionRecord| {
            (
                record.namespace.clone(),
                record.service.clone(),
                record.revision_hash.clone(),
            )
        },
        &mut state.revisions,
        &mut state.revision_rows,
        "revisions",
    )
    .await?;

    collect_initial_rows(
        &mut head_stream,
        &service_heads::parse_service_head,
        &|record: &ServiceHeadRecord| (record.namespace.clone(), record.service.clone()),
        &mut state.heads,
        &mut state.head_rows,
        "heads",
    )
    .await?;

    collect_initial_rows(
        &mut slot_stream,
        &service_slots::parse_service_slot,
        &|record: &ServiceSlotRecord| {
            (
                record.namespace.clone(),
                record.service.clone(),
                record.slot_id.clone(),
            )
        },
        &mut state.slots,
        &mut state.slot_rows,
        "slots",
    )
    .await?;

    collect_initial_rows(
        &mut instance_stream,
        &instance_status::parse_instance_status,
        &|record: &InstanceStatusRecord| record.instance_id.clone(),
        &mut state.instances,
        &mut state.instance_rows,
        "instances",
    )
    .await?;

    let initial = state.to_routing_state();
    let (tx, rx) = mpsc::channel(16);

    tokio::spawn(async move {
        let mut dirty = false;
        let mut debounce = std::pin::pin!(tokio::time::sleep(DEBOUNCE_WINDOW));

        loop {
            tokio::select! {
                event = revision_stream.next() => {
                    let Some(event) = event else { return; };
                    match event {
                        Ok(TypedQueryEvent::Change(change_type, rowid, cells, _)) => {
                            let _ = apply_routing_change(
                                change_type,
                                rowid,
                                &cells,
                                &service_revisions::parse_service_revision,
                                &|record: &ServiceRevisionRecord| {
                                    (
                                        record.namespace.clone(),
                                        record.service.clone(),
                                        record.revision_hash.clone(),
                                    )
                                },
                                &mut state.revisions,
                                &mut state.revision_rows,
                            );
                            dirty = true;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!(?e, "revision subscription error");
                        }
                    }
                }
                event = head_stream.next() => {
                    let Some(event) = event else { return; };
                    match event {
                        Ok(TypedQueryEvent::Change(change_type, rowid, cells, _)) => {
                            let _ = apply_routing_change(
                                change_type,
                                rowid,
                                &cells,
                                &service_heads::parse_service_head,
                                &|record: &ServiceHeadRecord| {
                                    (record.namespace.clone(), record.service.clone())
                                },
                                &mut state.heads,
                                &mut state.head_rows,
                            );
                            dirty = true;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!(?e, "head subscription error");
                        }
                    }
                }
                event = slot_stream.next() => {
                    let Some(event) = event else { return; };
                    match event {
                        Ok(TypedQueryEvent::Change(change_type, rowid, cells, _)) => {
                            let _ = apply_routing_change(
                                change_type,
                                rowid,
                                &cells,
                                &service_slots::parse_service_slot,
                                &|record: &ServiceSlotRecord| {
                                    (
                                        record.namespace.clone(),
                                        record.service.clone(),
                                        record.slot_id.clone(),
                                    )
                                },
                                &mut state.slots,
                                &mut state.slot_rows,
                            );
                            dirty = true;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!(?e, "slot subscription error");
                        }
                    }
                }
                event = instance_stream.next() => {
                    let Some(event) = event else { return; };
                    match event {
                        Ok(TypedQueryEvent::Change(change_type, rowid, cells, _)) => {
                            let _ = apply_routing_change(
                                change_type,
                                rowid,
                                &cells,
                                &instance_status::parse_instance_status,
                                &|record: &InstanceStatusRecord| record.instance_id.clone(),
                                &mut state.instances,
                                &mut state.instance_rows,
                            );
                            dirty = true;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!(?e, "instance subscription error");
                        }
                    }
                }
                _ = &mut debounce, if dirty => {
                    let snapshot = state.to_routing_state();
                    if tx.send(snapshot).await.is_err() {
                        return;
                    }
                    dirty = false;
                    debounce.as_mut().reset(tokio::time::Instant::now() + DEBOUNCE_WINDOW);
                }
                _ = tx.closed() => return,
            }

            if dirty {
                debounce
                    .as_mut()
                    .reset(tokio::time::Instant::now() + DEBOUNCE_WINDOW);
            }
        }
    });

    Ok((initial, rx))
}

#[cfg(test)]
mod tests {
    use super::apply_routing_change;
    use corro_api_types::{RowId, SqliteValue, sqlite::ChangeType};
    use ployz_sdk::error::{Error, Result};
    use std::collections::HashMap;

    fn parse_value(row: &[SqliteValue]) -> Result<u64> {
        let [value] = row else {
            return Err(Error::operation(
                "parse_value",
                format!("expected 1 column, got {}", row.len()),
            ));
        };
        let Some(number) = value.as_integer() else {
            return Err(Error::operation("parse_value", "expected integer"));
        };
        Ok(*number as u64)
    }

    #[test]
    fn apply_routing_change_tracks_insert_and_delete() {
        let mut map = HashMap::new();
        let mut row_index = HashMap::new();

        apply_routing_change(
            ChangeType::Insert,
            RowId(7),
            &[7_i64.into()],
            &parse_value,
            &|value: &u64| *value,
            &mut map,
            &mut row_index,
        )
        .expect("insert should succeed");

        assert_eq!(map.get(&7), Some(&7));
        assert_eq!(row_index.get(&7), Some(&7));

        apply_routing_change(
            ChangeType::Delete,
            RowId(7),
            &[],
            &parse_value,
            &|value: &u64| *value,
            &mut map,
            &mut row_index,
        )
        .expect("delete should succeed");

        assert!(map.is_empty());
        assert!(row_index.is_empty());
    }
}
