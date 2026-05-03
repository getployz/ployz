//! Snapshot + watch helpers for the per-node state view streams.
//!
//! Each node maintains a local `machine_state_view` and `instance_state_view`
//! stream that aggregates per-machine state via JetStream `sources`. With
//! `max_messages_per_subject = 1` on both the per-machine streams and the
//! view streams, the latest state for any key is always present from
//! sequence 1 onward — replaying the view stream gives a complete
//! snapshot. Live updates flow through the same consumer.
//!
//! Tombstone subjects (`state.<id>.machine_tombstone`,
//! `state.<id>.instance_tombstone.<ns>.<id>`) carry deletes. The consumer
//! interprets a tombstone arrival as a removal from the in-memory map.
use std::collections::HashMap;
use std::time::Duration;

use async_nats::jetstream::consumer::{AckPolicy, DeliverPolicy, push};
use futures_util::StreamExt;
use ployz_types::error::{Error, Result};
use tokio::sync::mpsc;
use tracing::warn;

use crate::NatsStore;

const STATE_VIEW_CHANNEL_CAPACITY: usize = 256;
const STATE_VIEW_ACK_WAIT: Duration = Duration::from_secs(30);
const STATE_VIEW_IDLE_HEARTBEAT: Duration = Duration::from_secs(5);
const STATE_VIEW_INACTIVE_THRESHOLD: Duration = Duration::from_secs(60);

/// Decoded action for one message off a state view.
pub(crate) enum StateMessage<T> {
    Upsert { key: String, value: T },
    Tombstone { key: String },
}

/// Outcome the consumer wants the watcher to report on the channel.
pub(crate) enum StateChange<T, E> {
    /// Steady-state event emit on the channel.
    Event(E),
    /// Decoded successfully but emit nothing (e.g. duplicate).
    Silent,
    #[allow(dead_code)]
    Phantom(std::marker::PhantomData<T>),
}

