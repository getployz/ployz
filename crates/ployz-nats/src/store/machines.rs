use async_nats::jetstream::kv;
use async_trait::async_trait;
use ployz_store_api::{MachineMembershipStore, MachineSubscription};
use ployz_types::error::{Error, Result, StoreRecordKind};
use ployz_types::model::{MachineEvent, MachineId, MachineMembership, RoutingEvent};

use crate::NatsStore;
use crate::store::kv_json;
use crate::store::kv_watch;

#[async_trait]
impl MachineMembershipStore for NatsStore {
    async fn list_machines(&self) -> Result<Vec<MachineMembership>> {
        let kv = machines_bucket(self).await?;
        let entries = kv_json::list_json_entries::<MachineMembership>(
            &kv,
            "nats_machine_decode",
            "nats_machines_list",
        )
        .await?;
        let machines = machine_snapshot(entries)?;
        Ok(machines)
    }

    async fn upsert_self_machine(&self, record: &MachineMembership) -> Result<()> {
        let kv = machines_bucket(self).await?;
        kv_json::put_json(
            &kv,
            record.id.0.as_str(),
            record,
            "nats_machine_encode",
            "nats_machine_put",
        )
        .await?;
        self.publish_routing_events(
            format!("machine:{}", record.id.0),
            "machine.upsert",
            &[RoutingEvent::MachineUpsert(record.clone())],
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
        let observed_revision =
            kv_json::latest_sequence(&kv, "nats_machines_observed_revision").await?;
        let snapshot_entries = kv_json::list_json_entries::<MachineMembership>(
            &kv,
            "nats_machine_decode",
            "nats_machines_list",
        )
        .await?;
        let snapshot = machine_snapshot(snapshot_entries)?;
        kv_watch::subscribe_all(
            &kv,
            snapshot,
            observed_revision,
            |record: &MachineMembership| record.id.0.clone(),
            decode_machine,
            MachineEvent::Upsert,
            |machine| MachineEvent::Removed { id: machine.id },
            "nats_machines_watch",
            "NATS machines watcher failed",
            "NATS machine event decode failed",
        )
        .await
    }
}

async fn machines_bucket(store: &NatsStore) -> Result<kv::Store> {
    kv_json::get_bucket(
        store.jetstream(),
        store.assets().machines_bucket.as_str(),
        "nats_machines_bucket",
    )
    .await
}

fn machine_snapshot(
    entries: Vec<kv_json::JsonEntry<MachineMembership>>,
) -> Result<Vec<MachineMembership>> {
    let mut snapshot = Vec::with_capacity(entries.len());
    for entry in entries {
        let machine = validate_machine_key(&entry.key, entry.value)?;
        snapshot.push(machine);
    }
    snapshot.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(snapshot)
}

fn decode_machine(key: &str, bytes: &[u8]) -> Result<MachineMembership> {
    let record: MachineMembership = kv_json::decode_json("nats_machine_decode", bytes)?;
    if record.id.0 != key {
        return Err(Error::store_key_mismatch(
            StoreRecordKind::Machine,
            key,
            record.id.0,
        ));
    }
    Ok(record)
}

fn validate_machine_key(key: &str, record: MachineMembership) -> Result<MachineMembership> {
    if record.id.0 != key {
        return Err(Error::store_key_mismatch(
            StoreRecordKind::Machine,
            key,
            record.id.0,
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
    fn machine_kv_key_mismatch_is_visible() {
        let machine = test_machine("payload-machine");
        let bytes = serde_json::to_vec(&machine).expect("encode machine");

        let error =
            decode_machine("key-machine", &bytes).expect_err("key mismatch should fail visibly");

        assert_eq!(
            error,
            Error::store_key_mismatch(StoreRecordKind::Machine, "key-machine", "payload-machine")
        );
    }

    #[test]
    fn machine_kv_decode_accepts_matching_key() {
        let machine = test_machine("machine-a");
        let bytes = serde_json::to_vec(&machine).expect("encode machine");

        let decoded = decode_machine("machine-a", &bytes).expect("matching key decodes");

        assert_eq!(decoded, machine);
    }

    #[test]
    fn machine_snapshot_returns_records_in_contract_identity_order() {
        let machine_a = test_machine("machine-a");
        let machine_b = test_machine("machine-b");
        let entries = vec![
            kv_json::JsonEntry {
                key: String::from("machine-b"),
                value: machine_b,
            },
            kv_json::JsonEntry {
                key: String::from("machine-a"),
                value: machine_a,
            },
        ];

        let snapshot = machine_snapshot(entries).expect("snapshot should decode");

        assert_eq!(
            snapshot
                .iter()
                .map(|machine| machine.id.0.as_str())
                .collect::<Vec<_>>(),
            ["machine-a", "machine-b"]
        );
    }

    #[test]
    fn machine_snapshot_rejects_key_mismatch() {
        let entries = vec![kv_json::JsonEntry {
            key: String::from("key-machine"),
            value: test_machine("payload-machine"),
        }];

        let error = machine_snapshot(entries).expect_err("key mismatch should fail");

        assert_eq!(
            error,
            Error::store_key_mismatch(StoreRecordKind::Machine, "key-machine", "payload-machine")
        );
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
}
