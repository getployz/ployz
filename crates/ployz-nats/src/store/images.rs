use async_nats::jetstream::kv;
use async_trait::async_trait;
use ployz_store_api::ImageAvailabilityStore;
use ployz_types::Result;
use ployz_types::error::{Error, StoreRecordKind};
use ployz_types::model::{ImageAvailabilityRecord, ImageDigest, MachineId};

use crate::NatsStore;
use crate::store::kv_json;
use crate::subjects;

#[async_trait]
impl ImageAvailabilityStore for NatsStore {
    async fn upsert_image_availability(&self, record: &ImageAvailabilityRecord) -> Result<()> {
        let kv = image_availability_bucket(self).await?;
        kv_json::put_json(
            &kv,
            image_availability_key(&record.machine_id, &record.digest).as_str(),
            record,
            "nats_image_availability_encode",
            "nats_image_availability_put",
        )
        .await
    }

    async fn get_image_availability(
        &self,
        machine_id: &MachineId,
        digest: &ImageDigest,
    ) -> Result<Option<ImageAvailabilityRecord>> {
        let kv = image_availability_bucket(self).await?;
        let key = image_availability_key(machine_id, digest);
        let Some(entry) = kv.entry(key.clone()).await.map_err(|error| {
            Error::operation("nats_image_availability_get", format!("{error:?}"))
        })?
        else {
            return Ok(None);
        };
        if entry.operation != kv::Operation::Put {
            return Ok(None);
        }
        let record = decode_image_availability(&entry.key, entry.value.as_ref())?;
        Ok(Some(record))
    }

    async fn list_image_availability(&self) -> Result<Vec<ImageAvailabilityRecord>> {
        let kv = image_availability_bucket(self).await?;
        image_availability_snapshot(
            kv_json::list_json_entries::<ImageAvailabilityRecord>(
                &kv,
                "nats_image_availability_decode",
                "nats_image_availability_list",
            )
            .await?,
        )
    }
}

async fn image_availability_bucket(store: &NatsStore) -> Result<kv::Store> {
    kv_json::get_bucket(
        store.jetstream(),
        store.assets().image_availability_bucket.as_str(),
        "nats_image_availability_bucket",
    )
    .await
}

fn image_availability_snapshot(
    entries: Vec<kv_json::JsonEntry<ImageAvailabilityRecord>>,
) -> Result<Vec<ImageAvailabilityRecord>> {
    let mut records = Vec::with_capacity(entries.len());
    for entry in entries {
        records.push(validate_image_availability_key(&entry.key, entry.value)?);
    }
    records.sort_by(|left, right| {
        left.machine_id
            .cmp(&right.machine_id)
            .then(left.digest.cmp(&right.digest))
    });
    Ok(records)
}

fn decode_image_availability(key: &str, bytes: &[u8]) -> Result<ImageAvailabilityRecord> {
    let record: ImageAvailabilityRecord =
        kv_json::decode_json("nats_image_availability_decode", bytes)?;
    validate_image_availability_key(key, record)
}

fn validate_image_availability_key(
    key: &str,
    record: ImageAvailabilityRecord,
) -> Result<ImageAvailabilityRecord> {
    let payload_key = image_availability_key(&record.machine_id, &record.digest);
    if payload_key != key {
        return Err(Error::store_key_mismatch(
            StoreRecordKind::ImageAvailability,
            key,
            payload_key,
        ));
    }
    Ok(record)
}

fn image_availability_key(machine_id: &MachineId, digest: &ImageDigest) -> String {
    format!(
        "{}.{}",
        subjects::subject_token(machine_id.as_str()),
        subjects::subject_token(digest.0.replace(':', "_").as_str())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{ImageArtifact, ImageArtifactProvenance, ImagePresence, ImageRef};

    #[test]
    fn image_availability_key_mismatch_is_visible() {
        let record = image_record("machine-a");
        let bytes = serde_json::to_vec(&record).expect("encode record");

        let error = decode_image_availability("machine-b.sha256_aaaa", &bytes)
            .expect_err("key mismatch should fail visibly");

        assert!(matches!(
            error,
            Error::Store(ployz_types::error::StoreError::KeyMismatch {
                record: StoreRecordKind::ImageAvailability,
                ..
            })
        ));
    }

    #[test]
    fn image_availability_snapshot_orders_by_machine_and_digest() {
        let record_a = image_record("machine-a");
        let record_b = image_record("machine-b");
        let entries = vec![
            kv_json::JsonEntry {
                key: image_availability_key(&record_b.machine_id, &record_b.digest),
                value: record_b,
            },
            kv_json::JsonEntry {
                key: image_availability_key(&record_a.machine_id, &record_a.digest),
                value: record_a,
            },
        ];

        let snapshot = image_availability_snapshot(entries).expect("snapshot");

        assert_eq!(
            snapshot
                .iter()
                .map(|record| record.machine_id.as_str())
                .collect::<Vec<_>>(),
            ["machine-a", "machine-b"]
        );
    }

    fn image_record(machine_id: &str) -> ImageAvailabilityRecord {
        let digest = ImageDigest("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into());
        let artifact = ImageArtifact {
            image: ImageRef::digest_only(digest.clone()),
            platform: None,
            provenance: ImageArtifactProvenance::External { source: None },
            created_at: 1,
        };
        ImageAvailabilityRecord {
            machine_id: MachineId::new(machine_id),
            digest,
            presence: ImagePresence::Present {
                artifact,
                recorded_at: 1,
                source_operation_id: None,
            },
            updated_at: 1,
        }
    }
}
