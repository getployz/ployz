use async_nats::jetstream::kv;
use futures_util::StreamExt;
use ployz_error::{Error, Result, StoreError};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::warn;

use crate::store::kv_json;

const KV_WATCH_CHANNEL_CAPACITY: usize = 128;

pub(crate) async fn subscribe_all<T, E>(
    bucket: &kv::Store,
    snapshot: Vec<T>,
    observed_revision: u64,
    snapshot_key: impl Fn(&T) -> String + Send + 'static,
    decode_record: impl Fn(&str, &[u8]) -> Result<T> + Send + 'static,
    upsert_event: impl Fn(T) -> E + Send + 'static,
    remove_event: impl Fn(T) -> E + Send + 'static,
    watch_operation: &'static str,
    watch_failure_message: &'static str,
    decode_failure_message: &'static str,
) -> Result<(Vec<T>, mpsc::Receiver<Result<E>>)>
where
    T: Clone + PartialEq + Send + 'static,
    E: Send + 'static,
{
    let mut watch = bucket
        .watch_all_from_revision(kv_json::next_sequence(observed_revision))
        .await
        .map_err(|error| {
            Error::Store(StoreError::WatchFailed {
                watch: watch_operation,
                message: format!("{error:?}"),
            })
        })?;
    let mut live_records = snapshot
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
                    let error = Error::Store(StoreError::WatchFailed {
                        watch: watch_operation,
                        message: format!("{error:?}"),
                    });
                    warn!(?error, "{watch_failure_message}");
                    let _ = tx.send(Err(error)).await;
                    break;
                }
            };
            let Some(event) = (match project_entry(
                &mut live_records,
                entry.key.as_str(),
                entry.value.as_ref(),
                entry.operation,
                &decode_record,
                &upsert_event,
                &remove_event,
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

fn project_entry<T, E>(
    live_records: &mut HashMap<String, T>,
    key: &str,
    bytes: &[u8],
    operation: kv::Operation,
    decode_record: &impl Fn(&str, &[u8]) -> Result<T>,
    upsert_event: &impl Fn(T) -> E,
    remove_event: &impl Fn(T) -> E,
) -> Result<Option<E>>
where
    T: Clone + PartialEq,
{
    match operation {
        kv::Operation::Put => {
            let record = decode_record(key, bytes)?;
            if live_records.get(key) == Some(&record) {
                return Ok(None);
            }
            live_records.insert(key.to_string(), record.clone());
            Ok(Some(upsert_event(record)))
        }
        kv::Operation::Delete | kv::Operation::Purge => {
            Ok(live_records.remove(key).map(remove_event))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_error::Error;

    #[test]
    fn put_updates_live_records_and_emits_event() {
        let mut live_records = HashMap::new();

        let event = project_entry(
            &mut live_records,
            "key-a",
            b"value-a",
            kv::Operation::Put,
            &decode_string,
            &|value| format!("upsert:{value}"),
            &|value| format!("remove:{value}"),
        )
        .expect("put should decode");

        assert_eq!(event.as_deref(), Some("upsert:value-a"));
        assert_eq!(
            live_records.get("key-a").map(String::as_str),
            Some("value-a")
        );
    }

    #[test]
    fn put_deduplicates_identical_value() {
        let mut live_records = HashMap::from([(String::from("key-a"), String::from("value-a"))]);

        let event = project_entry(
            &mut live_records,
            "key-a",
            b"value-a",
            kv::Operation::Put,
            &decode_string,
            &|value| format!("upsert:{value}"),
            &|value| format!("remove:{value}"),
        )
        .expect("put should decode");

        assert!(event.is_none());
        assert_eq!(
            live_records.get("key-a").map(String::as_str),
            Some("value-a")
        );
    }

    #[test]
    fn delete_uses_live_record_and_missing_delete_is_noop() {
        let mut live_records = HashMap::from([(String::from("key-a"), String::from("value-a"))]);

        let event = project_entry(
            &mut live_records,
            "key-a",
            &[],
            kv::Operation::Delete,
            &decode_string,
            &|value| format!("upsert:{value}"),
            &|value| format!("remove:{value}"),
        )
        .expect("delete should not decode");

        assert_eq!(event.as_deref(), Some("remove:value-a"));
        assert!(!live_records.contains_key("key-a"));

        let missing = project_entry(
            &mut live_records,
            "key-a",
            &[],
            kv::Operation::Delete,
            &decode_string,
            &|value| format!("upsert:{value}"),
            &|value| format!("remove:{value}"),
        )
        .expect("missing delete should not fail");

        assert!(missing.is_none());
    }

    #[test]
    fn decode_failure_does_not_update_live_records() {
        let mut live_records = HashMap::new();

        let result = project_entry(
            &mut live_records,
            "key-a",
            b"bad",
            kv::Operation::Put,
            &|_, _| Err(Error::operation("decode", "bad record")),
            &|value: String| format!("upsert:{value}"),
            &|value| format!("remove:{value}"),
        );

        assert!(result.is_err());
        assert!(live_records.is_empty());
    }

    fn decode_string(_key: &str, bytes: &[u8]) -> Result<String> {
        String::from_utf8(bytes.to_vec())
            .map_err(|error| Error::operation("decode", error.to_string()))
    }
}
