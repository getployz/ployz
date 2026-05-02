use std::collections::HashMap;

use async_nats::HeaderMap;
use async_nats::jetstream::context::PublishErrorKind;
use async_nats::jetstream::message::PublishMessage;
use ployz_store_api::InstanceStatusRepository;
use ployz_types::error::{Error, Result};
use ployz_types::model::{InstanceId, InstanceStatusRecord, MachineId, RoutingEvent};
use ployz_types::spec::Namespace;

use crate::NatsStore;
use crate::buckets::{ensure_machine_state_assets, local_read_stream_name};
use crate::store::kv_json;
use crate::store::state_view::{self, StateChange, StateMessage};
use crate::subjects::{
    INSTANCE_STATE_VIEW_STREAM, instance_state_namespace_filter, instance_state_stream,
    instance_state_subject, instance_tombstone_subject,
};

impl InstanceStatusRepository for NatsStore {
    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        let (snapshot, _events) = list_instance_view_with_filter(self, Some(namespace)).await?;
        Ok(snapshot)
    }

    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        ensure_machine_state_assets(self, &record.machine_id).await?;
        let old = read_instance_state(self, &record.machine_id, &record.namespace, &record.instance_id)
            .await?;
        publish_instance_state(self, record).await?;
        if let Some(ref old) = old
            && old == record
        {
            return Ok(());
        }
        let event = match old {
            Some(old) => RoutingEvent::InstanceUpdated {
                old,
                new: record.clone(),
            },
            None => RoutingEvent::InstanceAdded(record.clone()),
        };
        self.publish_routing_batch(
            format!("instance:{}", record.instance_id.0),
            "instance.status",
            &[event],
        )
        .await
    }

    async fn remove_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        let Some(old) = locate_instance_state(self, instance_id).await? else {
            return Ok(());
        };
        publish_instance_tombstone(self, &old.machine_id, &old.namespace, &old.instance_id)
            .await?;
        self.publish_routing_batch(
            format!("instance:remove:{}", instance_id.0),
            "instance.remove",
            &[RoutingEvent::InstanceRemoved(old)],
        )
        .await
    }
}

pub(crate) async fn list_all_instance_status(
    store: &NatsStore,
) -> Result<Vec<InstanceStatusRecord>> {
    let (snapshot, _events) = list_instance_view_with_filter(store, None).await?;
    Ok(snapshot)
}

async fn list_instance_view_with_filter(
    store: &NatsStore,
    namespace: Option<&Namespace>,
) -> Result<(
    Vec<InstanceStatusRecord>,
    tokio::sync::mpsc::Receiver<Result<()>>,
)> {
    let filter = namespace.map(instance_state_namespace_filter);
    state_view::subscribe(
        store,
        &local_read_stream_name(store, INSTANCE_STATE_VIEW_STREAM),
        filter,
        decode_instance_message,
        instance_event_for,
        |record: &InstanceStatusRecord| record.instance_id.0.clone(),
    )
    .await
}

/// Direct-get the most recent state record for one instance from the
/// local view. Returns `None` if the view doesn't carry the instance
/// (not yet replicated, never published, or tombstoned).
async fn read_instance_state(
    store: &NatsStore,
    machine_id: &MachineId,
    namespace: &Namespace,
    instance_id: &InstanceId,
) -> Result<Option<InstanceStatusRecord>> {
    let stream_name = local_read_stream_name(store, INSTANCE_STATE_VIEW_STREAM);
    let stream = match store.local_jetstream().get_stream(&stream_name).await {
        Ok(stream) => stream,
        Err(_) => return Ok(None),
    };
    let subject = instance_state_subject(machine_id, namespace, instance_id);
    match stream.direct_get_last_for_subject(&subject).await {
        Ok(message) => Ok(Some(kv_json::decode_json::<InstanceStatusRecord>(
            "nats_instance_decode",
            message.payload.as_ref(),
        )?)),
        Err(_) => Ok(None),
    }
}