/// Subscribe to a state view stream. Returns:
/// - the snapshot (every key's latest value at subscribe time), and
/// - a channel of subsequent events (errors land on the same channel
///   wrapped in `Err`, so consumers can react to subscription failures).
///
/// `decode` parses one stream message into `StateMessage<T>`. The watcher
/// uses it for both snapshot and steady-state phases. `event_for` decides
/// what (if anything) to emit on the channel — it is called with the
/// running `last_seen` map and the just-decoded message; it must update
/// `last_seen` itself so duplicate-suppression behaves consistently.
pub(crate) async fn subscribe<T, E>(
    store: &NatsStore,
    view_stream: &str,
    filter_subjects: Vec<String>,
    decode: impl Fn(&str, &[u8]) -> Result<StateMessage<T>> + Send + Sync + 'static,
    event_for: impl Fn(&mut HashMap<String, T>, StateMessage<T>) -> StateChange<T, E>
    + Send
    + Sync
    + 'static,
    snapshot_seed: impl Fn(&T) -> String,
) -> Result<(Vec<T>, mpsc::Receiver<Result<E>>)>
where
    T: Clone + Send + 'static,
    E: Send + 'static,
{
    // The view stream is created lazily by the directory reconciler — it
    // only exists once at least one machine has registered. If we're the
    // first reader (e.g. orchestrator startup right after the founder's
    // first `upsert_self_machine`), the stream may not exist yet. Fall
    // through with an empty snapshot and a closed receiver; the caller
    // re-subscribes via the routing-event path once events appear. Any
    // other JetStream error (transport, auth, server outage) propagates
    // — silently swallowing them would violate the failure-audience
    // rule by serving an empty snapshot as if it were authoritative.
    let stream = match store.local_jetstream().get_stream(view_stream).await {
        Ok(stream) => stream,
        Err(error) => {
            // We can't pattern-match on the kind in async-nats 0.47
            // without exposing internal types. Distinguish "stream not
            // found" from other failures by inspecting the debug
            // formatting; "stream not found" / NATS code 10059 are both
            // expected during the lazy-create window.
            let message = format!("{error:?}");
            if message.contains("stream not found") || message.contains("10059") {
                let (_tx, rx) = mpsc::channel::<Result<E>>(1);
                return Ok((Vec::new(), rx));
            }
            return Err(Error::operation("nats_state_view_stream", message));
        }
    };
    let inbox = store.client().new_inbox();
    let mut config = push::Config {
        deliver_subject: inbox,
        deliver_policy: DeliverPolicy::All,
        ack_policy: AckPolicy::None,
        ack_wait: STATE_VIEW_ACK_WAIT,
        idle_heartbeat: STATE_VIEW_IDLE_HEARTBEAT,
        inactive_threshold: STATE_VIEW_INACTIVE_THRESHOLD,
        memory_storage: true,
        ..Default::default()
    };
    // Multiple filters cover the live + tombstone subject pattern for
    // namespace-scoped instance subscriptions; without both, deletes
    // would never reach a namespace subscriber and stale records would
    // linger in the snapshot.
    if !filter_subjects.is_empty() {
        config.filter_subjects = filter_subjects;
    }
    let mut consumer: async_nats::jetstream::consumer::PushConsumer = stream
        .create_consumer(config)
        .await
        .map_err(|error| Error::operation("nats_state_view_consumer", format!("{error:?}")))?;
    // Snapshot count is the consumer's `num_pending` after creation: the
    // number of messages that match the consumer's filter (if any) and
    // are available to deliver. Using `stream.last_sequence` would
    // overcount when a `filter_subject` is set — the loop would then
    // wait forever for messages that don't match the filter.
    let consumer_info = consumer
        .info()
        .await
        .map_err(|error| Error::operation("nats_state_view_consumer_info", format!("{error:?}")))?
        .clone();
    let snapshot_count = consumer_info.num_pending;
    let mut messages = consumer
        .messages()
        .await
        .map_err(|error| Error::operation("nats_state_view_messages", format!("{error:?}")))?;

    // Snapshot phase: drain `snapshot_count` filter-matching messages.
    let mut last_seen: HashMap<String, T> = HashMap::new();
    let mut delivered: u64 = 0;
    while delivered < snapshot_count {
        let next = messages.next().await;
        let Some(item) = next else {
            return Err(Error::operation(
                "nats_state_view_snapshot",
                "consumer stream ended before snapshot completed",
            ));
        };
        let message = item.map_err(|error| {
            Error::operation("nats_state_view_snapshot", format!("{error:?}"))
        })?;
        delivered = delivered.saturating_add(1);
        let decoded = decode(message.subject.as_str(), message.payload.as_ref())?;
        let _ = event_for(&mut last_seen, decoded);
    }
    let snapshot: Vec<T> = last_seen
        .values()
        .cloned()
        .collect::<Vec<T>>()
        .into_iter()
        .map(|v| {
            let _ = snapshot_seed(&v);
            v
        })
        .collect();

    // Steady-state phase: forward decoded events to the channel.
    let (tx, rx) = mpsc::channel::<Result<E>>(STATE_VIEW_CHANNEL_CAPACITY);
    let view_label = view_stream.to_string();
    tokio::spawn(async move {
        loop {
            let next = tokio::select! {
                _ = tx.closed() => break,
                next = messages.next() => next,
            };
            let Some(item) = next else {
                warn!(view = view_label.as_str(), "state view consumer stream ended");
                break;
            };
            let message = match item {
                Ok(message) => message,
                Err(error) => {
                    let error = Error::operation(
                        "nats_state_view_message",
                        format!("{error:?}"),
                    );
                    let _ = tx.send(Err(error)).await;
                    break;
                }
            };
            let decoded = match decode(message.subject.as_str(), message.payload.as_ref()) {
                Ok(decoded) => decoded,
                Err(error) => {
                    warn!(?error, view = view_label.as_str(), "state view decode failed");
                    let _ = tx.send(Err(error)).await;
                    break;
                }
            };
            match event_for(&mut last_seen, decoded) {
                StateChange::Event(event) => {
                    if tx.send(Ok(event)).await.is_err() {
                        break;
                    }
                }
                StateChange::Silent | StateChange::Phantom(_) => {}
            }
        }
    });
    Ok((snapshot, rx))
}
