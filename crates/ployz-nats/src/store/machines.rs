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
        kv_json::delete(&kv, id.0.as_str(), "nats_machine_delete").await?;
        let Some(old) = old else {
            return Ok(());
        };
        self.publish_routing_batch(
            format!("machine:delete:{}", id.0),
            "machine.delete",
            &[RoutingEvent::MachineRemoved(old)],
        )
        .await
    }

    async fn subscribe_machines(&self) -> Result<MachineSubscription> {
        let kv = machines_bucket(self).await?;
        let mut watch = kv
            .watch_all()
            .await
            .map_err(|error| Error::operation("nats_machines_watch", format!("{error:?}")))?;
        let snapshot = self.list_machines().await?;
        let (tx, rx) = mpsc::channel(128);
        let last_seen_snapshot = snapshot.clone();
        tokio::spawn(async move {
            let mut last_seen = last_seen_snapshot
                .iter()
                .map(|record| (record.id.0.clone(), record.clone()))
                .collect::<HashMap<_, _>>();
            while let Some(next) = watch.next().await {
                let entry = match next {
                    Ok(entry) => entry,
                    Err(error) => {
                        let error = Error::operation("nats_machines_watch", format!("{error:?}"));
                        warn!(?error, "NATS machines watcher failed");
                        let _ = tx.send(Err(error)).await;
                        break;
                    }
                };
                let event = match entry.operation {
                    kv::Operation::Put => match decode_machine(&entry.key, entry.value.as_ref()) {
                        Ok(machine) => {
                            let event = if last_seen.contains_key(&entry.key) {
                                MachineEvent::Updated(machine.clone())
                            } else {
                                MachineEvent::Added(machine.clone())
                            };
                            last_seen.insert(entry.key, machine);
                            event
                        }
                        Err(error) => {
                            warn!(?error, key = %entry.key, "NATS machine event decode failed");
                            continue;
                        }
                    },
                    kv::Operation::Delete | kv::Operation::Purge => {
                        match last_seen.remove(&entry.key) {
                            Some(machine) => MachineEvent::Removed(machine),
                            None => continue,
                        }
                    }
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
