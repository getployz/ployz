use std::time::Duration;

use ployz_api::{DaemonPayload, DaemonResponse, VolumeZfsTransferPayload};
use ployz_node_runtime::{NodeRpcPolicy, VolumeZfsNodeClient};
use ployz_spec::{Namespace, VolumeScope};
use ployz_time::now_unix_secs;
use ployz_volume_zfs::{
    ClaimedTransfer, MoveClaimOutcome, validate_transfer_id, wait_for_claimed_transfer_record,
};

use super::responses::transfer_info;
use super::transfer_execution::{finalize_zfs_transfer, run_coordinated_zfs_transfer_inner};
use crate::daemon::DaemonState;
use crate::daemon::node_rpc::NatsVolumeZfsRpcTransport;

const ZFS_SEND_RPC_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

impl DaemonState {
    pub(crate) async fn handle_volume_zfs_send(
        &self,
        namespace: &str,
        volume: &str,
        snapshot: &str,
        target_machine: &str,
        from_snapshot: Option<&str>,
    ) -> DaemonResponse {
        let namespace = match Namespace::try_new(namespace) {
            Ok(namespace) => namespace,
            Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
        };
        let record = match self.volume_record(&namespace, volume).await {
            Ok(record) => record,
            Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
        };
        if record.scope != VolumeScope::Single {
            return self.err(
                "VOLUME_ZFS_SCOPE_NOT_SUPPORTED",
                format!(
                    "volume '{}/{}' has scope {:?}; only Single is supported in this build",
                    namespace.as_str(),
                    volume,
                    record.scope
                ),
            );
        }
        let source = match self
            .find_volume_move_source_machine(record.machine_id.as_str())
            .await
        {
            Ok(record) => record,
            Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
        };
        let target = match self.find_active_machine(target_machine).await {
            Ok(record) => record,
            Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
        };
        let local_driver =
            if source.id == self.identity.machine_id || target.id == self.identity.machine_id {
                match self.local_zfs_driver().await {
                    Ok(driver) => Some(driver),
                    Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
                }
            } else {
                None
            };
        let transfer_port = self.zfs_transfer_port;
        let needs_nats_rpc = source.id != self.identity.machine_id
            || (from_snapshot.is_some() && target.id != self.identity.machine_id);
        let volume_rpc = if needs_nats_rpc {
            match self.nats_node_rpc_client().await {
                Ok(client) => Some(
                    VolumeZfsNodeClient::new(NatsVolumeZfsRpcTransport::new(client)).with_policy(
                        NodeRpcPolicy {
                            timeout: ZFS_SEND_RPC_TIMEOUT,
                        },
                    ),
                ),
                Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
            }
        } else {
            None
        };

        let store = self.zfs_transfer_store();
        match store.find_reusable(
            &namespace,
            volume,
            &source.id,
            &target.id,
            snapshot,
            from_snapshot,
        ) {
            Ok(Some(transfer)) => {
                let info = transfer_info(&transfer);
                return self.ok_with_payload(
                    serde_json::to_string_pretty(&info).unwrap_or_else(|_| info.id.clone()),
                    Some(DaemonPayload::VolumeZfsTransfer(VolumeZfsTransferPayload {
                        transfer: info,
                    })),
                );
            }
            Ok(None) => {}
            Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
        }
        let transfer = {
            let mut transfer = None;
            for _ in 0..2 {
                let transfer_id = match store.new_transfer_id() {
                    Ok(transfer_id) => transfer_id,
                    Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
                };
                match store.create_move_claim(
                    &namespace,
                    volume,
                    &source.id,
                    &target.id,
                    snapshot,
                    from_snapshot,
                    &transfer_id,
                ) {
                    Ok(MoveClaimOutcome::Created) => {
                        match store.begin_with_id(
                            transfer_id,
                            &namespace,
                            volume,
                            source.id.clone(),
                            target.id.clone(),
                            snapshot.to_string(),
                            from_snapshot.map(str::to_string),
                            now_unix_secs(),
                        ) {
                            Ok(record) => {
                                transfer = Some(record);
                                break;
                            }
                            Err(error) => {
                                store.delete_move_claim(
                                    &namespace,
                                    volume,
                                    &source.id,
                                    &target.id,
                                    snapshot,
                                    from_snapshot,
                                );
                                return self.err("VOLUME_ZFS_SEND_FAILED", error);
                            }
                        }
                    }
                    Ok(MoveClaimOutcome::Exists(existing_id)) => {
                        if let Err(error) = validate_transfer_id(&existing_id) {
                            tracing::warn!(
                                transfer_id = %existing_id,
                                %error,
                                "deleting stale zfs transfer claim with invalid transfer id"
                            );
                            store.delete_move_claim(
                                &namespace,
                                volume,
                                &source.id,
                                &target.id,
                                snapshot,
                                from_snapshot,
                            );
                            continue;
                        }
                        match wait_for_claimed_transfer_record(&store, &existing_id).await {
                            Ok(ClaimedTransfer::Running(existing)) => {
                                let info = transfer_info(&existing);
                                return self.ok_with_payload(
                                    serde_json::to_string_pretty(&info)
                                        .unwrap_or_else(|_| info.id.clone()),
                                    Some(DaemonPayload::VolumeZfsTransfer(
                                        VolumeZfsTransferPayload { transfer: info },
                                    )),
                                );
                            }
                            Ok(ClaimedTransfer::Terminal(existing)) => {
                                tracing::warn!(
                                    transfer_id = %existing.id,
                                    status = ?existing.status(),
                                    "deleting stale zfs transfer claim for terminal transfer"
                                );
                                store.delete_move_claim(
                                    &namespace,
                                    volume,
                                    &source.id,
                                    &target.id,
                                    snapshot,
                                    from_snapshot,
                                );
                            }
                            Ok(ClaimedTransfer::MissingAfterWait) => {
                                tracing::warn!(
                                    transfer_id = %existing_id,
                                    "deleting stale zfs transfer claim for missing transfer"
                                );
                                store.delete_move_claim(
                                    &namespace,
                                    volume,
                                    &source.id,
                                    &target.id,
                                    snapshot,
                                    from_snapshot,
                                );
                            }
                            Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
                        }
                    }
                    Err(error) => return self.err("VOLUME_ZFS_SEND_FAILED", error),
                }
            }
            match transfer {
                Some(transfer) => transfer,
                None => {
                    return self.err(
                        "VOLUME_ZFS_SEND_FAILED",
                        "zfs transfer claim could not be acquired after stale claim cleanup",
                    );
                }
            }
        };

        let info = transfer_info(&transfer);
        let task_store = store.clone();
        let task_record = record;
        let task_source = source;
        let task_target = target;
        let task_local_driver = local_driver;
        let task_local = self.identity.machine_id.clone();
        let task_snapshot = snapshot.to_string();
        let task_from = from_snapshot.map(str::to_string);
        let mut task_transfer = transfer;
        tokio::spawn(async move {
            let result = run_coordinated_zfs_transfer_inner(
                &task_store,
                &mut task_transfer,
                &task_record,
                &task_source,
                &task_target,
                task_local_driver.as_ref(),
                volume_rpc,
                transfer_port,
                &task_local,
                &task_snapshot,
                task_from.as_deref(),
            )
            .await;
            finalize_zfs_transfer(&task_store, &mut task_transfer, result);
        });

        self.ok_with_payload(
            serde_json::to_string_pretty(&info).unwrap_or_else(|_| info.id.clone()),
            Some(DaemonPayload::VolumeZfsTransfer(VolumeZfsTransferPayload {
                transfer: info,
            })),
        )
    }
}
