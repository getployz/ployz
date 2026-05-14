mod local;
mod responses;
mod rpc;
mod send;
mod transfer_execution;

use ployz_api::{
    DaemonPayload, DaemonResponse, VolumeZfsTransferListPayload, VolumeZfsTransferPayload,
};
use ployz_model::{MachineLifecycle, MachineMembership};
use ployz_spec::Namespace;
use ployz_store_api::MachineMembershipStore;
use ployz_volume_zfs::TransferStore;
#[cfg(test)]
use ployz_volume_zfs::{
    MoveClaimOutcome, TransferRecord, TransferStatus, move_claim_key, unique_transfer_id,
};

use crate::daemon::DaemonState;
use responses::transfer_info;
#[cfg(test)]
use transfer_execution::finalize_zfs_transfer;

impl DaemonState {
    fn zfs_transfer_store(&self) -> TransferStore {
        TransferStore::new(self.data_dir.clone())
    }

    pub(crate) async fn recover_zfs_transfers_on_startup(&self) {
        match self.zfs_transfer_store().recover_startup() {
            Ok(count) if count > 0 => {
                tracing::warn!(count, "marked running zfs transfers interrupted")
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "failed to recover zfs transfer startup state"),
        }
    }

    pub(crate) async fn handle_volume_zfs_inspect(
        &self,
        namespace: &str,
        volume: &str,
        machine: Option<&str>,
    ) -> DaemonResponse {
        let namespace = match Namespace::try_new(namespace) {
            Ok(namespace) => namespace,
            Err(error) => return self.err("VOLUME_ZFS_INSPECT_FAILED", error),
        };
        let target_machine: Option<String> = match machine {
            Some(machine) => Some(machine.to_string()),
            None => match self.volume_record(&namespace, volume).await {
                Ok(record) => Some(record.machine_id.as_str().to_string()),
                Err(error) => return self.err("VOLUME_ZFS_INSPECT_FAILED", error),
            },
        };
        if let Some(machine) = target_machine
            && machine != self.identity.machine_id.as_str()
        {
            return self
                .forward_volume_zfs_inspect(&namespace.as_str(), volume, &machine)
                .await;
        }
        match self.inspect_local_volume_zfs(&namespace, volume).await {
            Ok(payload) => self.ok_with_payload(
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "ok".into()),
                Some(DaemonPayload::VolumeZfsInspect(payload)),
            ),
            Err(error) => self.err("VOLUME_ZFS_INSPECT_FAILED", error.to_string()),
        }
    }

    pub(crate) async fn handle_volume_zfs_snapshot(
        &self,
        namespace: &str,
        volume: &str,
        snapshot: &str,
    ) -> DaemonResponse {
        let namespace = match Namespace::try_new(namespace) {
            Ok(namespace) => namespace,
            Err(error) => return self.err("VOLUME_ZFS_SNAPSHOT_FAILED", error),
        };
        let record = match self.volume_record(&namespace, volume).await {
            Ok(record) => record,
            Err(error) => return self.err("VOLUME_ZFS_SNAPSHOT_FAILED", error),
        };
        if record.machine_id != self.identity.machine_id {
            return self
                .forward_volume_zfs_snapshot(&record.machine_id, &namespace, volume, snapshot)
                .await;
        }
        match self
            .snapshot_local_volume_zfs(&namespace, volume, snapshot)
            .await
        {
            Ok(payload) => self.ok_with_payload(
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "ok".into()),
                Some(DaemonPayload::VolumeZfsSnapshot(payload)),
            ),
            Err(error) => self.err("VOLUME_ZFS_SNAPSHOT_FAILED", error.to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn handle_deploy_node_clone_volume(
        &self,
        namespace: &str,
        deploy_id: &str,
        volume: &str,
        source_namespace: &str,
        source_volume: &str,
        snapshot: &str,
        quota: &str,
        mode: &str,
        owner: &str,
    ) -> DaemonResponse {
        let namespace = Namespace::new(namespace.to_string());
        let source_namespace = Namespace::new(source_namespace.to_string());
        match self
            .clone_local_volume_zfs(
                &namespace,
                deploy_id,
                volume,
                &source_namespace,
                source_volume,
                snapshot,
                quota,
                mode,
                owner,
            )
            .await
        {
            Ok(payload) => self.ok_with_payload(
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "ok".into()),
                Some(DaemonPayload::VolumeZfsClone(payload)),
            ),
            Err(error) => self.err("VOLUME_ZFS_CLONE_FAILED", error),
        }
    }

    pub(crate) async fn handle_deploy_node_cleanup_uncommitted_volume_clone(
        &self,
        namespace: &str,
        deploy_id: &str,
        volume: &str,
        source_namespace: &str,
        source_volume: &str,
        snapshot: &str,
    ) -> DaemonResponse {
        let namespace = Namespace::new(namespace.to_string());
        let source_namespace = Namespace::new(source_namespace.to_string());
        match self
            .cleanup_uncommitted_local_volume_clone_zfs(
                &namespace,
                deploy_id,
                volume,
                &source_namespace,
                source_volume,
                snapshot,
            )
            .await
        {
            Ok(()) => self.ok("uncommitted volume clone cleaned up"),
            Err(error) => self.err("VOLUME_ZFS_CLONE_CLEANUP_FAILED", error),
        }
    }

    pub(crate) async fn handle_volume_zfs_transfer_get(&self, id: &str) -> DaemonResponse {
        match self.zfs_transfer_store().load(id) {
            Ok(Some(record)) => self.ok_with_payload(
                serde_json::to_string_pretty(&transfer_info(&record))
                    .unwrap_or_else(|_| id.to_string()),
                Some(DaemonPayload::VolumeZfsTransfer(VolumeZfsTransferPayload {
                    transfer: transfer_info(&record),
                })),
            ),
            Ok(None) => self.err(
                "VOLUME_ZFS_TRANSFER_NOT_FOUND",
                format!("zfs transfer '{id}' not found"),
            ),
            Err(error) => self.err("VOLUME_ZFS_TRANSFER_GET_FAILED", error),
        }
    }

    pub(crate) async fn handle_volume_zfs_transfer_list(&self) -> DaemonResponse {
        match self.zfs_transfer_store().list() {
            Ok(records) => {
                let payload = VolumeZfsTransferListPayload {
                    transfers: records.iter().map(transfer_info).collect(),
                };
                self.ok_with_payload(
                    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "ok".into()),
                    Some(DaemonPayload::VolumeZfsTransferList(payload)),
                )
            }
            Err(error) => self.err("VOLUME_ZFS_TRANSFER_LIST_FAILED", error),
        }
    }

    async fn find_machine(&self, machine: &str) -> Option<ployz_model::MachineMembership> {
        let active = self.active.as_ref()?;
        let machines = active.mesh.store.list_machines().await.ok()?;
        machines
            .into_iter()
            .find(|record| record.id.as_str() == machine)
    }

    async fn find_active_machine(&self, machine: &str) -> Result<MachineMembership, String> {
        let record = self
            .find_machine(machine)
            .await
            .ok_or_else(|| format!("machine '{machine}' not found"))?;
        if record.lifecycle != MachineLifecycle::Active {
            return Err(format!(
                "machine '{}' is {}, expected active",
                record.id, record.lifecycle
            ));
        }
        Ok(record)
    }

    async fn find_volume_move_source_machine(
        &self,
        machine: &str,
    ) -> Result<MachineMembership, String> {
        let record = self
            .find_machine(machine)
            .await
            .ok_or_else(|| format!("machine '{machine}' not found"))?;
        if !matches!(
            record.lifecycle,
            MachineLifecycle::Active | MachineLifecycle::Draining
        ) {
            return Err(format!(
                "machine '{}' is {}, expected active or draining",
                record.id, record.lifecycle
            ));
        }
        Ok(record)
    }
}