/// Find the current record for an instance without knowing its namespace
/// or owning machine — used for `remove_instance_status` whose API only
/// takes an `InstanceId`. Walks the view snapshot.
async fn locate_instance_state(
    store: &NatsStore,
    instance_id: &InstanceId,
) -> Result<Option<InstanceStatusRecord>> {
    let snapshot = list_all_instance_status(store).await?;
    Ok(snapshot
        .into_iter()
        .find(|record| record.instance_id == *instance_id))
}

async fn publish_instance_state(store: &NatsStore, record: &InstanceStatusRecord) -> Result<()> {
    let payload = serde_json::to_vec(record)
        .map_err(|error| Error::operation("nats_instance_encode", error.to_string()))?;
    let publish = PublishMessage::build()
        .payload(payload.into())
        .expected_stream(instance_state_stream(&record.machine_id))
        .message_id(format!("instance_state:{}", record.instance_id.0));
    let ack = store
        .hub_jetstream()
        .send_publish(
            instance_state_subject(&record.machine_id, &record.namespace, &record.instance_id),
            publish,
        )
        .await
        .map_err(|error| Error::operation("nats_instance_publish", format!("{error:?}")))?;
    match ack.await {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == PublishErrorKind::WrongLastSequence => Ok(()),
        Err(error) => Err(Error::operation(
            "nats_instance_ack",
            format!("{error:?}"),
        )),
    }
}

async fn publish_instance_tombstone(
    store: &NatsStore,
    machine_id: &MachineId,
    namespace: &Namespace,
    instance_id: &InstanceId,
) -> Result<()> {
    let publish = PublishMessage::build()
        .payload(Vec::new().into())
        .expected_stream(instance_state_stream(machine_id))
        .headers(HeaderMap::new())
        .message_id(format!("instance_tombstone:{}", instance_id.0));
    let ack = store
        .hub_jetstream()
        .send_publish(
            instance_tombstone_subject(machine_id, namespace, instance_id),
            publish,
        )
        .await
        .map_err(|error| {
            Error::operation("nats_instance_tombstone_publish", format!("{error:?}"))
        })?;
    match ack.await {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == PublishErrorKind::WrongLastSequence => Ok(()),
        Err(error) => Err(Error::operation(
            "nats_instance_tombstone_ack",
            format!("{error:?}"),
        )),
    }
}

fn decode_instance_message(
    subject: &str,
    payload: &[u8],
) -> Result<StateMessage<InstanceStatusRecord>> {
    let key = subject_to_instance_key(subject)?;
    if subject_is_instance_tombstone(subject) {
        return Ok(StateMessage::Tombstone { key });
    }
    let record = kv_json::decode_json::<InstanceStatusRecord>("nats_instance_decode", payload)?;
    Ok(StateMessage::Upsert { key, value: record })
}

fn instance_event_for(
    last_seen: &mut HashMap<String, InstanceStatusRecord>,
    decoded: StateMessage<InstanceStatusRecord>,
) -> StateChange<InstanceStatusRecord, ()> {
    match decoded {
        StateMessage::Upsert { key, value } => {
            match last_seen.get(&key) {
                Some(existing) if existing == &value => StateChange::Silent,
                _ => {
                    last_seen.insert(key, value);
                    // No subscribe API exposes instance events today;
                    // updates land in the snapshot via the inserted map.
                    // Step 8's mirror_lag work surfaces staleness.
                    StateChange::Silent
                }
            }
        }
        StateMessage::Tombstone { key } => {
            last_seen.remove(&key);
            StateChange::Silent
        }
    }
}

