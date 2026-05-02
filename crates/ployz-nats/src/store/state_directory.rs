use std::collections::HashMap;

use async_nats::jetstream::kv;
use ployz_types::error::{Error, Result};
use ployz_types::model::MachineId;
use ployz_types::time::now_unix_secs;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::NatsStore;
use crate::store::kv_json;
use crate::store::kv_watch;
use crate::subjects::{
    MACHINE_DIRECTORY_BUCKET, instance_state_stream, machine_state_stream,
};

/// Catalog entry for one machine's per-node state streams. Stored in the
/// hub-side `machine_directory` KV; mirrored to leaves via the standard
/// durable-KV mirror plumbing. Every node reads this directory to discover
/// which `state_machine_<id>` / `state_instances_<id>` streams should feed
/// its local view-stream `sources` set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineDirectoryEntry {
    pub machine_id: MachineId,
    pub machine_state_stream: String,
    pub instance_state_stream: String,
    pub created_at: u64,
}

impl MachineDirectoryEntry {
    /// Construct an entry with stream names derived from `machine_id`.
    /// Sole writer of the directory should be the coordinator handling
    /// `handle_machine_add`.
    #[must_use]
    pub fn for_machine(machine_id: MachineId) -> Self {
        let machine_state_stream = machine_state_stream(&machine_id);
        let instance_state_stream = instance_state_stream(&machine_id);
        Self {
            machine_id,
            machine_state_stream,
            instance_state_stream,
            created_at: now_unix_secs(),
        }
    }
}

/// Event emitted by the directory subscription. Mirrors the `MachineEvent`
/// shape but is scoped to directory entries (not the full
/// `MachineMembership` record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryEvent {
    Added(MachineDirectoryEntry),
    Updated(MachineDirectoryEntry),
    Removed(MachineId),
}

pub type DirectorySubscription = (
    Vec<MachineDirectoryEntry>,
    mpsc::Receiver<Result<DirectoryEvent>>,
);

/// Register a machine's state streams in the directory. Idempotent — a
/// re-register with the same `machine_id` overwrites the entry.
pub async fn register(store: &NatsStore, machine_id: &MachineId) -> Result<()> {
    let bucket = directory_write_bucket(store).await?;
    let entry = MachineDirectoryEntry::for_machine(machine_id.clone());
    kv_json::put_json(
        &bucket,
        &machine_id.0,
        &entry,
        "nats_machine_directory_encode",
        "nats_machine_directory_put",
    )
    .await
}

/// Remove a machine from the directory. The reconciler on every node will
/// observe this deletion and drop the corresponding source from its view
/// streams. Idempotent — removing an absent entry is not an error.
pub async fn deregister(store: &NatsStore, machine_id: &MachineId) -> Result<()> {
    let bucket = directory_write_bucket(store).await?;
    kv_json::delete(&bucket, &machine_id.0, "nats_machine_directory_delete").await
}

/// Read all directory entries from the local view (mirror on Mirrors,
/// hub on StorageCandidate).
pub async fn list(store: &NatsStore) -> Result<Vec<MachineDirectoryEntry>> {
    let bucket = directory_read_bucket(store).await?;
    kv_json::list_json(
        &bucket,
        "nats_machine_directory_decode",
        "nats_machine_directory_list",
    )
    .await
}

/// Snapshot + watch the directory. Mirrors the contract of
/// `subscribe_machines`: returns the initial snapshot synchronously and a
/// channel of subsequent events.
pub async fn subscribe(store: &NatsStore) -> Result<DirectorySubscription> {
    let bucket = directory_read_bucket(store).await?;
    let snapshot_boundary =
        kv_json::latest_sequence(&bucket, "nats_machine_directory_snapshot_boundary").await?;
    let snapshot = list(store).await?;
    kv_watch::subscribe_all(
        &bucket,
        snapshot,
        snapshot_boundary,
        |entry: &MachineDirectoryEntry| entry.machine_id.0.clone(),
        "nats_machine_directory_watch",
        "NATS machine_directory watcher failed",
        "NATS machine_directory event decode failed",
        directory_event_from_kv_entry,
    )
    .await
}

async fn directory_read_bucket(store: &NatsStore) -> Result<kv::Store> {
    kv_json::read_bucket_for(store, MACHINE_DIRECTORY_BUCKET, "nats_machine_directory_bucket")
        .await
}

async fn directory_write_bucket(store: &NatsStore) -> Result<kv::Store> {
    kv_json::write_bucket_for(store, MACHINE_DIRECTORY_BUCKET, "nats_machine_directory_bucket")
        .await
}

