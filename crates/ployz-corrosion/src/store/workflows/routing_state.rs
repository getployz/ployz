use crate::client::{CorrClient, SubscriptionStream};
use crate::store::tables::{instance_status, service_releases, service_revisions};
use corro_api_types::{RowId, SqliteValue, Statement, TypedQueryEvent, sqlite::ChangeType};
use futures_util::StreamExt;
use ployz_sdk::error::{Error, Result};
use ployz_sdk::model::{
    InstanceId, InstanceStatusRecord, RoutingState, ServiceReleaseRecord, ServiceRevisionRecord,
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
    releases: HashMap<(Namespace, String), ServiceReleaseRecord>,
    release_rows: HashMap<u64, (Namespace, String)>,
    instances: HashMap<InstanceId, InstanceStatusRecord>,
    instance_rows: HashMap<u64, InstanceId>,
}

impl LiveRoutingState {
    fn new() -> Self {
        Self {
            revisions: HashMap::new(),
            revision_rows: HashMap::new(),
            releases: HashMap::new(),
            release_rows: HashMap::new(),
            instances: HashMap::new(),
            instance_rows: HashMap::new(),
        }
    }

    fn to_routing_state(&self) -> RoutingState {
        RoutingState {
            revisions: self.revisions.values().cloned().collect(),
            releases: self.releases.values().cloned().collect(),
            instances: self.instances.values().cloned().collect(),
        }
    }
}

