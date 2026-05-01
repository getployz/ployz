use ployz_store_api::InstanceStatusRepository;
use ployz_types::error::{Error, Result};
use ployz_types::model::{InstanceId, InstanceStatusRecord, RoutingEvent};
use ployz_types::spec::Namespace;

use crate::NatsStore;
use crate::buckets::INSTANCES_BUCKET;
use crate::store::kv_json;

impl InstanceStatusRepository for NatsStore {
    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        let bucket =
            kv_json::get_bucket(self.jetstream(), INSTANCES_BUCKET, "nats_instances_bucket")
                .await?;
        let records = kv_json::list_json::<InstanceStatusRecord>(
            &bucket,
            "nats_instance_decode",
            "nats_instances_list",
        )
        .await?;
        Ok(records
            .into_iter()
            .filter(|record| &record.namespace == namespace)
            .collect())
    }

    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        let bucket =
            kv_json::get_bucket(self.jetstream(), INSTANCES_BUCKET, "nats_instances_bucket")
                .await?;
        let old = bucket
            .get(&record.instance_id.0)
            .await
            .map_err(|error| Error::operation("nats_instance_get", format!("{error:?}")))?
            .map(|bytes| kv_json::decode_json("nats_instance_decode", bytes.as_ref()))
            .transpose()?;
        kv_json::put_json(
            &bucket,
            &record.instance_id.0,
            record,
            "nats_instance_encode",
            "nats_instance_put",
        )
        .await?;
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
        let bucket =
            kv_json::get_bucket(self.jetstream(), INSTANCES_BUCKET, "nats_instances_bucket")
                .await?;
        let old = bucket
            .get(&instance_id.0)
            .await
            .map_err(|error| Error::operation("nats_instance_get", format!("{error:?}")))?
            .map(|bytes| kv_json::decode_json("nats_instance_decode", bytes.as_ref()))
            .transpose()?;
        let Some(old) = old else {
            return Ok(());
        };
        self.publish_routing_batch(
            format!("instance:remove:{}", instance_id.0),
            "instance.remove",
            &[RoutingEvent::InstanceRemoved(old)],
        )
        .await?;
        kv_json::delete(&bucket, &instance_id.0, "nats_instance_delete").await
    }
}

pub(crate) async fn list_all_instance_status(
    store: &NatsStore,
) -> Result<Vec<InstanceStatusRecord>> {
    let bucket =
        kv_json::get_bucket(store.jetstream(), INSTANCES_BUCKET, "nats_instances_bucket").await?;
    kv_json::list_json::<InstanceStatusRecord>(
        &bucket,
        "nats_instance_decode",
        "nats_instances_list",
    )
    .await
}
