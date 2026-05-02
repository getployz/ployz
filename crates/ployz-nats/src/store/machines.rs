use std::collections::HashMap;

use async_nats::HeaderMap;
use async_nats::jetstream::context::PublishErrorKind;
use async_nats::jetstream::message::PublishMessage;
use ployz_store_api::{MachineRegistry, MachineSubscription};
use ployz_types::error::{Error, Result};
use ployz_types::model::{MachineEvent, MachineId, MachineMembership, RoutingEvent};

use crate::NatsStore;
use crate::buckets::{
    delete_machine_state_assets, ensure_machine_state_assets, local_read_stream_name,
    reconcile_view_streams,
};
use crate::store::kv_json;
use crate::store::state_directory;
use crate::store::state_view::{self, StateChange, StateMessage};
use crate::subjects::{
    MACHINE_STATE_VIEW_STREAM, machine_state_stream, machine_state_subject,
    machine_tombstone_subject,
};

impl MachineRegistry for NatsStore {
    /// List all machines from the local view stream. On `StorageCandidate`
    /// the view lives on hub; on `Mirror` it's mirrored locally so this
    /// is a local read.
    async fn list_machines(&self) -> Result<Vec<MachineMembership>> {
        let (snapshot, _events) = self.subscribe_machines_internal().await?;
        Ok(snapshot)
    }

    /// Publish the local node's self-record to its per-machine state
    /// stream. The first call also ensures the per-machine streams exist
    /// on hub and registers the machine in `machine_directory`.
    /// Continues to publish a `RoutingEvent::Machine*` for cross-node
    /// fan-out signal — consumers that read from `subscribe_machines`
    /// also use these events on the routing batch path.
    async fn upsert_self_machine(&self, record: &MachineMembership) -> Result<()> {
        ensure_machine_state_assets(self, &record.id).await?;
        state_directory::register(self, &record.id).await?;
        // Synchronously reconcile the local view streams so the next
        // reader (e.g. the orchestrator's first `subscribe_machines`
        // right after this returns) sees a view stream sourcing from
        // this machine's per-machine streams. The background reconciler
        // would converge on the same state via the directory event,
        // but the writer-side eager call closes a startup race.
        let directory = state_directory::list(self).await?;
        let machine_ids: Vec<MachineId> = directory
            .into_iter()
            .map(|entry| entry.machine_id)
            .collect();
        reconcile_view_streams(self, &machine_ids).await?;
        let old = read_machine_state(self, &record.id).await?;
        publish_machine_state(self, record).await?;
        let event = match old {
            Some(old) if old == *record => return Ok(()),
            Some(old) => RoutingEvent::MachineUpdated {
                old,
                new: record.clone(),
            },
            None => RoutingEvent::MachineAdded(record.clone()),
        };
        self.publish_routing_batch(
            format!("machine:{}", record.id.0),
            "machine.upsert",
            &[event],
        )
        .await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        let Some(old) = read_machine_state(self, id).await? else {
            return Ok(());
        };
        publish_machine_tombstone(self, id).await?;
        self.publish_routing_batch(
            format!("machine:delete:{}", id.0),
            "machine.delete",
            &[RoutingEvent::MachineRemoved(old)],
        )
        .await?;
        state_directory::deregister(self, id).await?;
        delete_machine_state_assets(self, id).await
    }

    async fn subscribe_machines(&self) -> Result<MachineSubscription> {
        self.subscribe_machines_internal().await
    }
}

impl NatsStore {
    async fn subscribe_machines_internal(&self) -> Result<MachineSubscription> {
        state_view::subscribe(
            self,
            &local_read_stream_name(self, MACHINE_STATE_VIEW_STREAM),
            None,
            decode_machine_message,
            machine_event_for,
            |record: &MachineMembership| record.id.0.clone(),
        )
        .await
    }
}

/// Read the current self-state for `machine_id` from the local view
/// stream. Returns `None` if the view doesn't carry a record for this
/// machine yet (e.g. fresh cluster, source replication still catching up).
async fn read_machine_state(
    store: &NatsStore,
    machine_id: &MachineId,
) -> Result<Option<MachineMembership>> {
    let stream_name = local_read_stream_name(store, MACHINE_STATE_VIEW_STREAM);
    let stream = match store.local_jetstream().get_stream(&stream_name).await {
        Ok(stream) => stream,
        Err(_) => return Ok(None),
    };
    let subject = machine_state_subject(machine_id);
    match stream.direct_get_last_for_subject(&subject).await {
        Ok(message) => Ok(Some(kv_json::decode_json::<MachineMembership>(
            "nats_machine_decode",
            message.payload.as_ref(),
        )?)),
        Err(_) => Ok(None),
    }
}

