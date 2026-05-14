use std::time::Duration;

use async_trait::async_trait;
use ployz_model::{DeployId, InstanceId, MachineId, SlotId};
use ployz_node_api::{
    DEPLOY_CANDIDATE_STARTED_PAYLOAD_KIND, DEPLOY_NAMESPACE_SNAPSHOT_PAYLOAD_KIND,
    NodeDeployCandidateStartedPayload, NodeDeployNamespaceSnapshotPayload, NodeRequest,
    NodeResponse, NodeVolumeZfsClonePayload, VOLUME_ZFS_CLONE_PAYLOAD_KIND,
};

use super::{NodeRpcError, NodeRpcPolicy, decode_typed_payload, ensure_success};

pub const DEPLOY_PARTICIPANT_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(10 * 60);
pub const DEPLOY_VOLUME_CLONE_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(2 * 60 * 60);
pub const DEPLOY_VOLUME_CLONE_CLEANUP_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(30 * 60);
pub const DEPLOY_VOLUME_MOVE_START_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(60);
pub const DEPLOY_VOLUME_MOVE_POLL_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(60);
pub const DEPLOY_VOLUME_MOVE_WAIT_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
pub const DEPLOY_VOLUME_MOVE_POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployRpcOperation {
    InspectNamespace,
    StartCandidate,
    CloneVolume,
    CleanupVolumeClone,
    DrainInstance,
    RemoveInstance,
}

impl DeployRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::InspectNamespace => "deploy_node_inspect",
            Self::StartCandidate => "deploy_node_start_candidate",
            Self::CloneVolume => "deploy_node_clone_volume",
            Self::CleanupVolumeClone => "deploy_node_cleanup_uncommitted_volume_clone",
            Self::DrainInstance => "deploy_node_drain",
            Self::RemoveInstance => "deploy_node_remove",
        }
    }
}