fn directory_event_from_kv_entry(
    last_seen: &mut HashMap<String, MachineDirectoryEntry>,
    key: &str,
    bytes: &[u8],
    operation: kv::Operation,
) -> Result<Option<DirectoryEvent>> {
    match operation {
        kv::Operation::Put => {
            let entry: MachineDirectoryEntry =
                kv_json::decode_json("nats_machine_directory_decode", bytes)?;
            if entry.machine_id.0 != key {
                return Err(Error::operation(
                    "nats_machine_directory_decode",
                    format!(
                        "directory key {key} does not match payload machine_id {}",
                        entry.machine_id.0
                    ),
                ));
            }
            let event = match last_seen.get(key) {
                Some(existing) if existing == &entry => return Ok(None),
                Some(_) => DirectoryEvent::Updated(entry.clone()),
                None => DirectoryEvent::Added(entry.clone()),
            };
            last_seen.insert(key.to_string(), entry);
            Ok(Some(event))
        }
        kv::Operation::Delete | kv::Operation::Purge => match last_seen.remove(key) {
            Some(removed) => Ok(Some(DirectoryEvent::Removed(removed.machine_id))),
            None => Ok(None),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_for_machine_uses_canonical_stream_names() {
        let id = MachineId("m1".into());
        let entry = MachineDirectoryEntry::for_machine(id.clone());
        assert_eq!(entry.machine_state_stream, machine_state_stream(&id));
        assert_eq!(entry.instance_state_stream, instance_state_stream(&id));
        assert_eq!(entry.machine_id, id);
    }

    #[test]
    fn directory_event_decode_emits_added_for_first_observation() {
        let mut last_seen = HashMap::new();
        let entry = MachineDirectoryEntry::for_machine(MachineId("m1".into()));
        let bytes = serde_json::to_vec(&entry).expect("encode entry");
        let event =
            directory_event_from_kv_entry(&mut last_seen, "m1", &bytes, kv::Operation::Put)
                .expect("decode event");
        assert_eq!(event, Some(DirectoryEvent::Added(entry.clone())));
        assert_eq!(last_seen.get("m1"), Some(&entry));
    }

    #[test]
    fn directory_event_decode_skips_identical_value() {
        let mut last_seen = HashMap::new();
        let entry = MachineDirectoryEntry::for_machine(MachineId("m1".into()));
        last_seen.insert("m1".into(), entry.clone());
        let bytes = serde_json::to_vec(&entry).expect("encode entry");
        let event =
            directory_event_from_kv_entry(&mut last_seen, "m1", &bytes, kv::Operation::Put)
                .expect("decode event");
        assert!(event.is_none());
    }

    #[test]
    fn directory_event_decode_emits_updated_for_changed_value() {
        let mut last_seen = HashMap::new();
        let original = MachineDirectoryEntry {
            machine_id: MachineId("m1".into()),
            machine_state_stream: "old".into(),
            instance_state_stream: "old_i".into(),
            created_at: 1,
        };
        last_seen.insert("m1".into(), original);
        let updated = MachineDirectoryEntry {
            machine_id: MachineId("m1".into()),
            machine_state_stream: "new".into(),
            instance_state_stream: "new_i".into(),
            created_at: 2,
        };
        let bytes = serde_json::to_vec(&updated).expect("encode entry");
        let event =
            directory_event_from_kv_entry(&mut last_seen, "m1", &bytes, kv::Operation::Put)
                .expect("decode event");
        assert_eq!(event, Some(DirectoryEvent::Updated(updated)));
    }

    #[test]
    fn directory_event_decode_emits_removed_on_delete() {
        let mut last_seen = HashMap::new();
        let entry = MachineDirectoryEntry::for_machine(MachineId("m1".into()));
        last_seen.insert("m1".into(), entry.clone());
        let event =
            directory_event_from_kv_entry(&mut last_seen, "m1", &[], kv::Operation::Delete)
                .expect("decode event");
        assert_eq!(event, Some(DirectoryEvent::Removed(entry.machine_id)));
        assert!(last_seen.is_empty());
    }

    #[test]
    fn directory_event_decode_delete_for_unknown_key_is_noop() {
        let mut last_seen = HashMap::new();
        let event =
            directory_event_from_kv_entry(&mut last_seen, "unknown", &[], kv::Operation::Delete)
                .expect("decode event");
        assert!(event.is_none());
    }

    #[test]
    fn directory_event_decode_rejects_key_payload_mismatch() {
        let mut last_seen = HashMap::new();
        let entry = MachineDirectoryEntry::for_machine(MachineId("m1".into()));
        let bytes = serde_json::to_vec(&entry).expect("encode entry");
        let error = directory_event_from_kv_entry(
            &mut last_seen,
            "different",
            &bytes,
            kv::Operation::Put,
        )
        .expect_err("mismatched payload is rejected");
        assert!(error.to_string().contains("does not match payload"));
    }
}
