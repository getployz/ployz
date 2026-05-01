use async_nats::jetstream::kv;
use futures_util::{StreamExt, TryStreamExt};
use ployz_store_api::{MachineRegistry, MachineSubscription};
use ployz_types::error::{Error, Result};
use ployz_types::model::{MachineEvent, MachineId, MachineMembership, RoutingEvent};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::warn;

use crate::NatsStore;
use crate::buckets::MACHINES_BUCKET;
use crate::store::kv_json;

impl MachineRegistry for NatsStore {
    async fn list_machines(&self) -> Result<Vec<MachineMembership>> {
        let kv = machines_bucket(self).await?;
        let keys = kv
            .keys()
            .await
            .map_err(|error| Error::operation("nats_machines_keys", format!("{error:?}")))?
            .try_collect::<Vec<String>>()
            .await
            .map_err(|error| Error::operation("nats_machines_keys", format!("{error:?}")))?;
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
        self.publish_routing_batch(
            format!("machine:{}", record.id.0),
            "machine.upsert",
            &[event],
        )
        .await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        let kv = machines_bucket(self).await?;
        let old = kv
            .get(id.0.as_str())
            .await
            .map_err(|error| Error::operation("nats_machine_get", format!("{error:?}")))?
            .map(|bytes| decode_machine(id.0.as_str(), bytes.as_ref()))
            .transpose()?;
        let Some(old) = old else {
            return Ok(());
        };
        self.publish_routing_batch(
            format!("machine:delete:{}", id.0),
            "machine.delete",
            &[RoutingEvent::MachineRemoved(old)],
        )
        .await?;
        kv_json::delete(&kv, id.0.as_str(), "nats_machine_delete").await
    }

    async fn subscribe_machines(&self) -> Result<MachineSubscription> {
        let kv = machines_bucket(self).await?;
        let snapshot_boundary =
            kv_json::latest_sequence(&kv, "nats_machines_snapshot_boundary").await?;
        let snapshot = self.list_machines().await?;
        let mut watch = kv
            .watch_all_from_revision(kv_json::next_sequence(snapshot_boundary))
            .await
            .map_err(|error| Error::operation("nats_machines_watch", format!("{error:?}")))?;
        let (tx, rx) = mpsc::channel(128);
        let last_seen_snapshot = snapshot.clone();
        tokio::spawn(async move {
            let mut last_seen = last_seen_snapshot
                .iter()
                .map(|record| (record.id.0.clone(), record.clone()))
                .collect::<HashMap<_, _>>();
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
                        let error = Error::operation("nats_machines_watch", format!("{error:?}"));
                        warn!(?error, "NATS machines watcher failed");
                        let _ = tx.send(Err(error)).await;
                        break;
                    }
                };
                let Some(event) = (match machine_event_from_kv_entry(
                    &mut last_seen,
                    entry.key.as_str(),
                    entry.value.as_ref(),
                    entry.operation,
                ) {
                    Ok(event) => event,
                    Err(error) => {
                        warn!(?error, key = %entry.key, "NATS machine event decode failed");
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
}

async fn machines_bucket(store: &NatsStore) -> Result<kv::Store> {
    kv_json::get_bucket(store.jetstream(), MACHINES_BUCKET, "nats_machines_bucket").await
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
            let event = if last_seen.contains_key(key) {
                MachineEvent::Updated(machine.clone())
            } else {
                MachineEvent::Added(machine.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{
        MachineLifecycle, MachineRole, MachineTopology, OverlayIp, PublicKey,
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
            control_target: None,
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
}
