use ployz_api::ImageStatusRequest;
use ployz_model::ImageAvailabilityRecord;
use ployz_store_api::ImageAvailabilityStore;

pub async fn image_status_records(
    store: &dyn ImageAvailabilityStore,
    request: &ImageStatusRequest,
) -> Result<Vec<ImageAvailabilityRecord>, String> {
    match (&request.machine_id, &request.digest) {
        (Some(machine_id), Some(digest)) => Ok(store
            .get_image_availability(machine_id, digest)
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .collect()),
        (Some(machine_id), None) => Ok(store
            .list_image_availability()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|record| record.machine_id == *machine_id)
            .collect()),
        (None, Some(digest)) => Ok(store
            .list_image_availability()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|record| record.digest == *digest)
            .collect()),
        (None, None) => store
            .list_image_availability()
            .await
            .map_err(|error| error.to_string()),
    }
}

pub fn render_image_record_line(record: &ImageAvailabilityRecord) -> String {
    format!(
        "{}  {}  {}",
        record.machine_id.as_str(),
        record.digest.as_str(),
        image_presence_label(record)
    )
}

fn image_presence_label(record: &ImageAvailabilityRecord) -> &'static str {
    match record.presence {
        ployz_model::ImagePresence::Present { .. } => "present",
        ployz_model::ImagePresence::Absent { .. } => "absent",
        ployz_model::ImagePresence::Transferring { .. } => "transferring",
        ployz_model::ImagePresence::Failed { .. } => "failed",
    }
}

#[cfg(test)]
mod tests {
    use ployz_model::{
        BuildLocation, BuildMethod, ImageArtifact, ImageArtifactProvenance,
        ImageAvailabilityRecord, ImageDigest, ImagePresence, ImageRef, MachineId,
    };
    use ployz_store_api::ImageAvailabilityStore;
    use ployz_store_memory::MemoryStore;
    use ployz_time::now_unix_secs;

    use super::*;

    fn digest(hex: char) -> ImageDigest {
        ImageDigest::try_new(format!("sha256:{}", hex.to_string().repeat(64)))
            .expect("valid digest")
    }

    fn present_record(machine: &str, digest: ImageDigest) -> ImageAvailabilityRecord {
        let now = now_unix_secs();
        ImageAvailabilityRecord {
            machine_id: MachineId::new(machine),
            digest: digest.clone(),
            presence: ImagePresence::Present {
                artifact: ImageArtifact {
                    image: ImageRef::repository_digest(
                        "example/app",
                        Some("latest".into()),
                        digest,
                    ),
                    platform: None,
                    provenance: ImageArtifactProvenance::Build {
                        method: BuildMethod::Dockerfile,
                        location: BuildLocation::Local,
                        source_digest: None,
                    },
                    created_at: now,
                },
                recorded_at: now,
                source_operation_id: Some("op-1".into()),
            },
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn status_filters_by_machine_and_digest() {
        let store = MemoryStore::new();
        let digest_a = digest('a');
        let digest_b = digest('b');
        store
            .upsert_image_availability(&present_record("machine-a", digest_a.clone()))
            .await
            .expect("upsert a");
        store
            .upsert_image_availability(&present_record("machine-b", digest_b.clone()))
            .await
            .expect("upsert b");

        let records = image_status_records(
            &store,
            &ImageStatusRequest {
                digest: Some(digest_a.clone()),
                machine_id: Some(MachineId::new("machine-a")),
            },
        )
        .await
        .expect("status records");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].machine_id, MachineId::new("machine-a"));
        assert_eq!(records[0].digest, digest_a);
    }

    #[tokio::test]
    async fn status_lists_all_records_when_unfiltered() {
        let store = MemoryStore::new();
        store
            .upsert_image_availability(&present_record("machine-a", digest('a')))
            .await
            .expect("upsert a");
        store
            .upsert_image_availability(&present_record("machine-b", digest('b')))
            .await
            .expect("upsert b");

        let records = image_status_records(
            &store,
            &ImageStatusRequest {
                digest: None,
                machine_id: None,
            },
        )
        .await
        .expect("status records");
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn render_status_line_includes_machine_digest_and_presence() {
        let record = present_record("machine-a", digest('a'));
        let line = render_image_record_line(&record);

        assert!(line.contains("machine-a"));
        assert!(line.contains(record.digest.as_str()));
        assert!(line.contains("present"));
    }
}