fn volume_dataset(root: &str, namespace: &Namespace, volume: &str) -> String {
    format!("{root}/{}/{}", namespace.as_str(), volume)
}

#[cfg(test)]
mod tests {
    use super::{MoveClaimOutcome, TransferStatus, TransferStore, finalize_zfs_transfer};
    use crate::daemon::DaemonState;
    use ployz_api::VolumeZfsTransferState;
    use ployz_model::MachineId;
    use ployz_runtime_api::Identity;
    use ployz_spec::Namespace;
    use ployz_time::now_unix_secs;
    use ployz_volume_zfs::{ClaimedTransfer, wait_for_claimed_transfer_record};
    use std::path::PathBuf;
    use std::time::Duration;

    fn tmp_root(label: &str) -> PathBuf {
        let id = super::unique_transfer_id(0).expect("unique id");
        std::env::temp_dir().join(format!("ployz-zfs-transfer-test-{label}-{id}"))
    }

    fn begin(store: &TransferStore) -> super::TransferRecord {
        store
            .begin(
                &Namespace::new("default"),
                "data",
                MachineId::new("source"),
                MachineId::new("target"),
                "snap".into(),
                None,
            )
            .expect("begin transfer")
    }

    fn create_claim(store: &TransferStore, transfer_id: &str) -> MoveClaimOutcome {
        store
            .create_move_claim(
                &Namespace::new("default"),
                "data",
                &MachineId::new("source"),
                &MachineId::new("target"),
                "snap",
                None,
                transfer_id,
            )
            .expect("create claim")
    }

