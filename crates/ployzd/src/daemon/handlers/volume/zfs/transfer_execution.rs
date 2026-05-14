use std::time::Duration;

use ployz_api::{DaemonPayload, DaemonResponse, VolumeZfsSnapshotPayload};
use ployz_model::{MachineId, MachineMembership, VolumeRecord};
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject};
use ployz_node_api::NodeRequest;
use ployz_spec::Namespace;
use ployz_volume_zfs::{
    SendResult, TokioShellRunner, TransferRecord, TransferStatus, TransferStore, ZfsDriver,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::daemon::handlers::volume::transfer_listener::{ZfsTransferOpen, ZfsTransferReceived};

const ACK_READ_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ACK_BYTES: usize = 16 * 1024;

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_coordinated_zfs_transfer_inner(
    store: &TransferStore,
    transfer: &mut TransferRecord,
    record: &VolumeRecord,
    source: &MachineMembership,
    target: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    nats_rpc: Option<NatsNodeRpcClient>,
    transfer_port: u16,
    local_machine_id: &MachineId,
    snapshot: &str,
    from_snapshot: Option<&str>,
) -> Result<(), String> {
    store.update_stage(transfer, "snapshot")?;
    let snap_info = snapshot_on_machine(
        source,
        local_driver,
        nats_rpc.as_ref(),
        local_machine_id,
        &record.namespace,
        &record.volume_name,
        snapshot,
    )
    .await?;
    transfer.state.with_snapshot_guid(snap_info.guid);
    store.save(transfer)?;

    if let Some(from_snapshot) = from_snapshot {
        store.update_stage(transfer, "verify-base")?;
        let from_guid = snapshot_guid_on_machine(
            source,
            local_driver,
            nats_rpc.as_ref(),
            local_machine_id,
            &record.namespace,
            &record.volume_name,
            from_snapshot,
        )
        .await?;
        let target_from_guid = snapshot_guid_on_machine(
            target,
            local_driver,
            nats_rpc.as_ref(),
            local_machine_id,
            &record.namespace,
            &record.volume_name,
            from_snapshot,
        )
        .await?;
        if target_from_guid.guid != from_guid.guid {
            return Err(format!(
                "target base snapshot guid {} did not match source {}",
                target_from_guid.guid, from_guid.guid
            ));
        }
        transfer.state.with_from_snapshot_guid(from_guid.guid);
        store.save(transfer)?;
    }

    store.update_stage(transfer, "send")?;
    let result = start_send_on_machine(
        source,
        target,
        local_driver,
        nats_rpc.as_ref(),
        local_machine_id,
        transfer_port,
        record,
        snapshot,
        snap_info.guid,
        from_snapshot,
        transfer.from_snapshot_guid(),
    )
    .await?;
    transfer
        .state
        .with_bytes_transferred(result.bytes_transferred);
    store.save(transfer)?;

    store.update_stage(transfer, "verify")?;
    if result.snapshot_guid != snap_info.guid {
        return Err(format!(
            "target snapshot guid {} did not match source {}",
            result.snapshot_guid, snap_info.guid
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn send_zfs_stream_from_local(
    record: &VolumeRecord,
    target: &MachineMembership,
    driver: &ZfsDriver<TokioShellRunner>,
    transfer_port: u16,
    local_machine_id: &MachineId,
    snapshot: &str,
    expected_guid: u64,
    from_snapshot: Option<&str>,
    from_snapshot_guid: Option<u64>,
) -> Result<SendResult, String> {
    let dataset = super::volume_dataset(
        driver.root_dataset(),
        &record.namespace,
        &record.volume_name,
    );
    let actual_guid = driver
        .snapshot_guid(&dataset, snapshot)
        .await
        .map_err(|err| err.to_string())?;
    if actual_guid != expected_guid {
        return Err(format!(
            "local snapshot '{dataset}@{snapshot}' guid {actual_guid} did not match expected {expected_guid}"
        ));
    }
    if let Some(from_snapshot) = from_snapshot
        && let Some(expected_from) = from_snapshot_guid
    {
        let actual_from = driver
            .snapshot_guid(&dataset, from_snapshot)
            .await
            .map_err(|err| err.to_string())?;
        if actual_from != expected_from {
            return Err(format!(
                "local base snapshot '{dataset}@{from_snapshot}' guid {actual_from} did not match expected {expected_from}"
            ));
        }
    }
    let address =
        std::net::SocketAddr::new(std::net::IpAddr::V6(target.overlay_ip.0), transfer_port);
    let stream = TcpStream::connect(address)
        .await
        .map_err(|err| format!("connect zfs transfer target {address}: {err}"))?;
    let (reader, mut writer) = stream.into_split();
    let open = ZfsTransferOpen {
        namespace: record.namespace.as_str().to_string(),
        volume: record.volume_name.clone(),
        snapshot: snapshot.to_string(),
        expected_guid,
        source_machine_id: Some(local_machine_id.clone()),
        from_snapshot: from_snapshot.map(str::to_string),
        from_snapshot_guid,
    };
    let mut header =
        serde_json::to_string(&open).map_err(|err| format!("encode transfer open: {err}"))?;
    header.push('\n');
    writer
        .write_all(header.as_bytes())
        .await
        .map_err(|err| format!("write transfer open: {err}"))?;

    let mut send = match from_snapshot {
        Some(from_snapshot) => driver
            .spawn_send_incremental(&dataset, from_snapshot, snapshot)
            .map_err(|err| err.to_string())?,
        None => driver
            .spawn_send_full(&dataset, snapshot)
            .map_err(|err| err.to_string())?,
    };
    let Some(mut stdout) = send.stdout.take() else {
        return Err("zfs send stdout was not piped".to_string());
    };
    let copy_result = tokio::io::copy(&mut stdout, &mut writer).await;
    drop(stdout);
    let bytes = match copy_result {
        Ok(bytes) => bytes,
        Err(err) => {
            let copy_error = format!("copy zfs send stream: {err}");
            // Always reap the child even if kill fails, so we never leave a
            // zombie behind on stream errors.
            if let Err(kill_err) = send.kill().await {
                tracing::warn!(error = %kill_err, "failed to kill zfs send after copy error");
            }
            if let Err(wait_err) = send.wait_with_output().await {
                return Err(format!("{copy_error}; failed to reap zfs send: {wait_err}"));
            }
            return Err(copy_error);
        }
    };
    writer
        .shutdown()
        .await
        .map_err(|err| format!("shutdown zfs transfer stream: {err}"))?;
    let output = send
        .wait_with_output()
        .await
        .map_err(|err| format!("wait for zfs send: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "zfs send failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let mut reader = BufReader::new(reader).take((MAX_ACK_BYTES + 1) as u64);
    let mut buf = Vec::new();
    let bytes_read = tokio::time::timeout(ACK_READ_TIMEOUT, reader.read_until(b'\n', &mut buf))
        .await
        .map_err(|_| "timed out waiting for zfs transfer response".to_string())?
        .map_err(|err| format!("read zfs transfer response: {err}"))?;
    if bytes_read == 0 {
        return Err("connection closed before zfs transfer response".to_string());
    }
    if buf.len() > MAX_ACK_BYTES {
        return Err(format!(
            "zfs transfer response exceeded {MAX_ACK_BYTES} bytes"
        ));
    }
    let response: ZfsTransferReceived = serde_json::from_slice(&buf)
        .map_err(|err| format!("decode zfs transfer response: {err}"))?;
    if !response.ok {
        return Err(response.message);
    }
    let Some(snapshot_guid) = response.snapshot_guid else {
        return Err("target did not report snapshot guid".to_string());
    };
    Ok(SendResult {
        bytes_transferred: bytes,
        snapshot_guid,
    })
}

async fn snapshot_on_machine(
    machine: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    nats_rpc: Option<&NatsNodeRpcClient>,
    local_machine_id: &MachineId,
    namespace: &Namespace,
    volume: &str,
    snapshot: &str,
) -> Result<VolumeZfsSnapshotPayload, String> {
    if machine.id == *local_machine_id {
        let driver =
            local_driver.ok_or_else(|| "local zfs driver is not configured".to_string())?;
        let dataset = super::volume_dataset(driver.root_dataset(), namespace, volume);
        let snap_info = driver
            .create_snapshot(&dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(VolumeZfsSnapshotPayload {
            namespace: namespace.as_str().to_string(),
            volume: volume.to_string(),
            machine_id: machine.id.clone(),
            dataset,
            snapshot: snap_info.name,
            guid: snap_info.guid,
        });
    }

    let nats_rpc = nats_rpc.ok_or_else(|| "NATS RPC client is not configured".to_string())?;
    let response = nats_rpc
        .request(
            NodeCommandSubject::volume_zfs_snapshot(&machine.id),
            &NodeRequest::VolumeZfsPeerSnapshot {
                namespace: namespace.as_str().to_string(),
                volume: volume.to_string(),
                snapshot: snapshot.to_string(),
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    expect_snapshot_payload(response, "remote peer snapshot")
}

async fn snapshot_guid_on_machine(
    machine: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    nats_rpc: Option<&NatsNodeRpcClient>,
    local_machine_id: &MachineId,
    namespace: &Namespace,
    volume: &str,
    snapshot: &str,
) -> Result<VolumeZfsSnapshotPayload, String> {
    if machine.id == *local_machine_id {
        let driver =
            local_driver.ok_or_else(|| "local zfs driver is not configured".to_string())?;
        let dataset = super::volume_dataset(driver.root_dataset(), namespace, volume);
        let guid = driver
            .snapshot_guid(&dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(VolumeZfsSnapshotPayload {
            namespace: namespace.as_str().to_string(),
            volume: volume.to_string(),
            machine_id: machine.id.clone(),
            dataset,
            snapshot: snapshot.to_string(),
            guid,
        });
    }

    let nats_rpc = nats_rpc.ok_or_else(|| "NATS RPC client is not configured".to_string())?;
    let response = nats_rpc
        .request(
            NodeCommandSubject::volume_zfs_snapshot_guid(&machine.id),
            &NodeRequest::VolumeZfsPeerSnapshotGuid {
                namespace: namespace.as_str().to_string(),
                volume: volume.to_string(),
                snapshot: snapshot.to_string(),
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    expect_snapshot_payload(response, "remote peer snapshot guid")
}

#[allow(clippy::too_many_arguments)]
async fn start_send_on_machine(
    source: &MachineMembership,
    target: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    nats_rpc: Option<&NatsNodeRpcClient>,
    local_machine_id: &MachineId,
    transfer_port: u16,
    record: &VolumeRecord,
    snapshot: &str,
    expected_guid: u64,
    from_snapshot: Option<&str>,
    from_snapshot_guid: Option<u64>,
) -> Result<SendResult, String> {
    if source.id == *local_machine_id {
        let driver =
            local_driver.ok_or_else(|| "local zfs driver is not configured".to_string())?;
        return send_zfs_stream_from_local(
            record,
            target,
            driver,
            transfer_port,
            local_machine_id,
            snapshot,
            expected_guid,
            from_snapshot,
            from_snapshot_guid,
        )
        .await;
    }

    let nats_rpc = nats_rpc.ok_or_else(|| "NATS RPC client is not configured".to_string())?;
    let response = nats_rpc
        .request(
            NodeCommandSubject::volume_zfs_start_send(&source.id),
            &NodeRequest::VolumeZfsPeerStartSend {
                namespace: record.namespace.as_str().to_string(),
                volume: record.volume_name.clone(),
                snapshot: snapshot.to_string(),
                target_machine: target.id.as_str().to_string(),
                expected_guid,
                from_snapshot: from_snapshot.map(str::to_string),
                from_snapshot_guid,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    if !response.is_ok() {
        return Err(format!(
            "remote peer start-send failed [{}]: {}",
            response.code(),
            response.message()
        ));
    }
    let Some(DaemonPayload::VolumeZfsPeerSend(payload)) = response.payload() else {
        return Err("remote peer start-send response missing payload".to_string());
    };
    Ok(SendResult {
        bytes_transferred: payload.bytes_transferred,
        snapshot_guid: payload.snapshot_guid,
    })
}

fn expect_snapshot_payload(
    response: DaemonResponse,
    operation: &str,
) -> Result<VolumeZfsSnapshotPayload, String> {
    if !response.is_ok() {
        return Err(format!(
            "{operation} failed [{}]: {}",
            response.code(),
            response.message()
        ));
    }
    let Some(DaemonPayload::VolumeZfsSnapshot(payload)) = response.payload() else {
        return Err(format!("{operation} response missing payload"));
    };
    Ok(payload)
}

pub(super) fn finalize_zfs_transfer(
    store: &TransferStore,
    transfer: &mut TransferRecord,
    result: Result<(), String>,
) {
    let transfer_id = transfer.id.clone();
    match result {
        Ok(()) => {
            if let Err(error) = store.update_stage(transfer, "complete") {
                tracing::warn!(%error, transfer_id, "failed to record complete stage");
            }
            if let Err(error) = store.update_status(transfer, TransferStatus::Succeeded, None) {
                tracing::warn!(%error, transfer_id, "failed to record success status");
            }
        }
        Err(error) => {
            if let Err(save_err) =
                store.update_status(transfer, TransferStatus::Failed, Some(error))
            {
                tracing::warn!(%save_err, transfer_id, "failed to record failed status");
            }
            store.delete_claim_for(transfer);
        }
    }
}
