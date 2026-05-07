use async_nats::jetstream::kv;
use futures_util::TryStreamExt;
use ployz_store_api::{MachineRegistry, MachineSubscription};
use ployz_types::error::{Error, Result};
use ployz_types::model::{MachineEvent, MachineId, MachineMembership, RoutingEvent};
use std::collections::HashMap;

use crate::NatsStore;
use crate::buckets::MACHINES_BUCKET;
use crate::store::kv_json;
use crate::store::kv_watch;

impl MachineRegistry for NatsStore {
    async fn list_machines(&self) -> Result<Vec<MachineMembership>> {
        let kv = machines_bucket(self).await?;
        let mut keys = kv
            .keys()
            .await
            .map_err(|error| Error::operation("nats_machines_keys", format!("{error:?}")))?
            .try_collect::<Vec<String>>()
            .await
            .map_err(|error| Error::operation("nats_machines_keys", format!("{error:?}")))?;
        keys.sort();
        let mut machines = Vec::new();
        for key in keys {
            let Some(bytes) = kv
                .get(key.clone())
                .await
                .map_err(|error| Error::operation("nats_machine_get", format!("{error:?}")))?
            else {
                continue;
            };
            machines.push(decode_machine(&key, bytes.as_ref())?);
        }
        Ok(machines)
    }

    async fn upsert_self_machine(&self, record: &MachineMembership) -> Result<()> {
        let kv = machines_bucket(self).await?;
        let old = kv
            .get(record.id.0.as_str())
            .await
            .map_err(|error| Error::operation("nats_machine_get", format!("{error:?}")))?
            .map(|bytes| decode_machine(record.id.0.as_str(), bytes.as_ref()))
            .transpose()?;
        kv_json::put_json(
            &kv,
            record.id.0.as_str(),
            record,
            "nats_machine_encode",
            "nats_machine_put",
        )
        .await?;
        let event = match old {
            Some(old) => RoutingEvent::MachineUpdated {
                old,
                new: record.clone(),
            },
            None => RoutingEvent::MachineAdded(record.clone()),
        };
        self.publish_routing_events(
            format!("machine:{}", record.id.0),
            "machine.upsert",
            &[event],
        )
        .await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        let kv = machines_bucket(self).await?;
        kv_json::delete(&kv, id.0.as_str(), "nats_machine_delete").await?;
        self.publish_routing_events(
            format!("machine:delete:{}", id.0),
            "machine.delete",
            &[RoutingEvent::MachineRemoved { id: id.clone() }],
        )
        .await
    }

    async fn subscribe_machines(&self) -> Result<MachineSubscription> {
        let kv = machines_bucket(self).await?;
        let snapshot_boundary =
            kv_json::latest_sequence(&kv, "nats_machines_snapshot_boundary").await?;
        let snapshot_entries = kv_json::list_json_entries::<MachineMembership>(
            &kv,
            "nats_machine_decode",
            "nats_machines_list",
        )
        .await?;
        let (snapshot, snapshot_revisions) = machine_snapshot_parts(snapshot_entries)?;
        kv_watch::subscribe_all_with_snapshot_revisions(
            &kv,
            snapshot,
            snapshot_revisions,
            snapshot_boundary,
            |record: &MachineMembership| record.id.0.clone(),
            "nats_machines_watch",
            "NATS machines watcher failed",
            "NATS machine event decode failed",
            machine_event_from_kv_entry,
        )
        .await
    }
}

async fn machines_bucket(store: &NatsStore) -> Result<kv::Store> {
    kv_json::get_bucket(store.jetstream(), MACHINES_BUCKET, "nats_machines_bucket").await
}

fn machine_snapshot_parts(
    entries: Vec<kv_json::JsonEntry<MachineMembership>>,
) -> Result<(Vec<MachineMembership>, HashMap<String, u64>)> {
    let mut snapshot = Vec::with_capacity(entries.len());
    let mut snapshot_revisions = HashMap::with_capacity(entries.len());
    for entry in entries {
        let machine = validate_machine_key(&entry.key, entry.value)?;
        snapshot_revisions.insert(entry.key, entry.revision);
        snapshot.push(machine);
    }
    snapshot.sort_by(|left, right| left.id.cmp(&right.id));
    Ok((snapshot, snapshot_revisions))
}

fn machine_event_from_kv_entry(
    last_seen: &mut HashMap<String, MachineMembership>,
    key: &str,
    bytes: &[u8],
    operation: kv::Operation,
) -> Result<Option<MachineEvent>> {
    match operation {
        kv::Operation::Put => {
            let machine = decode_machine(key, bytes)?;
            let event = match last_seen.get(key) {
                Some(existing) if existing == &machine => return Ok(None),
                Some(_) => MachineEvent::Updated(machine.clone()),
                None => MachineEvent::Added(machine.clone()),
            };
            last_seen.insert(key.to_string(), machine);
            Ok(Some(event))
        }
        kv::Operation::Delete | kv::Operation::Purge => {
            Ok(last_seen.remove(key).map(MachineEvent::Removed))
        }
    }
}