    fn claim_path(store: &TransferStore) -> PathBuf {
        let key = super::move_claim_key(
            &Namespace::new("default"),
            "data",
            &MachineId::new("source"),
            &MachineId::new("target"),
            "snap",
            None,
        );
        store.claim_path(&key)
    }

    fn make_state(label: &str) -> DaemonState {
        let data_dir = tmp_root(label);
        let identity = Identity::generate(MachineId::new("founder"), [42; 32]);
        DaemonState::new_for_tests(
            &data_dir,
            identity,
            "10.210.0.0/16".into(),
            24,
            4319,
            "127.0.0.1:0".into(),
            None,
            1,
        )
    }

    #[tokio::test]
    async fn peer_snapshot_rejects_invalid_namespace() {
        let state = make_state("peer-snapshot-invalid-namespace");

        let response = state
            .handle_volume_zfs_peer_snapshot("Prod", "data", "snap")
            .await;

        assert!(!response.is_ok());
        assert_eq!(response.code(), "VOLUME_ZFS_PEER_SNAPSHOT_FAILED");
        assert!(
            response.message().contains("namespace"),
            "got: {}",
            response.message()
        );
    }

    #[tokio::test]
    async fn peer_snapshot_guid_rejects_invalid_namespace() {
        let state = make_state("peer-snapshot-guid-invalid-namespace");

        let response = state
            .handle_volume_zfs_peer_snapshot_guid("bad ns", "data", "snap")
            .await;

        assert!(!response.is_ok());
        assert_eq!(response.code(), "VOLUME_ZFS_PEER_SNAPSHOT_GUID_FAILED");
        assert!(
            response.message().contains("namespace"),
            "got: {}",
            response.message()
        );
    }

    #[tokio::test]
    async fn peer_start_send_rejects_invalid_namespace() {
        let state = make_state("peer-start-send-invalid-namespace");

        let response = state
            .handle_volume_zfs_peer_start_send("bad ns", "data", "snap", "target", 1, None, None)
            .await;

        assert!(!response.is_ok());
        assert_eq!(response.code(), "VOLUME_ZFS_PEER_START_SEND_FAILED");
        assert!(
            response.message().contains("namespace"),
            "got: {}",
            response.message()
        );
    }