async fn publish_machine_state(store: &NatsStore, record: &MachineMembership) -> Result<()> {
    let payload = serde_json::to_vec(record)
        .map_err(|error| Error::operation("nats_machine_encode", error.to_string()))?;
    let publish = PublishMessage::build()
        .payload(payload.into())
        .expected_stream(machine_state_stream(&record.id))
        .message_id(format!("machine_state:{}", record.id.0));
    let ack = store
        .hub_jetstream()
        .send_publish(machine_state_subject(&record.id), publish)
        .await
        .map_err(|error| Error::operation("nats_machine_publish", format!("{error:?}")))?;
    match ack.await {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == PublishErrorKind::WrongLastSequence => {
            // Idempotent dedup window hit — same record-id within the
            // duplicate window. Treat as success because the new state is
            // already (or about to be) on the stream.
            Ok(())
        }
        Err(error) => Err(Error::operation(
            "nats_machine_ack",
            format!("{error:?}"),
        )),
    }
}

async fn publish_machine_tombstone(store: &NatsStore, machine_id: &MachineId) -> Result<()> {
    let publish = PublishMessage::build()
        .payload(Vec::new().into())
        .expected_stream(machine_state_stream(machine_id))
        .headers(HeaderMap::new())
        .message_id(format!("machine_tombstone:{}", machine_id.0));
    let ack = store
        .hub_jetstream()
        .send_publish(machine_tombstone_subject(machine_id), publish)
        .await
        .map_err(|error| {
            Error::operation("nats_machine_tombstone_publish", format!("{error:?}"))
        })?;
    match ack.await {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == PublishErrorKind::WrongLastSequence => Ok(()),
        Err(error) => Err(Error::operation(
            "nats_machine_tombstone_ack",
            format!("{error:?}"),
        )),
    }
}

fn decode_machine_message(subject: &str, payload: &[u8]) -> Result<StateMessage<MachineMembership>> {
    let key = subject_to_key(subject)?;
    if subject_is_tombstone(subject) {
        return Ok(StateMessage::Tombstone { key });
    }
    let machine = kv_json::decode_json::<MachineMembership>("nats_machine_decode", payload)?;
    Ok(StateMessage::Upsert {
        key,
        value: machine,
    })
}

/// Subject layout: `state.<machine_id>.machine` /
/// `state.<machine_id>.machine_tombstone`. We key the in-memory map by
/// `machine_id` so a put on `state` and a tombstone on the tombstone
/// subject act on the same logical row.
fn subject_to_key(subject: &str) -> Result<String> {
    let mut parts = subject.split('.');
    let prefix = parts.next();
    let token = parts.next();
    match (prefix, token) {
        (Some("state"), Some(token)) => Ok(token.to_string()),
        _ => Err(Error::operation(
            "nats_machine_state_subject",
            format!("unexpected machine state subject: {subject}"),
        )),
    }
}

fn subject_is_tombstone(subject: &str) -> bool {
    subject.ends_with(".machine_tombstone")
}