#[async_trait]
pub trait DeployRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn deploy_request(
        &self,
        machine_id: &MachineId,
        operation: DeployRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct DeployNodeClient<T> {
    transport: T,
}

impl<T> DeployNodeClient<T>
where
    T: DeployRpcTransport,
{
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    #[must_use]
    pub fn with_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            transport: self.transport.with_node_rpc_policy(policy),
        }
    }

    pub async fn inspect_namespace(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        deploy_id: &DeployId,
    ) -> Result<NodeDeployNamespaceSnapshotPayload, NodeRpcError> {
        self.request_typed(
            machine_id,
            DeployRpcOperation::InspectNamespace,
            &NodeRequest::DeployNodeInspectNamespace {
                namespace: namespace.to_string(),
                deploy_id: deploy_id.as_str().to_string(),
            },
            DEPLOY_NAMESPACE_SNAPSHOT_PAYLOAD_KIND,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start_candidate(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        deploy_id: &DeployId,
        service: &str,
        slot_id: &SlotId,
        instance_id: &InstanceId,
        spec_json: &str,
        volumes_json: &str,
    ) -> Result<NodeDeployCandidateStartedPayload, NodeRpcError> {
        self.request_typed(
            machine_id,
            DeployRpcOperation::StartCandidate,
            &NodeRequest::DeployNodeStartCandidate {
                namespace: namespace.to_string(),
                deploy_id: deploy_id.as_str().to_string(),
                service: service.to_string(),
                slot_id: slot_id.as_str().to_string(),
                instance_id: instance_id.as_str().to_string(),
                spec_json: spec_json.to_string(),
                volumes_json: volumes_json.to_string(),
            },
            DEPLOY_CANDIDATE_STARTED_PAYLOAD_KIND,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn clone_volume(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        deploy_id: &DeployId,
        volume: &str,
        source_namespace: &str,
        source_volume: &str,
        snapshot: &str,
        quota: &str,
        mode: &str,
        owner: &str,
    ) -> Result<NodeVolumeZfsClonePayload, NodeRpcError> {
        self.request_typed(
            machine_id,
            DeployRpcOperation::CloneVolume,
            &NodeRequest::DeployNodeCloneVolume {
                namespace: namespace.to_string(),
                deploy_id: deploy_id.as_str().to_string(),
                volume: volume.to_string(),
                source_namespace: source_namespace.to_string(),
                source_volume: source_volume.to_string(),
                snapshot: snapshot.to_string(),
                quota: quota.to_string(),
                mode: mode.to_string(),
                owner: owner.to_string(),
            },
            VOLUME_ZFS_CLONE_PAYLOAD_KIND,
        )
        .await
    }

    pub async fn cleanup_volume_clone(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        deploy_id: &DeployId,
        volume: &str,
        source_namespace: &str,
        source_volume: &str,
        snapshot: &str,
    ) -> Result<(), NodeRpcError> {
        self.request_ok(
            machine_id,
            DeployRpcOperation::CleanupVolumeClone,
            &NodeRequest::DeployNodeCleanupUncommittedVolumeClone {
                namespace: namespace.to_string(),
                deploy_id: deploy_id.as_str().to_string(),
                volume: volume.to_string(),
                source_namespace: source_namespace.to_string(),
                source_volume: source_volume.to_string(),
                snapshot: snapshot.to_string(),
            },
        )
        .await
    }

    pub async fn drain_instance(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> Result<(), NodeRpcError> {
        self.request_ok(
            machine_id,
            DeployRpcOperation::DrainInstance,
            &NodeRequest::DeployNodeDrainInstance {
                namespace: namespace.to_string(),
                deploy_id: deploy_id.as_str().to_string(),
                instance_id: instance_id.as_str().to_string(),
            },
        )
        .await
    }

    pub async fn remove_instance(
        &self,
        machine_id: &MachineId,
        namespace: &str,
        deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> Result<(), NodeRpcError> {
        self.request_ok(
            machine_id,
            DeployRpcOperation::RemoveInstance,
            &NodeRequest::DeployNodeRemoveInstance {
                namespace: namespace.to_string(),
                deploy_id: deploy_id.as_str().to_string(),
                instance_id: instance_id.as_str().to_string(),
            },
        )
        .await
    }

    async fn request_typed<P>(
        &self,
        machine_id: &MachineId,
        operation: DeployRpcOperation,
        request: &NodeRequest,
        expected_kind: &'static str,
    ) -> Result<P, NodeRpcError>
    where
        P: serde::de::DeserializeOwned,
    {
        let response = self
            .transport
            .deploy_request(machine_id, operation, request)
            .await?;
        decode_typed_payload(operation.operation_name(), response, expected_kind)
    }

    async fn request_ok(
        &self,
        machine_id: &MachineId,
        operation: DeployRpcOperation,
        request: &NodeRequest,
    ) -> Result<(), NodeRpcError> {
        let response = self
            .transport
            .deploy_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use ployz_model::{DrainState, InstancePhase, InstanceStatusRecord, Namespace};

    use super::super::NodeRpcErrorKind;
    use super::*;

    #[derive(Clone, Default)]
    struct FakeDeployTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, DeployRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeDeployTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl DeployRpcTransport for FakeDeployTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn deploy_request(
            &self,
            machine_id: &MachineId,
            operation: DeployRpcOperation,
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
                .unwrap_or_else(|| Ok(default_response(operation)))
        }
    }

    #[tokio::test]
    async fn deploy_node_client_builds_requests_and_applies_policy() {
        let transport = FakeDeployTransport::default();
        let client = DeployNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
            timeout: Duration::from_secs(11),
        });
        let machine_id = MachineId::new("machine-a");
        let deploy_id = DeployId::new("deploy-1");
        let instance_id = InstanceId::new("instance-1");

        client
            .inspect_namespace(&machine_id, "prod", &deploy_id)
            .await
            .expect("inspect namespace");
        client
            .start_candidate(
                &machine_id,
                "prod",
                &deploy_id,
                "web",
                &SlotId::new("slot-1"),
                &instance_id,
                "{\"image\":\"app\"}",
                "[{\"name\":\"data\"}]",
            )
            .await
            .expect("start candidate");
        client
            .clone_volume(
                &machine_id,
                "prod",
                &deploy_id,
                "data",
                "staging",
                "source-data",
                "snap",
                "10G",
                "0750",
                "999:999",
            )
            .await
            .expect("clone volume");
        client
            .cleanup_volume_clone(
                &machine_id,
                "prod",
                &deploy_id,
                "data",
                "staging",
                "source-data",
                "snap",
            )
            .await
            .expect("cleanup clone");
        client
            .drain_instance(&machine_id, "prod", &deploy_id, &instance_id)
            .await
            .expect("drain instance");
        client
            .remove_instance(&machine_id, "prod", &deploy_id, &instance_id)
            .await
            .expect("remove instance");

        let requests = transport.requests.lock().expect("requests");
        let [inspect, start, clone, cleanup, drain, remove] = requests.as_slice() else {
            panic!("expected six deploy requests");
        };
        assert_eq!(inspect.0, machine_id);
        assert_eq!(inspect.1, DeployRpcOperation::InspectNamespace);
        assert!(matches!(
            &inspect.2,
            NodeRequest::DeployNodeInspectNamespace {
                namespace,
                deploy_id,
            } if namespace == "prod" && deploy_id == "deploy-1"
        ));
        assert_eq!(start.1, DeployRpcOperation::StartCandidate);
        assert!(matches!(
            &start.2,
            NodeRequest::DeployNodeStartCandidate {
                namespace,
                deploy_id,
                service,
                slot_id,
                instance_id,
                spec_json,
                volumes_json,
            } if namespace == "prod"
                && deploy_id == "deploy-1"
                && service == "web"
                && slot_id == "slot-1"
                && instance_id == "instance-1"
                && spec_json == "{\"image\":\"app\"}"
                && volumes_json == "[{\"name\":\"data\"}]"
        ));
        assert_eq!(clone.1, DeployRpcOperation::CloneVolume);
        assert!(matches!(
            &clone.2,
            NodeRequest::DeployNodeCloneVolume {
                namespace,
                deploy_id,
                volume,
                source_namespace,
                source_volume,
                snapshot,
                quota,
                mode,
                owner,
            } if namespace == "prod"
                && deploy_id == "deploy-1"
                && volume == "data"
                && source_namespace == "staging"
                && source_volume == "source-data"
                && snapshot == "snap"
                && quota == "10G"
                && mode == "0750"
                && owner == "999:999"
        ));
        assert_eq!(cleanup.1, DeployRpcOperation::CleanupVolumeClone);
        assert!(matches!(
            &cleanup.2,
            NodeRequest::DeployNodeCleanupUncommittedVolumeClone {
                namespace,
                deploy_id,
                volume,
                source_namespace,
                source_volume,
                snapshot,
            } if namespace == "prod"
                && deploy_id == "deploy-1"
                && volume == "data"
                && source_namespace == "staging"
                && source_volume == "source-data"
                && snapshot == "snap"
        ));
        assert_eq!(drain.1, DeployRpcOperation::DrainInstance);
        assert!(matches!(
            &drain.2,
            NodeRequest::DeployNodeDrainInstance {
                namespace,
                deploy_id,
                instance_id,
            } if namespace == "prod" && deploy_id == "deploy-1" && instance_id == "instance-1"
        ));
        assert_eq!(remove.1, DeployRpcOperation::RemoveInstance);
        assert!(matches!(
            &remove.2,
            NodeRequest::DeployNodeRemoveInstance {
                namespace,
                deploy_id,
                instance_id,
            } if namespace == "prod" && deploy_id == "deploy-1" && instance_id == "instance-1"
        ));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(11)
            }]
        );
    }

    #[tokio::test]
    async fn deploy_node_client_maps_transport_remote_and_payload_errors() {
        let machine_id = MachineId::new("machine-a");
        let deploy_id = DeployId::new("deploy-1");
        let instance_id = InstanceId::new("instance-1");

        let transport = FakeDeployTransport::with_responses(vec![Err(NodeRpcError::transport(
            "deploy_node_inspect",
            "NATS_RPC_TIMEOUT",
            "timed out",
        ))]);
        let error = DeployNodeClient::new(transport)
            .inspect_namespace(&machine_id, "prod", &deploy_id)
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "deploy_node_inspect");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");

        let transport = FakeDeployTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_FAILED",
            "remote failed",
            None,
        ))]);
        let error = DeployNodeClient::new(transport)
            .drain_instance(&machine_id, "prod", &deploy_id, &instance_id)
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "deploy_node_drain");
        assert_eq!(error.code, "REMOTE_FAILED");
        assert_eq!(error.message, "remote failed");

        let transport =
            FakeDeployTransport::with_responses(vec![Ok(NodeResponse::success("ok", None))]);
        let error = DeployNodeClient::new(transport)
            .clone_volume(
                &machine_id,
                "prod",
                &deploy_id,
                "data",
                "staging",
                "source-data",
                "snap",
                "10G",
                "0750",
                "999:999",
            )
            .await
            .expect_err("missing payload should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::MissingPayload);
        assert_eq!(error.operation, "deploy_node_clone_volume");
        assert_eq!(error.code, "NODE_RPC_MISSING_PAYLOAD");

        let transport = FakeDeployTransport::with_responses(vec![Ok(NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": "wrong-kind",
                "instances": []
            })),
        ))]);
        let error = DeployNodeClient::new(transport)
            .inspect_namespace(&machine_id, "prod", &deploy_id)
            .await
            .expect_err("wrong kind should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::MissingPayload);
        assert_eq!(error.operation, "deploy_node_inspect");

        let transport = FakeDeployTransport::with_responses(vec![Ok(NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": DEPLOY_CANDIDATE_STARTED_PAYLOAD_KIND
            })),
        ))]);
        let error = DeployNodeClient::new(transport)
            .start_candidate(
                &machine_id,
                "prod",
                &deploy_id,
                "web",
                &SlotId::new("slot-1"),
                &instance_id,
                "{}",
                "[]",
            )
            .await
            .expect_err("invalid payload fields should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Decode);
        assert_eq!(error.operation, "deploy_node_start_candidate");
        assert_eq!(error.code, "NODE_RPC_DECODE_PAYLOAD");
    }

    #[test]
    fn deploy_rpc_policy_presets_match_operation_classes() {
        assert_eq!(
            DEPLOY_PARTICIPANT_RPC_POLICY.timeout,
            Duration::from_secs(10 * 60)
        );
        assert_eq!(
            DEPLOY_VOLUME_CLONE_RPC_POLICY.timeout,
            Duration::from_secs(2 * 60 * 60)
        );
        assert_eq!(
            DEPLOY_VOLUME_CLONE_CLEANUP_RPC_POLICY.timeout,
            Duration::from_secs(30 * 60)
        );
        assert_eq!(
            DEPLOY_VOLUME_MOVE_START_RPC_POLICY.timeout,
            Duration::from_secs(60)
        );
        assert_eq!(
            DEPLOY_VOLUME_MOVE_POLL_RPC_POLICY.timeout,
            Duration::from_secs(60)
        );
        assert_eq!(
            DEPLOY_VOLUME_MOVE_WAIT_TIMEOUT,
            Duration::from_secs(24 * 60 * 60)
        );
        assert_eq!(DEPLOY_VOLUME_MOVE_POLL_INTERVAL, Duration::from_secs(2));
    }

    fn default_response(operation: DeployRpcOperation) -> NodeResponse {
        let payload = match operation {
            DeployRpcOperation::InspectNamespace => serde_json::json!({
                "kind": DEPLOY_NAMESPACE_SNAPSHOT_PAYLOAD_KIND,
                "instances": []
            }),
            DeployRpcOperation::StartCandidate => serde_json::json!({
                "kind": DEPLOY_CANDIDATE_STARTED_PAYLOAD_KIND,
                "status": instance_status()
            }),
            DeployRpcOperation::CloneVolume => serde_json::json!({
                "kind": VOLUME_ZFS_CLONE_PAYLOAD_KIND,
                "namespace": "prod",
                "volume": "data",
                "source_namespace": "staging",
                "source_volume": "source-data",
                "machine_id": "machine-a",
                "source_dataset": "pool/staging/source-data",
                "target_dataset": "pool/prod/data",
                "snapshot": "snap",
                "guid": 42
            }),
            DeployRpcOperation::CleanupVolumeClone
            | DeployRpcOperation::DrainInstance
            | DeployRpcOperation::RemoveInstance => {
                return NodeResponse::success("ok", None);
            }
        };
        NodeResponse::success("ok", Some(payload))
    }

    fn instance_status() -> InstanceStatusRecord {
        InstanceStatusRecord {
            instance_id: InstanceId::new("instance-1"),
            namespace: Namespace::new("prod"),
            service: "web".into(),
            slot_id: SlotId::new("slot-1"),
            machine_id: MachineId::new("machine-a"),
            revision_hash: "rev".into(),
            deploy_id: DeployId::new("deploy-1"),
            docker_container_id: "container-1".into(),
            overlay_ip: None,
            backend_ports: BTreeMap::new(),
            phase: InstancePhase::Ready,
            ready: true,
            drain_state: DrainState::None,
            error: None,
            started_at: 1,
            updated_at: 2,
        }
    }
}
