use std::time::Duration;

use ployz_model::{MachineId, MachineMembership, VolumeRecord};
use ployz_node_api::NodeVolumeZfsSnapshotPayload;
use ployz_node_runtime::{VolumeZfsNodeClient, VolumeZfsRpcTransport};
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
pub(super) async fn run_coordinated_zfs_transfer_inner<R>(
    store: &TransferStore,
    transfer: &mut TransferRecord,
    record: &VolumeRecord,
    source: &MachineMembership,
    target: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    volume_rpc: Option<VolumeZfsNodeClient<R>>,
    transfer_port: u16,
    local_machine_id: &MachineId,
    snapshot: &str,
    from_snapshot: Option<&str>,
) -> Result<(), String>
where
    R: VolumeZfsRpcTransport,
{
    store.update_stage(transfer, "snapshot")?;
    let snap_info = snapshot_on_machine(
        source,
        local_driver,
        volume_rpc.as_ref(),
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
            volume_rpc.as_ref(),
            local_machine_id,
            &record.namespace,
            &record.volume_name,
            from_snapshot,
        )
        .await?;
        let target_from_guid = snapshot_guid_on_machine(
            target,
            local_driver,
            volume_rpc.as_ref(),
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
        volume_rpc.as_ref(),
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

async fn snapshot_on_machine<R>(
    machine: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    volume_rpc: Option<&VolumeZfsNodeClient<R>>,
    local_machine_id: &MachineId,
    namespace: &Namespace,
    volume: &str,
    snapshot: &str,
) -> Result<NodeVolumeZfsSnapshotPayload, String>
where
    R: VolumeZfsRpcTransport,
{
    if machine.id == *local_machine_id {
        let driver =
            local_driver.ok_or_else(|| "local zfs driver is not configured".to_string())?;
        let dataset = super::volume_dataset(driver.root_dataset(), namespace, volume);
        let snap_info = driver
            .create_snapshot(&dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(NodeVolumeZfsSnapshotPayload {
            namespace: namespace.as_str().to_string(),
            volume: volume.to_string(),
            machine_id: machine.id.clone(),
            dataset,
            snapshot: snap_info.name,
            guid: snap_info.guid,
        });
    }

    let volume_rpc = volume_rpc.ok_or_else(|| "volume RPC client is not configured".to_string())?;
    volume_rpc
        .peer_snapshot(&machine.id, namespace.as_str(), volume, snapshot)
        .await
        .map_err(|error| error.to_string())
}

async fn snapshot_guid_on_machine<R>(
    machine: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    volume_rpc: Option<&VolumeZfsNodeClient<R>>,
    local_machine_id: &MachineId,
    namespace: &Namespace,
    volume: &str,
    snapshot: &str,
) -> Result<NodeVolumeZfsSnapshotPayload, String>
where
    R: VolumeZfsRpcTransport,
{
    if machine.id == *local_machine_id {
        let driver =
            local_driver.ok_or_else(|| "local zfs driver is not configured".to_string())?;
        let dataset = super::volume_dataset(driver.root_dataset(), namespace, volume);
        let guid = driver
            .snapshot_guid(&dataset, snapshot)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(NodeVolumeZfsSnapshotPayload {
            namespace: namespace.as_str().to_string(),
            volume: volume.to_string(),
            machine_id: machine.id.clone(),
            dataset,
            snapshot: snapshot.to_string(),
            guid,
        });
    }

    let volume_rpc = volume_rpc.ok_or_else(|| "volume RPC client is not configured".to_string())?;
    volume_rpc
        .peer_snapshot_guid(&machine.id, namespace.as_str(), volume, snapshot)
        .await
        .map_err(|error| error.to_string())
}

#[allow(clippy::too_many_arguments)]
async fn start_send_on_machine<R>(
    source: &MachineMembership,
    target: &MachineMembership,
    local_driver: Option<&ZfsDriver<TokioShellRunner>>,
    volume_rpc: Option<&VolumeZfsNodeClient<R>>,
    local_machine_id: &MachineId,
    transfer_port: u16,
    record: &VolumeRecord,
    snapshot: &str,
    expected_guid: u64,
    from_snapshot: Option<&str>,
    from_snapshot_guid: Option<u64>,
) -> Result<SendResult, String>
where
    R: VolumeZfsRpcTransport,
{
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

    let volume_rpc = volume_rpc.ok_or_else(|| "volume RPC client is not configured".to_string())?;
    let payload = volume_rpc
        .peer_start_send(
            &source.id,
            record.namespace.as_str(),
            &record.volume_name,
            snapshot,
            &target.id,
            expected_guid,
            from_snapshot,
            from_snapshot_guid,
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(SendResult {
        bytes_transferred: payload.bytes_transferred,
        snapshot_guid: payload.snapshot_guid,
    })
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    use async_trait::async_trait;
    use ployz_model::{DeployId, MachineLifecycle, OverlayIp, PublicKey, VolumeScope};
    use ployz_node_api::{
        NodeRequest, NodeResponse, VOLUME_ZFS_PEER_SEND_PAYLOAD_KIND,
        VOLUME_ZFS_SNAPSHOT_PAYLOAD_KIND,
    };
    use ployz_node_runtime::{NodeRpcError, NodeRpcPolicy, VolumeZfsRpcOperation};
    use ployz_volume_zfs::TransferState;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeVolumeRpc {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, VolumeZfsRpcOperation, NodeRequest)>>>,
    }

    impl FakeVolumeRpc {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn requests(&self) -> Vec<(MachineId, VolumeZfsRpcOperation, NodeRequest)> {
            self.requests.lock().expect("requests").clone()
        }
    }

    #[async_trait]
    impl VolumeZfsRpcTransport for FakeVolumeRpc {
        fn with_node_rpc_policy(&self, _policy: NodeRpcPolicy) -> Self {
            self.clone()
        }

        async fn volume_zfs_request(
            &self,
            machine_id: &MachineId,
            operation: VolumeZfsRpcOperation,
            request: &NodeRequest,
        ) -> Result<NodeResponse, NodeRpcError> {
            self.requests.lock().expect("requests").push((
                machine_id.clone(),
                operation,
                request.clone(),
            ));
            self.responses
                .lock()
                .expect("responses")
                .pop_front()
                .expect("fake response queued")
        }
    }

    #[tokio::test]
    async fn coordinated_transfer_uses_remote_volume_rpc_for_source_and_target() {
        let root = tmp_root("remote-success");
        let store = TransferStore::new(root.clone());
        let namespace = Namespace::new("prod");
        let record = volume_record(&namespace);
        let source = machine("machine-a", "fd00::a");
        let target = machine("machine-b", "fd00::b");
        let mut transfer = store
            .begin_with_id(
                "transfer-remote".into(),
                &namespace,
                "data",
                source.id.clone(),
                target.id.clone(),
                "snap".into(),
                Some("base".into()),
                1,
            )
            .expect("begin transfer");
        let transport = FakeVolumeRpc::with_responses(vec![
            Ok(snapshot_response("machine-a", "snap", 42)),
            Ok(snapshot_response("machine-a", "base", 24)),
            Ok(snapshot_response("machine-b", "base", 24)),
            Ok(peer_send_response(4096, 42)),
        ]);

        run_coordinated_zfs_transfer_inner(
            &store,
            &mut transfer,
            &record,
            &source,
            &target,
            None,
            Some(VolumeZfsNodeClient::new(transport.clone())),
            4319,
            &MachineId::new("local"),
            "snap",
            Some("base"),
        )
        .await
        .expect("remote transfer should succeed");

        assert_eq!(transfer.snapshot_guid(), Some(42));
        assert_eq!(transfer.from_snapshot_guid(), Some(24));
        assert!(matches!(
            transfer.state,
            TransferState::Running {
                ref stage,
                bytes_transferred: Some(4096),
                ..
            } if stage == "verify"
        ));

        let requests = transport.requests();
        let [snapshot, source_guid, target_guid, start_send] = requests.as_slice() else {
            panic!("expected four remote RPC requests, got {requests:?}");
        };
        assert_snapshot_request(
            snapshot,
            "machine-a",
            VolumeZfsRpcOperation::PeerSnapshot,
            "snap",
        );
        assert_snapshot_request(
            source_guid,
            "machine-a",
            VolumeZfsRpcOperation::PeerSnapshotGuid,
            "base",
        );
        assert_snapshot_request(
            target_guid,
            "machine-b",
            VolumeZfsRpcOperation::PeerSnapshotGuid,
            "base",
        );
        let (machine_id, operation, request) = start_send;
        assert_eq!(machine_id, &MachineId::new("machine-a"));
        assert_eq!(*operation, VolumeZfsRpcOperation::PeerStartSend);
        assert!(matches!(
            request,
            NodeRequest::VolumeZfsPeerStartSend {
                namespace,
                volume,
                snapshot,
                target_machine,
                expected_guid: 42,
                from_snapshot: Some(from_snapshot),
                from_snapshot_guid: Some(24),
            } if namespace == "prod"
                && volume == "data"
                && snapshot == "snap"
                && target_machine == "machine-b"
                && from_snapshot == "base"
        ));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn coordinated_transfer_stops_when_target_base_guid_differs() {
        let root = tmp_root("remote-base-mismatch");
        let store = TransferStore::new(root.clone());
        let namespace = Namespace::new("prod");
        let record = volume_record(&namespace);
        let source = machine("machine-a", "fd00::a");
        let target = machine("machine-b", "fd00::b");
        let mut transfer = store
            .begin_with_id(
                "transfer-mismatch".into(),
                &namespace,
                "data",
                source.id.clone(),
                target.id.clone(),
                "snap".into(),
                Some("base".into()),
                1,
            )
            .expect("begin transfer");
        let transport = FakeVolumeRpc::with_responses(vec![
            Ok(snapshot_response("machine-a", "snap", 42)),
            Ok(snapshot_response("machine-a", "base", 24)),
            Ok(snapshot_response("machine-b", "base", 25)),
        ]);

        let error = run_coordinated_zfs_transfer_inner(
            &store,
            &mut transfer,
            &record,
            &source,
            &target,
            None,
            Some(VolumeZfsNodeClient::new(transport.clone())),
            4319,
            &MachineId::new("local"),
            "snap",
            Some("base"),
        )
        .await
        .expect_err("base guid mismatch should fail");

        assert!(
            error.contains("target base snapshot guid 25 did not match source 24"),
            "got: {error}"
        );
        assert_eq!(transfer.from_snapshot_guid(), None);
        assert_eq!(transport.requests().len(), 3);

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn coordinated_transfer_surfaces_missing_remote_payload() {
        let root = tmp_root("remote-missing-payload");
        let store = TransferStore::new(root.clone());
        let namespace = Namespace::new("prod");
        let record = volume_record(&namespace);
        let source = machine("machine-a", "fd00::a");
        let target = machine("machine-b", "fd00::b");
        let mut transfer = store
            .begin_with_id(
                "transfer-missing".into(),
                &namespace,
                "data",
                source.id.clone(),
                target.id.clone(),
                "snap".into(),
                None,
                1,
            )
            .expect("begin transfer");
        let transport = FakeVolumeRpc::with_responses(vec![Ok(NodeResponse::success("ok", None))]);

        let error = run_coordinated_zfs_transfer_inner(
            &store,
            &mut transfer,
            &record,
            &source,
            &target,
            None,
            Some(VolumeZfsNodeClient::new(transport.clone())),
            4319,
            &MachineId::new("local"),
            "snap",
            None,
        )
        .await
        .expect_err("missing remote payload should fail");

        assert!(error.contains("NODE_RPC_MISSING_PAYLOAD"), "got: {error}");
        assert_eq!(transport.requests().len(), 1);

        let _ = std::fs::remove_dir_all(root);
    }

    fn assert_snapshot_request(
        request: &(MachineId, VolumeZfsRpcOperation, NodeRequest),
        expected_machine: &str,
        expected_operation: VolumeZfsRpcOperation,
        expected_snapshot: &str,
    ) {
        let (machine_id, operation, node_request) = request;
        assert_eq!(machine_id, &MachineId::new(expected_machine));
        assert_eq!(*operation, expected_operation);
        match node_request {
            NodeRequest::VolumeZfsPeerSnapshot {
                namespace,
                volume,
                snapshot,
            }
            | NodeRequest::VolumeZfsPeerSnapshotGuid {
                namespace,
                volume,
                snapshot,
            } => {
                assert_eq!(namespace, "prod");
                assert_eq!(volume, "data");
                assert_eq!(snapshot, expected_snapshot);
            }
            other => panic!("expected snapshot request, got {other:?}"),
        }
    }

    fn snapshot_response(machine_id: &str, snapshot: &str, guid: u64) -> NodeResponse {
        NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": VOLUME_ZFS_SNAPSHOT_PAYLOAD_KIND,
                "namespace": "prod",
                "volume": "data",
                "machine_id": machine_id,
                "dataset": format!("pool/prod/data/{machine_id}"),
                "snapshot": snapshot,
                "guid": guid
            })),
        )
    }

    fn peer_send_response(bytes_transferred: u64, snapshot_guid: u64) -> NodeResponse {
        NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": VOLUME_ZFS_PEER_SEND_PAYLOAD_KIND,
                "bytes_transferred": bytes_transferred,
                "snapshot_guid": snapshot_guid
            })),
        )
    }

    fn tmp_root(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("ployz-zfs-transfer-execution-{label}-{nanos}"))
    }

    fn volume_record(namespace: &Namespace) -> VolumeRecord {
        VolumeRecord {
            namespace: namespace.clone(),
            volume_name: "data".into(),
            scope: VolumeScope::Single,
            machine_id: MachineId::new("machine-a"),
            quota: "10G".into(),
            mode: "0750".into(),
            owner: "999:999".into(),
            attached_services: vec!["db".into()],
            created_at: 1,
            created_by_deploy_id: DeployId::new("deploy-1"),
            last_modified_at: 1,
            last_modified_by_deploy_id: DeployId::new("deploy-1"),
        }
    }

    fn machine(id: &str, overlay: &str) -> MachineMembership {
        let mut machine = MachineMembership::seed(
            MachineId::new(id),
            PublicKey([1; 32]),
            OverlayIp(overlay.parse().expect("valid overlay")),
            None,
            Vec::new(),
        );
        machine.lifecycle = MachineLifecycle::Active;
        machine
    }
}