    #[test]
    fn startup_recovery_marks_running_transfers_interrupted() {
        let root = tmp_root("startup-recovery");
        let store = TransferStore::new(root.clone());
        let transfer = begin(&store);
        assert!(matches!(
            create_claim(&store, &transfer.id),
            MoveClaimOutcome::Created
        ));
        assert_eq!(transfer.status(), TransferStatus::Running);

        let count = store.recover_startup().expect("recover startup");
        assert_eq!(count, 1);
        let loaded = store
            .load(&transfer.id)
            .expect("load")
            .expect("record exists");
        assert_eq!(loaded.status(), TransferStatus::Interrupted);
        assert!(
            !claim_path(&store).exists(),
            "startup recovery should delete the interrupted transfer claim"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn move_claim_created_then_existing_returns_same_transfer_id() {
        let root = tmp_root("claim-idempotent");
        let store = TransferStore::new(root.clone());

        assert!(matches!(
            create_claim(&store, "transfer-a"),
            MoveClaimOutcome::Created
        ));
        match create_claim(&store, "transfer-b") {
            MoveClaimOutcome::Exists(existing_id) => assert_eq!(existing_id, "transfer-a"),
            MoveClaimOutcome::Created => panic!("claim should already exist"),
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn deleting_move_claim_allows_reclaim_after_stale_claim() {
        let root = tmp_root("claim-delete");
        let store = TransferStore::new(root.clone());

        assert!(matches!(
            create_claim(&store, "missing-transfer"),
            MoveClaimOutcome::Created
        ));
        store.delete_move_claim(
            &Namespace::new("default"),
            "data",
            &MachineId::new("source"),
            &MachineId::new("target"),
            "snap",
            None,
        );
        assert!(matches!(
            create_claim(&store, "transfer-b"),
            MoveClaimOutcome::Created
        ));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn deleting_move_claim_allows_reclaim_after_invalid_claim() {
        let root = tmp_root("claim-invalid");
        let store = TransferStore::new(root.clone());
        std::fs::create_dir_all(store.claim_dir()).expect("claim dir");
        std::fs::write(claim_path(&store), "\n").expect("invalid claim");

        match create_claim(&store, "transfer-a") {
            MoveClaimOutcome::Exists(existing_id) => assert!(existing_id.is_empty()),
            MoveClaimOutcome::Created => panic!("claim should already exist"),
        }
        store.delete_move_claim(
            &Namespace::new("default"),
            "data",
            &MachineId::new("source"),
            &MachineId::new("target"),
            "snap",
            None,
        );
        assert!(matches!(
            create_claim(&store, "transfer-b"),
            MoveClaimOutcome::Created
        ));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn claimed_transfer_waits_for_record_creation() {
        let root = tmp_root("claim-waits-for-record");
        let store = TransferStore::new(root.clone());

        assert!(matches!(
            create_claim(&store, "transfer-a"),
            MoveClaimOutcome::Created
        ));

        let delayed_store = store.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            delayed_store
                .begin_with_id(
                    "transfer-a".into(),
                    &Namespace::new("default"),
                    "data",
                    MachineId::new("source"),
                    MachineId::new("target"),
                    "snap".into(),
                    None,
                    now_unix_secs(),
                )
                .expect("begin delayed transfer");
        });

        match wait_for_claimed_transfer_record(&store, "transfer-a")
            .await
            .expect("wait for claimed transfer")
        {
            ClaimedTransfer::Running(record) => assert_eq!(record.id, "transfer-a"),
            ClaimedTransfer::Terminal(record) => {
                panic!("expected running transfer, got {:?}", record.status())
            }
            ClaimedTransfer::MissingAfterWait => {
                panic!("claim should wait for newly created transfer record")
            }
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn load_rejects_path_segment_id() {
        let store = TransferStore::new(tmp_root("reject-id"));
        for bad in ["../etc/passwd", "foo/bar", "with space", "", "."] {
            let err = store
                .load(bad)
                .expect_err(&format!("id '{bad}' should be rejected"));
            assert!(err.contains("transfer id"), "got: {err}");
        }
    }

    #[test]
    fn list_orders_by_started_at_desc_then_id_asc() {
        let root = tmp_root("list-order");
        let store = TransferStore::new(root.clone());
        let mut a = begin(&store);
        let mut b = begin(&store);
        let mut c = begin(&store);
        // begin uses nanos in the id, so ids grow with call order
        assert!(a.id < b.id && b.id < c.id);

        a.started_at = 100;
        b.started_at = 200;
        c.started_at = 200;
        store.save(&a).expect("save a");
        store.save(&b).expect("save b");
        store.save(&c).expect("save c");

        let listed = store.list().expect("list");
        let ids: Vec<&str> = listed.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec![b.id.as_str(), c.id.as_str(), a.id.as_str()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reusable_transfer_returns_running_matching_move() {
        let root = tmp_root("reusable");
        let store = TransferStore::new(root.clone());
        let transfer = begin(&store);

        let found = store
            .find_reusable(
                &Namespace::new("default"),
                "data",
                &MachineId::new("source"),
                &MachineId::new("target"),
                "snap",
                None,
            )
            .expect("find reusable")
            .expect("running transfer reusable");

        assert_eq!(found.id, transfer.id);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reusable_transfer_ignores_terminal_matching_move() {
        let root = tmp_root("terminal-not-reusable");
        let store = TransferStore::new(root.clone());
        let mut transfer = begin(&store);
        for status in [TransferStatus::Succeeded, TransferStatus::Failed] {
            store
                .update_status(&mut transfer, status, Some("boom".into()))
                .expect("terminal transfer");

            let found = store
                .find_reusable(
                    &Namespace::new("default"),
                    "data",
                    &MachineId::new("source"),
                    &MachineId::new("target"),
                    "snap",
                    None,
                )
                .expect("find reusable");

            assert!(found.is_none());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finalize_records_complete_stage_and_succeeded_status() {
        let root = tmp_root("finalize-ok");
        let store = TransferStore::new(root.clone());
        let mut transfer = begin(&store);
        store
            .update_stage(&mut transfer, "snapshot")
            .expect("stage snapshot");
        store
            .update_stage(&mut transfer, "send")
            .expect("stage send");

        finalize_zfs_transfer(&store, &mut transfer, Ok(()));

        let loaded = store
            .load(&transfer.id)
            .expect("load")
            .expect("record exists");
        assert_eq!(loaded.status(), TransferStatus::Succeeded);
        assert_eq!(loaded.stage(), "complete");
        assert!(loaded.last_error().is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finalize_captures_last_error_on_failure() {
        let root = tmp_root("finalize-err");
        let store = TransferStore::new(root.clone());
        let mut transfer = begin(&store);
        assert!(matches!(
            create_claim(&store, &transfer.id),
            MoveClaimOutcome::Created
        ));

        finalize_zfs_transfer(&store, &mut transfer, Err("boom".into()));

        let loaded = store
            .load(&transfer.id)
            .expect("load")
            .expect("record exists");
        assert_eq!(loaded.status(), TransferStatus::Failed);
        assert_eq!(loaded.last_error(), Some("boom"));
        assert!(
            !claim_path(&store).exists(),
            "failed transfer should delete its move claim"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_transfer_preserves_prior_failure_error() {
        let root = tmp_root("interrupt-after-failure");
        let store = TransferStore::new(root.clone());
        let mut transfer = begin(&store);
        store
            .update_status(
                &mut transfer,
                TransferStatus::Failed,
                Some("send failed".into()),
            )
            .expect("record transfer failure");

        store
            .update_status(&mut transfer, TransferStatus::Interrupted, None)
            .expect("record interruption");

        let loaded = store
            .load(&transfer.id)
            .expect("load")
            .expect("record exists");
        assert_eq!(loaded.status(), TransferStatus::Interrupted);
        assert_eq!(loaded.last_error(), Some("send failed"));
        assert!(
            matches!(
                super::transfer_info(&loaded).state,
                VolumeZfsTransferState::Interrupted {
                    last_error: Some(ref error),
                    ..
                } if error == "send failed"
            ),
            "operator-facing payload should preserve the failure audience"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
