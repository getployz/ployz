use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use ployz_model::{
    DeployId, MachineId, MachineLifecycle, MachineMembership, OverlayIp, PublicKey, VolumeRecord,
    VolumeScope,
};
use ployz_node_api::{
    NodeRequest, NodeResponse, VOLUME_ZFS_PEER_SEND_PAYLOAD_KIND, VOLUME_ZFS_SNAPSHOT_PAYLOAD_KIND,
};
use ployz_node_runtime::{
    NodeRpcError, NodeRpcPolicy, VolumeZfsNodeClient, VolumeZfsRpcOperation, VolumeZfsRpcTransport,
};
use ployz_spec::Namespace;
use ployz_volume_zfs::{TransferState, TransferStore};

use super::run_coordinated_zfs_transfer_inner;

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
