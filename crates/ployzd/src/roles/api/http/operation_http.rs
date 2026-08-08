//! Coarse operation lookup and Corrosion-row watching.

use std::convert::Infallible;
use std::time::Duration;

use bytes::Bytes;
use futures_util::stream;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use hyper::{Response, StatusCode};
use ployz_core::corrosion::{
    CorrosionTable, OperationDocument, SqliteParameter, Statement, read_rows,
};
use ployz_core::ids::OperationRowId;
use ployz_core::{
    LensCollection, LensSnapshot, OperationLookupRefusal, OperationLookupReply,
    OperationWatchEvent, OperationWatchRefusal,
};
use tokio::sync::watch;

use super::server::{
    ApiService, HttpBody, corrosion_unavailable_response, json_response, sse_data, sse_keepalive,
    sse_response,
};
use crate::corrosion::{StoredRowLimit, collect_stored_rows};

const OPERATION_APPEAR_TIMEOUT: Duration = Duration::from_secs(15);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

pub(super) async fn handle_lookup(
    service: &ApiService,
    operation_id: OperationRowId,
) -> Response<HttpBody> {
    match lookup(service, &operation_id).await {
        Ok(Some(operation)) => super::mutations::typed_response(
            StatusCode::OK,
            &OperationLookupReply {
                operation_id,
                operation,
            },
        ),
        Ok(None) => super::mutations::typed_response(
            StatusCode::NOT_FOUND,
            &OperationLookupRefusal::NotFound { operation_id },
        ),
        Err(error) => {
            tracing::warn!(%error, "operation lookup failed");
            corrosion_unavailable_response()
        }
    }
}

pub(super) async fn handle_watch(
    service: &ApiService,
    operation_id: OperationRowId,
    shutdown: watch::Receiver<bool>,
) -> Response<HttpBody> {
    let Some(lenses) = service.lenses() else {
        return corrosion_unavailable_response();
    };
    let mut updates = lenses.watch(LensCollection::Operations).subscribe();
    let operation = match await_operation(service, &operation_id, &mut updates).await {
        Ok(Some(operation)) => operation,
        Ok(None) => {
            return watch_refusal(OperationWatchRefusal::NotFound { operation_id });
        }
        Err(error) => {
            tracing::warn!(%error, "operation watch lookup failed");
            return corrosion_unavailable_response();
        }
    };
    let initial = OperationLookupReply {
        operation_id: operation_id.clone(),
        operation,
    };
    sse_response(watch_body(operation_id, initial, updates, shutdown))
}

async fn await_operation(
    service: &ApiService,
    operation_id: &OperationRowId,
    updates: &mut watch::Receiver<Option<super::server::LensState>>,
) -> Result<Option<OperationDocument>, String> {
    if let Some(operation) = lookup(service, operation_id).await? {
        return Ok(Some(operation));
    }
    tokio::time::timeout(OPERATION_APPEAR_TIMEOUT, async {
        loop {
            if let Some(state) = updates.borrow_and_update().clone() {
                match state {
                    Ok(LensSnapshot::Operations { rows, .. }) => {
                        if let Some(row) = rows.into_iter().find(|row| &row.id == operation_id) {
                            return Ok(Some(row.document));
                        }
                    }
                    Ok(_) => return Err("operations lens returned another collection".to_owned()),
                    Err(refusal) => return Err(format!("operations lens failed: {refusal:?}")),
                }
            }
            if updates.changed().await.is_err() {
                return Err("operations lens stopped".to_owned());
            }
        }
    })
    .await
    .unwrap_or(Ok(None))
}

