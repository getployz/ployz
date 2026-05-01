use ployz_store_api::InstanceStatusRepository;
use ployz_types::error::Result;
use ployz_types::model::{InstanceId, InstanceStatusRecord};
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
        kv_json::put_json(
            &bucket,
            &record.instance_id.0,
            record,
            "nats_instance_encode",
            "nats_instance_put",
        )
        .await
    }

    async fn remove_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        let bucket =
            kv_json::get_bucket(self.jetstream(), INSTANCES_BUCKET, "nats_instances_bucket")
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
