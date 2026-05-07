use async_nats::jetstream::kv;
use futures_util::StreamExt;
use ployz_types::error::{Error, Result};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::warn;

use crate::store::kv_json;

const KV_WATCH_CHANNEL_CAPACITY: usize = 128;

pub(crate) async fn subscribe_all_with_snapshot_revisions<T, E>(
    bucket: &kv::Store,
    snapshot: Vec<T>,
    snapshot_revisions: HashMap<String, u64>,
    snapshot_boundary: u64,
    snapshot_key: impl Fn(&T) -> String + Send + 'static,
    watch_operation: &'static str,
    watch_failure_message: &'static str,
    decode_failure_message: &'static str,
    event_from_entry: impl Fn(&mut HashMap<String, T>, &str, &[u8], kv::Operation) -> Result<Option<E>>
    + Send
    + 'static,
) -> Result<(Vec<T>, mpsc::Receiver<Result<E>>)>
where
    T: Clone + Send + 'static,
    E: Send + 'static,
{
    let mut watch = bucket
        .watch_all_from_revision(kv_json::next_sequence(snapshot_boundary))
        .await
        .map_err(|error| Error::operation(watch_operation, format!("{error:?}")))?;
    let mut last_seen = snapshot
        .iter()
        .map(|record| (snapshot_key(record), record.clone()))
        .collect::<HashMap<_, _>>();
    let (tx, rx) = mpsc::channel(KV_WATCH_CHANNEL_CAPACITY);
    tokio::spawn(async move {
        loop {
            let next = tokio::select! {
                _ = tx.closed() => break,
                next = watch.next() => next,
            };
            let Some(next) = next else {
                break;
            };
            let entry = match next {
                Ok(entry) => entry,
                Err(error) => {
                    let error = Error::operation(watch_operation, format!("{error:?}"));
                    warn!(?error, "{watch_failure_message}");
                    let _ = tx.send(Err(error)).await;
                    break;
                }
            };
            if snapshot_revisions
                .get(entry.key.as_str())
                .is_some_and(|revision| entry.revision <= *revision)
            {
                continue;
            }
            let Some(event) = (match event_from_entry(
                &mut last_seen,
                entry.key.as_str(),
                entry.value.as_ref(),
                entry.operation,
            ) {
                Ok(event) => event,
                Err(error) => {
                    warn!(?error, key = %entry.key, "{decode_failure_message}");
                    let _ = tx.send(Err(error)).await;
                    break;
                }
            }) else {
                continue;
            };
            if tx.send(Ok(event)).await.is_err() {
                break;
            }
        }
    });
    Ok((snapshot, rx))
}