async fn lookup(
    service: &ApiService,
    operation_id: &OperationRowId,
) -> Result<Option<OperationDocument>, String> {
    let statement = Statement::with_params(
        format!(
            "SELECT id, document FROM {} WHERE id = ?",
            CorrosionTable::Operations.as_str()
        ),
        vec![SqliteParameter::Text(operation_id.as_str().to_owned())],
    );
    let mut stream = service
        .corrosion
        .query(&statement)
        .await
        .map_err(|error| error.to_string())?;
    let rows = collect_stored_rows(&mut stream, StoredRowLimit::new(2))
        .await
        .map_err(|error| error.to_string())?;
    let report = read_rows::<OperationDocument>(&service.cluster_id, rows);
    if !report.skipped.is_empty() || report.accepted.len() > 1 {
        return Err("operation query returned invalid rows".to_owned());
    }
    Ok(report.accepted.into_iter().next().map(|row| row.value))
}

struct WatchState {
    operation_id: OperationRowId,
    last: OperationLookupReply,
    pending_initial: bool,
    done: bool,
    updates: watch::Receiver<Option<super::server::LensState>>,
    shutdown: watch::Receiver<bool>,
    keepalive: tokio::time::Interval,
}

fn watch_body(
    operation_id: OperationRowId,
    initial: OperationLookupReply,
    updates: watch::Receiver<Option<super::server::LensState>>,
    shutdown: watch::Receiver<bool>,
) -> HttpBody {
    let stream = stream::unfold(
        WatchState {
            operation_id,
            last: initial,
            pending_initial: true,
            done: false,
            updates,
            shutdown,
            keepalive: tokio::time::interval(KEEPALIVE_INTERVAL),
        },
        |mut state| async move {
            loop {
                if state.done {
                    return None;
                }
                if state.pending_initial {
                    state.pending_initial = false;
                    let event = row_event(state.last.clone());
                    state.done = matches!(event, OperationWatchEvent::Terminal { .. });
                    return Some((
                        Ok::<_, Infallible>(Frame::data(encode_event(&event))),
                        state,
                    ));
                }
                tokio::select! {
                    biased;
                    changed = state.shutdown.changed() => {
                        if changed.is_err() || *state.shutdown.borrow() {
                            return None;
                        }
                    }
                    changed = state.updates.changed() => {
                        if changed.is_err() {
                            return None;
                        }
                        let Some(Ok(LensSnapshot::Operations { rows, .. })) =
                            state.updates.borrow_and_update().clone()
                        else {
                            continue;
                        };
                        let Some(row) = rows.into_iter().find(|row| row.id == state.operation_id)
                        else {
                            continue;
                        };
                        let next = OperationLookupReply {
                            operation_id: row.id,
                            operation: row.document,
                        };
                        if next == state.last {
                            continue;
                        }
                        state.last = next.clone();
                        let event = row_event(next);
                        let terminal = matches!(event, OperationWatchEvent::Terminal { .. });
                        let frame = Frame::data(encode_event(&event));
                        if terminal {
                            state.done = true;
                            return Some((Ok(frame), state));
                        }
                        return Some((Ok(frame), state));
                    }
                    _ = state.keepalive.tick() => {
                        return Some((Ok(Frame::data(sse_keepalive())), state));
                    }
                }
            }
        },
    );
    StreamBody::new(stream).boxed()
}

fn row_event(operation: OperationLookupReply) -> OperationWatchEvent {
    if operation.operation.is_terminal() {
        OperationWatchEvent::Terminal { operation }
    } else {
        OperationWatchEvent::State { operation }
    }
}

fn encode_event(event: &OperationWatchEvent) -> Bytes {
    let json = serde_json::to_vec(event).expect("operation watch contracts serialize");
    let data = sse_data(&json);
    let mut frame = Vec::with_capacity(event.event_name().len() + data.len() + 8);
    frame.extend_from_slice(b"event: ");
    frame.extend_from_slice(event.event_name().as_bytes());
    frame.push(b'\n');
    frame.extend_from_slice(&data);
    Bytes::from(frame)
}

fn watch_refusal(refusal: OperationWatchRefusal) -> Response<HttpBody> {
    match serde_json::to_vec(&refusal) {
        Ok(body) => json_response(StatusCode::NOT_FOUND, body),
        Err(_) => corrosion_unavailable_response(),
    }
}