fn machine_event_for(
    last_seen: &mut HashMap<String, MachineMembership>,
    decoded: StateMessage<MachineMembership>,
) -> StateChange<MachineMembership, MachineEvent> {
    match decoded {
        StateMessage::Upsert { key, value } => {
            let event = match last_seen.get(&key) {
                Some(existing) if existing == &value => return StateChange::Silent,
                Some(_) => MachineEvent::Updated(value.clone()),
                None => MachineEvent::Added(value.clone()),
            };
            last_seen.insert(key, value);
            StateChange::Event(event)
        }
        StateMessage::Tombstone { key } => match last_seen.remove(&key) {
            Some(removed) => StateChange::Event(MachineEvent::Removed(removed)),
            None => StateChange::Silent,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{
        MachineLifecycle, MachineRole, MachineTopology, OverlayIp, PublicKey,
    };

    fn test_machine(id: &str) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.to_string()),
            public_key: PublicKey([0; 32]),
            overlay_ip: OverlayIp("fd00::1".parse().expect("valid overlay")),
            topology: MachineTopology::local(),
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: MachineLifecycle::Active,
            role: MachineRole::StorageCandidate,
            created_at: 1,
            updated_at: 1,
            labels: Default::default(),
        }
    }

    #[test]
    fn subject_decode_extracts_machine_id_from_state_subject() {
        let key = subject_to_key("state.machine-a.machine").expect("decode");
        assert_eq!(key, "machine-a");
    }

    #[test]
    fn subject_decode_extracts_machine_id_from_tombstone_subject() {
        let key = subject_to_key("state.machine-a.machine_tombstone").expect("decode");
        assert_eq!(key, "machine-a");
    }

    #[test]
    fn subject_decode_rejects_unrelated_subject() {
        assert!(subject_to_key("foo.bar").is_err());
        assert!(subject_to_key("state").is_err());
    }

    #[test]
    fn subject_is_tombstone_distinguishes_tombstone_from_live() {
        assert!(subject_is_tombstone("state.m1.machine_tombstone"));
        assert!(!subject_is_tombstone("state.m1.machine"));
    }

    #[test]
    fn upsert_decode_yields_upsert_state_message() {
        let machine = test_machine("m1");
        let bytes = serde_json::to_vec(&machine).expect("encode");
        let message = decode_machine_message("state.m1.machine", &bytes).expect("decode");
        match message {
            StateMessage::Upsert { key, value } => {
                assert_eq!(key, "m1");
                assert_eq!(value, machine);
            }
            _ => panic!("expected Upsert"),
        }
    }

    #[test]
    fn tombstone_decode_yields_tombstone_state_message() {
        let message =
            decode_machine_message("state.m1.machine_tombstone", &[]).expect("decode");
        match message {
            StateMessage::Tombstone { key } => assert_eq!(key, "m1"),
            _ => panic!("expected Tombstone"),
        }
    }

    #[test]
    fn machine_event_for_emits_added_on_first_observation() {
        let mut last_seen = HashMap::new();
        let machine = test_machine("m1");
        let change = machine_event_for(
            &mut last_seen,
            StateMessage::Upsert {
                key: "m1".into(),
                value: machine.clone(),
            },
        );
        match change {
            StateChange::Event(MachineEvent::Added(record)) => assert_eq!(record, machine),
            _ => panic!("expected Added event"),
        }
        assert_eq!(last_seen.get("m1"), Some(&machine));
    }

    #[test]
    fn machine_event_for_skips_identical_value() {
        let mut last_seen = HashMap::new();
        let machine = test_machine("m1");
        last_seen.insert("m1".into(), machine.clone());
        let change = machine_event_for(
            &mut last_seen,
            StateMessage::Upsert {
                key: "m1".into(),
                value: machine,
            },
        );
        assert!(matches!(change, StateChange::Silent));
    }

    #[test]
    fn machine_event_for_emits_updated_on_change() {
        let mut last_seen = HashMap::new();
        let mut original = test_machine("m1");
        original.lifecycle = MachineLifecycle::Active;
        last_seen.insert("m1".into(), original);
        let mut updated = test_machine("m1");
        updated.lifecycle = MachineLifecycle::Draining;
        let change = machine_event_for(
            &mut last_seen,
            StateMessage::Upsert {
                key: "m1".into(),
                value: updated.clone(),
            },
        );
        match change {
            StateChange::Event(MachineEvent::Updated(record)) => {
                assert_eq!(record.lifecycle, MachineLifecycle::Draining);
                assert_eq!(record, updated);
            }
            _ => panic!("expected Updated event"),
        }
    }

    #[test]
    fn machine_event_for_emits_removed_on_tombstone() {
        let mut last_seen = HashMap::new();
        let machine = test_machine("m1");
        last_seen.insert("m1".into(), machine.clone());
        let change = machine_event_for(
            &mut last_seen,
            StateMessage::Tombstone { key: "m1".into() },
        );
        match change {
            StateChange::Event(MachineEvent::Removed(record)) => assert_eq!(record, machine),
            _ => panic!("expected Removed event"),
        }
        assert!(last_seen.is_empty());
    }

    #[test]
    fn machine_event_for_silent_on_unknown_tombstone() {
        let mut last_seen = HashMap::new();
        let change = machine_event_for(
            &mut last_seen,
            StateMessage::Tombstone {
                key: "unknown".into(),
            },
        );
        assert!(matches!(change, StateChange::Silent));
    }
}