fn decode_machine(key: &str, bytes: &[u8]) -> Result<MachineMembership> {
    let record: MachineMembership = kv_json::decode_json("nats_machine_decode", bytes)?;
    if record.id.0 != key {
        return Err(Error::operation(
            "nats_machine_decode",
            format!(
                "machine key {key} does not match payload id {}",
                record.id.0
            ),
        ));
    }
    Ok(record)
}

fn validate_machine_key(key: &str, record: MachineMembership) -> Result<MachineMembership> {
    if record.id.0 != key {
        return Err(Error::operation(
            "nats_machine_decode",
            format!(
                "machine key {key} does not match payload id {}",
                record.id.0
            ),
        ));
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{
        MachineLifecycle, MachineTopology, OverlayIp, PublicKey, StorageParticipation,
    };

    #[test]
    fn machine_kv_decode_failure_is_subscription_failure() {
        let mut last_seen = HashMap::new();

        let result =
            machine_event_from_kv_entry(&mut last_seen, "machine-a", b"{", kv::Operation::Put);

        assert!(result.is_err());
        assert!(last_seen.is_empty());
    }

    #[test]
    fn machine_kv_key_mismatch_is_visible() {
        let machine = test_machine("payload-machine");
        let bytes = serde_json::to_vec(&machine).expect("encode machine");

        let error =
            decode_machine("key-machine", &bytes).expect_err("key mismatch should fail visibly");

        assert!(error.to_string().contains("key-machine"));
        assert!(error.to_string().contains("payload-machine"));
    }

    #[test]
    fn machine_kv_decode_accepts_matching_key() {
        let machine = test_machine("machine-a");
        let bytes = serde_json::to_vec(&machine).expect("encode machine");

        let decoded = decode_machine("machine-a", &bytes).expect("matching key decodes");

        assert_eq!(decoded, machine);
    }

    #[test]
    fn machine_snapshot_parts_keep_entry_revisions() {
        let machine_a = test_machine("machine-a");
        let machine_b = test_machine("machine-b");
        let entries = vec![
            kv_json::JsonEntry {
                key: String::from("machine-b"),
                revision: 41,
                value: machine_b,
            },
            kv_json::JsonEntry {
                key: String::from("machine-a"),
                revision: 42,
                value: machine_a,
            },
        ];

        let (snapshot, revisions) =
            machine_snapshot_parts(entries).expect("snapshot parts should decode");

        assert_eq!(
            snapshot
                .iter()
                .map(|machine| machine.id.0.as_str())
                .collect::<Vec<_>>(),
            ["machine-a", "machine-b"]
        );
        assert_eq!(revisions.get("machine-a"), Some(&42));
        assert_eq!(revisions.get("machine-b"), Some(&41));
    }

    #[test]
    fn machine_kv_delete_for_unknown_key_is_noop() {
        let mut last_seen = HashMap::new();

        let event =
            machine_event_from_kv_entry(&mut last_seen, "machine-a", &[], kv::Operation::Delete)
                .expect("delete should not fail");

        assert!(event.is_none());
    }

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
            storage: true,
            storage_participation: StorageParticipation::default_authority(),
            created_at: 1,
            updated_at: 1,
            labels: Default::default(),
        }
    }

    #[test]
    fn machine_kv_put_updates_last_seen() {
        let machine = test_machine("machine-a");
        let bytes = serde_json::to_vec(&machine).expect("encode machine");
        let mut last_seen = HashMap::new();

        let event =
            machine_event_from_kv_entry(&mut last_seen, "machine-a", &bytes, kv::Operation::Put)
                .expect("put should decode");

        assert!(matches!(event, Some(MachineEvent::Added(record)) if record == machine));
        assert_eq!(last_seen.get("machine-a"), Some(&machine));
    }

    #[test]
    fn machine_kv_put_ignores_identical_value() {
        let machine = test_machine("machine-a");
        let bytes = serde_json::to_vec(&machine).expect("encode machine");
        let mut last_seen = HashMap::from([(String::from("machine-a"), machine.clone())]);

        let event =
            machine_event_from_kv_entry(&mut last_seen, "machine-a", &bytes, kv::Operation::Put)
                .expect("put should decode");

        assert!(event.is_none());
        assert_eq!(last_seen.get("machine-a"), Some(&machine));
    }
}
