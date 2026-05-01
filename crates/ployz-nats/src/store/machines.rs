use async_nats::jetstream::kv;
use futures_util::{StreamExt, TryStreamExt};
use ployz_store_api::{MachineRegistry, MachineSubscription};
use ployz_types::error::{Error, Result};
use ployz_types::model::{MachineEvent, MachineId, MachineMembership};
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
        kv_json::put_json(
            &kv,
            record.id.0.as_str(),
            record,
            "nats_machine_encode",
            "nats_machine_put",
        )
        .await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        let kv = machines_bucket(self).await?;
        kv_json::delete(&kv, id.0.as_str(), "nats_machine_delete").await
    }

    async fn subscribe_machines(&self) -> Result<MachineSubscription> {
        let snapshot = self.list_machines().await?;
        let kv = machines_bucket(self).await?;
        let mut watch = kv
            .watch_all()
            .await
            .map_err(|error| Error::operation("nats_machines_watch", format!("{error:?}")))?;
        let (tx, rx) = mpsc::channel(128);
        tokio::spawn(async move {
            while let Some(next) = watch.next().await {
                let entry = match next {
                    Ok(entry) => entry,
                    Err(error) => {
                        warn!(?error, "NATS machines watcher failed");
                        break;
                    }
                };
                let event = match entry.operation {
                    kv::Operation::Put => match decode_machine(&entry.key, entry.value.as_ref()) {
                        Ok(machine) => MachineEvent::Updated(machine),
                        Err(error) => {
                            warn!(?error, key = %entry.key, "NATS machine event decode failed");
                            continue;
                        }
                    },
                    kv::Operation::Delete | kv::Operation::Purge => {
                        MachineEvent::Removed(MachineMembership::seed(
                            MachineId(entry.key),
                            ployz_types::model::PublicKey([0; 32]),
                            ployz_types::model::OverlayIp(std::net::Ipv6Addr::UNSPECIFIED),
                            None,
                            Vec::new(),
                        ))
                    }
                };
                if tx.send(event).await.is_err() {
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
