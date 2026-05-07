use async_nats::jetstream::kv;
use futures_util::TryStreamExt;
use ployz_types::error::{Error, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug, Clone)]
pub struct JsonEntry<T> {
    pub key: String,
    pub revision: u64,
    pub value: T,
}

pub async fn get_bucket(
    js: &async_nats::jetstream::Context,
    bucket: &str,
    operation: &'static str,
) -> Result<kv::Store> {
    js.get_key_value(bucket)
        .await
        .map_err(|error| Error::operation(operation, format!("{error:?}")))
}

pub async fn latest_sequence(store: &kv::Store, operation: &'static str) -> Result<u64> {
    let mut stream = store.stream.clone();
    stream
        .info()
        .await
        .map(|info| info.state.last_sequence)
        .map_err(|error| Error::operation(operation, format!("{error:?}")))
}

#[must_use]
pub fn next_sequence(sequence: u64) -> u64 {
    sequence.saturating_add(1)
}

pub async fn list_json_entries<T>(
    store: &kv::Store,
    decode_operation: &'static str,
    get_operation: &'static str,
) -> Result<Vec<JsonEntry<T>>>
where
    T: DeserializeOwned,
{
    let keys = store
        .keys()
        .await
        .map_err(|error| Error::operation(get_operation, format!("{error:?}")))?
        .try_collect::<Vec<String>>()
        .await
        .map_err(|error| Error::operation(get_operation, format!("{error:?}")))?;
    let mut records = Vec::with_capacity(keys.len());
    for key in keys {
        let Some(entry) = store
            .entry(key)
            .await
            .map_err(|error| Error::operation(get_operation, format!("{error:?}")))?
        else {
            continue;
        };
        if entry.operation != kv::Operation::Put {
            continue;
        }
        records.push(JsonEntry {
            key: entry.key,
            revision: entry.revision,
            value: decode_json(decode_operation, entry.value.as_ref())?,
        });
    }
    Ok(records)
}

pub async fn put_json<T>(
    store: &kv::Store,
    key: &str,
    value: &T,
    encode_operation: &'static str,
    put_operation: &'static str,
) -> Result<()>
where
    T: Serialize,
{
    let payload = serde_json::to_vec(value)
        .map_err(|error| Error::operation(encode_operation, error.to_string()))?;
    store
        .put(key, payload.into())
        .await
        .map(|_| ())
        .map_err(|error| Error::operation(put_operation, format!("{error:?}")))
}

pub async fn delete(store: &kv::Store, key: &str, delete_operation: &'static str) -> Result<()> {
    store
        .delete(key)
        .await
        .map_err(|error| Error::operation(delete_operation, format!("{error:?}")))
}

pub fn decode_json<T>(operation: &'static str, bytes: &[u8]) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_slice(bytes).map_err(|error| Error::operation(operation, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::next_sequence;

    #[test]
    fn next_sequence_saturates_at_u64_max() {
        assert_eq!(next_sequence(0), 1);
        assert_eq!(next_sequence(u64::MAX), u64::MAX);
    }
}