/// Subjects for instance state are `state.<machine>.instance.<ns>.<id>`
/// and `state.<machine>.instance_tombstone.<ns>.<id>`. We key the map by
/// the full `<machine>:<ns>:<instance>` triple so tombstones cancel
/// upserts on the same logical row.
fn subject_to_instance_key(subject: &str) -> Result<String> {
    let parts: Vec<&str> = subject.split('.').collect();
    let [_, machine_token, kind, namespace_token, instance_token] = parts.as_slice() else {
        return Err(Error::operation(
            "nats_instance_state_subject",
            format!("unexpected instance state subject: {subject}"),
        ));
    };
    if !matches!(*kind, "instance" | "instance_tombstone") {
        return Err(Error::operation(
            "nats_instance_state_subject",
            format!("unexpected instance state subject kind: {subject}"),
        ));
    }
    Ok(format!(
        "{}:{}:{}",
        machine_token, namespace_token, instance_token
    ))
}

fn subject_is_instance_tombstone(subject: &str) -> bool {
    subject
        .split('.')
        .nth(2)
        .is_some_and(|kind| kind == "instance_tombstone")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{
        DeployId, DrainState, InstancePhase, MachineId, SlotId,
    };
    use std::collections::BTreeMap;

    #[test]
    fn subject_decode_extracts_machine_namespace_instance() {
        let key = subject_to_instance_key("state.m1.instance.alpha.inst-1").expect("decode");
        assert_eq!(key, "m1:alpha:inst-1");
    }

    #[test]
    fn subject_decode_extracts_for_tombstone_subject() {
        let key = subject_to_instance_key("state.m1.instance_tombstone.alpha.inst-1")
            .expect("decode");
        assert_eq!(key, "m1:alpha:inst-1");
    }

    #[test]
    fn subject_is_instance_tombstone_distinguishes_kind() {
        assert!(subject_is_instance_tombstone(
            "state.m1.instance_tombstone.alpha.inst-1"
        ));
        assert!(!subject_is_instance_tombstone(
            "state.m1.instance.alpha.inst-1"
        ));
    }

    #[test]
    fn subject_decode_rejects_wrong_arity() {
        assert!(subject_to_instance_key("state.m1.instance.alpha").is_err());
        assert!(subject_to_instance_key("state.m1.instance.alpha.inst.extra").is_err());
    }

    fn test_instance(id: &str, ns: &str, machine: &str) -> InstanceStatusRecord {
        InstanceStatusRecord {
            instance_id: InstanceId(id.into()),
            namespace: Namespace(ns.into()),
            service: "svc".into(),
            slot_id: SlotId("0".into()),
            machine_id: MachineId(machine.into()),
            revision_hash: String::new(),
            deploy_id: DeployId("d".into()),
            docker_container_id: String::new(),
            overlay_ip: None,
            backend_ports: BTreeMap::new(),
            phase: InstancePhase::Pending,
            ready: false,
            drain_state: DrainState::None,
            error: None,
            started_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn instance_event_for_skips_identical_value() {
        let mut last_seen = HashMap::new();
        let record = test_instance("inst-1", "alpha", "m1");
        last_seen.insert("m1:alpha:inst-1".into(), record.clone());
        let change = instance_event_for(
            &mut last_seen,
            StateMessage::Upsert {
                key: "m1:alpha:inst-1".into(),
                value: record,
            },
        );
        assert!(matches!(change, StateChange::Silent));
    }

    #[test]
    fn instance_event_for_inserts_on_first_observation() {
        let mut last_seen = HashMap::new();
        let record = test_instance("inst-1", "alpha", "m1");
        let _ = instance_event_for(
            &mut last_seen,
            StateMessage::Upsert {
                key: "m1:alpha:inst-1".into(),
                value: record.clone(),
            },
        );
        assert_eq!(last_seen.get("m1:alpha:inst-1"), Some(&record));
    }

    #[test]
    fn instance_event_for_removes_on_tombstone() {
        let mut last_seen = HashMap::new();
        let record = test_instance("inst-1", "alpha", "m1");
        last_seen.insert("m1:alpha:inst-1".into(), record);
        let _ = instance_event_for(
            &mut last_seen,
            StateMessage::Tombstone {
                key: "m1:alpha:inst-1".into(),
            },
        );
        assert!(last_seen.is_empty());
    }
}
