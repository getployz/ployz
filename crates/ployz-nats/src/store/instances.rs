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
        let records = list_instances(&bucket).await?;
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
            .map(|bytes| decode_instance(&record.instance_id.0, bytes.as_ref()))
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
        self.publish_routing_events(
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
        kv_json::delete(&bucket, &instance_id.0, "nats_instance_delete").await?;
        self.publish_routing_events(
            format!("instance:remove:{}", instance_id.0),
            "instance.remove",
            &[RoutingEvent::InstanceRemoved {
                instance_id: instance_id.clone(),
            }],
        )
        .await
    }
}

pub(crate) async fn list_all_instance_status(
    store: &NatsStore,
) -> Result<Vec<InstanceStatusRecord>> {
    let bucket =
        kv_json::get_bucket(store.jetstream(), INSTANCES_BUCKET, "nats_instances_bucket").await?;
    list_instances(&bucket).await
}

async fn list_instances(
    bucket: &async_nats::jetstream::kv::Store,
) -> Result<Vec<InstanceStatusRecord>> {
    kv_json::list_json_entries::<InstanceStatusRecord>(
        bucket,
        "nats_instance_decode",
        "nats_instances_list",
    )
    .await?
    .into_iter()
    .map(|entry| validate_instance_key(&entry.key, entry.value))
    .collect()
}

fn decode_instance(key: &str, bytes: &[u8]) -> Result<InstanceStatusRecord> {
    let record: InstanceStatusRecord = kv_json::decode_json("nats_instance_decode", bytes)?;
    validate_instance_key(key, record)
}

fn validate_instance_key(key: &str, record: InstanceStatusRecord) -> Result<InstanceStatusRecord> {
    if record.instance_id.0 != key {
        return Err(Error::operation(
            "nats_instance_decode",
            format!(
                "instance key {key} does not match payload id {}",
                record.instance_id.0
            ),
        ));
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::decode_instance;
    use ployz_types::model::{
        DeployId, DrainState, InstanceId, InstancePhase, InstanceStatusRecord, MachineId, SlotId,
    };
    use ployz_types::spec::Namespace;
    use std::collections::BTreeMap;

    #[test]
    fn instance_kv_decode_failure_is_visible() {
        let error = decode_instance("instance-a", b"{").expect_err("invalid JSON should fail");

        assert!(error.to_string().contains("nats_instance_decode"));
    }

    #[test]
    fn instance_kv_key_mismatch_is_visible() {
        let record = test_instance("payload-instance");
        let bytes = serde_json::to_vec(&record).expect("encode instance");

        let error = decode_instance("key-instance", &bytes).expect_err("key mismatch should fail");

        assert!(error.to_string().contains("key-instance"));
        assert!(error.to_string().contains("payload-instance"));
    }

    #[test]
    fn instance_kv_decode_accepts_matching_key() {
        let record = test_instance("instance-a");
        let bytes = serde_json::to_vec(&record).expect("encode instance");

        let decoded = decode_instance("instance-a", &bytes).expect("matching key decodes");

        assert_eq!(decoded, record);
    }

    fn test_instance(id: &str) -> InstanceStatusRecord {
        InstanceStatusRecord {
            instance_id: InstanceId(id.into()),
            namespace: Namespace("prod".into()),
            service: "api".into(),
            slot_id: SlotId("slot-1".into()),
            machine_id: MachineId("machine-a".into()),
            revision_hash: "rev-1".into(),
            deploy_id: DeployId("deploy-1".into()),
            docker_container_id: format!("container-{id}"),
            overlay_ip: None,
            backend_ports: BTreeMap::new(),
            phase: InstancePhase::Ready,
            ready: true,
            drain_state: DrainState::None,
            error: None,
            started_at: 1,
            updated_at: 1,
        }
    }
}