pub(crate) async fn load_routing_state(client: &CorrClient) -> Result<RoutingState> {
    let (revisions, releases, instances) = tokio::join!(
        service_revisions::load_all_service_revisions(client),
        service_releases::load_all_service_releases(client),
        instance_status::load_all_instance_status(client),
    );
    Ok(RoutingState {
        revisions: revisions?,
        releases: releases?,
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

fn routing_subscription_statements() -> [(&'static str, Statement); 3] {
    [
        ("service_revisions", service_revisions::all_statement()),
        ("service_releases", service_releases::all_statement()),
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
    let mut release_stream = client
        .subscribe(&service_releases::all_statement(), false, None)
        .await
        .map_err(|e| Error::operation("subscribe_routing_state", format!("releases: {e}")))?;
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
        &mut release_stream,
        &service_releases::parse_service_release,
        &|record: &ServiceReleaseRecord| (record.namespace.clone(), record.service.clone()),
        &mut state.releases,
        &mut state.release_rows,
        "releases",
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
        let debounce = tokio::time::sleep(DEBOUNCE_WINDOW);
        tokio::pin!(debounce);

        loop {
            tokio::select! {
                event = revision_stream.next() => {
                    match event {
                        Some(Ok(TypedQueryEvent::Columns(_) | TypedQueryEvent::EndOfQuery { .. })) => {}
                        Some(Ok(TypedQueryEvent::Error(err))) => {
                            warn!(?err, "routing revision subscription error");
                            return;
                        }
                        Some(Ok(TypedQueryEvent::Row(rowid, cells))) => {
                            if apply_routing_change(
                                ChangeType::Insert,
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
                            ).is_ok() {
                                dirty = true;
                            } else {
                                return;
                            }
                        }
                        Some(Ok(TypedQueryEvent::Change(change_type, rowid, cells, _))) => {
                            if apply_routing_change(
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
                            ).is_ok() {
                                dirty = true;
                            } else {
                                return;
                            }
                        }
                        Some(Err(err)) => {
                            warn!(?err, "routing revision stream error");
                            return;
                        }
                        None => return,
                    }
                }
                event = release_stream.next() => {
                    match event {
                        Some(Ok(TypedQueryEvent::Columns(_) | TypedQueryEvent::EndOfQuery { .. })) => {}
                        Some(Ok(TypedQueryEvent::Error(err))) => {
                            warn!(?err, "routing release subscription error");
                            return;
                        }
                        Some(Ok(TypedQueryEvent::Row(rowid, cells))) => {
                            if apply_routing_change(
                                ChangeType::Insert,
                                rowid,
                                &cells,
                                &service_releases::parse_service_release,
                                &|record: &ServiceReleaseRecord| {
                                    (record.namespace.clone(), record.service.clone())
                                },
                                &mut state.releases,
                                &mut state.release_rows,
                            ).is_ok() {
                                dirty = true;
                            } else {
                                return;
                            }
                        }
                        Some(Ok(TypedQueryEvent::Change(change_type, rowid, cells, _))) => {
                            if apply_routing_change(
                                change_type,
                                rowid,
                                &cells,
                                &service_releases::parse_service_release,
                                &|record: &ServiceReleaseRecord| {
                                    (record.namespace.clone(), record.service.clone())
                                },
                                &mut state.releases,
                                &mut state.release_rows,
                            ).is_ok() {
                                dirty = true;
                            } else {
                                return;
                            }
                        }
                        Some(Err(err)) => {
                            warn!(?err, "routing release stream error");
                            return;
                        }
                        None => return,
                    }
                }
                event = instance_stream.next() => {
                    match event {
                        Some(Ok(TypedQueryEvent::Columns(_) | TypedQueryEvent::EndOfQuery { .. })) => {}
                        Some(Ok(TypedQueryEvent::Error(err))) => {
                            warn!(?err, "routing instance subscription error");
                            return;
                        }
                        Some(Ok(TypedQueryEvent::Row(rowid, cells))) => {
                            if apply_routing_change(
                                ChangeType::Insert,
                                rowid,
                                &cells,
                                &instance_status::parse_instance_status,
                                &|record: &InstanceStatusRecord| record.instance_id.clone(),
                                &mut state.instances,
                                &mut state.instance_rows,
                            ).is_ok() {
                                dirty = true;
                            } else {
                                return;
                            }
                        }
                        Some(Ok(TypedQueryEvent::Change(change_type, rowid, cells, _))) => {
                            if apply_routing_change(
                                change_type,
                                rowid,
                                &cells,
                                &instance_status::parse_instance_status,
                                &|record: &InstanceStatusRecord| record.instance_id.clone(),
                                &mut state.instances,
                                &mut state.instance_rows,
                            ).is_ok() {
                                dirty = true;
                            } else {
                                return;
                            }
                        }
                        Some(Err(err)) => {
                            warn!(?err, "routing instance stream error");
                            return;
                        }
                        None => return,
                    }
                }
                _ = &mut debounce, if dirty => {
                    dirty = false;
                    debounce.as_mut().reset(tokio::time::Instant::now() + DEBOUNCE_WINDOW);
                    if tx.send(state.to_routing_state()).await.is_err() {
                        return;
                    }
                }
                _ = tx.closed() => return,
            }
        }
    });

    Ok((initial, rx))
}

#[cfg(test)]
mod tests {
    use super::apply_routing_change;
    use crate::store::tables::service_releases;
    use corro_api_types::{RowId, SqliteValue, sqlite::ChangeType};
    use ployz_sdk::model::{DeployId, ServiceRelease, ServiceReleaseRecord, ServiceRoutingPolicy};
    use ployz_sdk::spec::Namespace;
    use std::collections::HashMap;

    fn release_row(namespace: &str, service: &str) -> Vec<SqliteValue> {
        let record = ServiceReleaseRecord {
            namespace: Namespace(namespace.into()),
            service: service.into(),
            release: ServiceRelease {
                primary_revision_hash: String::from("rev-1"),
                referenced_revision_hashes: vec![String::from("rev-1")],
                routing: ServiceRoutingPolicy::Direct {
                    revision_hash: String::from("rev-1"),
                },
                slots: Vec::new(),
                updated_by_deploy_id: DeployId(String::from("dep-1")),
                updated_at: 1,
            },
        };
        vec![
            namespace.into(),
            service.into(),
            serde_json::to_string(&record)
                .expect("serialize release")
                .into(),
        ]
    }

    #[test]
    fn apply_routing_change_tracks_insert_and_delete() {
        let mut map = HashMap::new();
        let mut row_index = HashMap::new();

        apply_routing_change(
            ChangeType::Insert,
            RowId(1),
            &release_row("prod", "api"),
            &service_releases::parse_service_release,
            &|record: &ServiceReleaseRecord| (record.namespace.clone(), record.service.clone()),
            &mut map,
            &mut row_index,
        )
        .expect("insert should succeed");

        assert_eq!(map.len(), 1);

        apply_routing_change(
            ChangeType::Delete,
            RowId(1),
            &[],
            &service_releases::parse_service_release,
            &|record: &ServiceReleaseRecord| (record.namespace.clone(), record.service.clone()),
            &mut map,
            &mut row_index,
        )
        .expect("delete should succeed");

        assert!(map.is_empty());
    }
}
