use async_trait::async_trait;
use ployz_model::{
    MachineId, MachineSelfTransition, MachineStorageAuthorityPeer, StorageParticipation,
    StorageReplicaPolicy,
};
use ployz_node_api::{NodeRequest, NodeResponse};

use super::{NodeRpcError, NodeRpcPolicy, ensure_success};

#[cfg(test)]
use super::NodeRpcErrorKind;
#[cfg(test)]
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineOperationRpcOperation {
    Get,
}

impl MachineOperationRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::Get => "machine_operation_get",
        }
    }
}

#[async_trait]
pub trait MachineOperationRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn machine_operation_request(
        &self,
        machine_id: &MachineId,
        operation: MachineOperationRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct MachineOperationNodeClient<T> {
    transport: T,
}

impl<T> MachineOperationNodeClient<T>
where
    T: MachineOperationRpcTransport,
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

    pub async fn get(
        &self,
        machine_id: &MachineId,
        operation_id: &str,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.request_expect_response(
            machine_id,
            MachineOperationRpcOperation::Get,
            &NodeRequest::MachineOperationGet {
                id: operation_id.to_string(),
            },
        )
        .await
    }

    async fn request_expect_response(
        &self,
        machine_id: &MachineId,
        operation: MachineOperationRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        let response = self
            .transport
            .machine_operation_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)?;
        Ok(response)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineLifecycleRpcOperation {
    TransitionSelf,
}

impl MachineLifecycleRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::TransitionSelf => "machine_transition_self",
        }
    }
}

#[async_trait]
pub trait MachineLifecycleRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn machine_lifecycle_request(
        &self,
        machine_id: &MachineId,
        operation: MachineLifecycleRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct MachineLifecycleNodeClient<T> {
    transport: T,
}

impl<T> MachineLifecycleNodeClient<T>
where
    T: MachineLifecycleRpcTransport,
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

    pub async fn transition_self(
        &self,
        machine_id: &MachineId,
        transition: MachineSelfTransition,
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MachineLifecycleRpcOperation::TransitionSelf,
            &NodeRequest::MachineTransitionSelf { transition },
        )
        .await
    }

    async fn request_expect_ok(
        &self,
        machine_id: &MachineId,
        operation: MachineLifecycleRpcOperation,
        request: &NodeRequest,
    ) -> Result<(), NodeRpcError> {
        let response = self
            .transport
            .machine_lifecycle_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineUpdateRpcOperation {
    PrepareUpdate,
    ExecuteUpdate,
}

impl MachineUpdateRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::PrepareUpdate => "machine_update_prepare",
            Self::ExecuteUpdate => "machine_update_execute",
        }
    }
}

#[async_trait]
pub trait MachineUpdateRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn machine_update_request(
        &self,
        machine_id: &MachineId,
        operation: MachineUpdateRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct MachineUpdateNodeClient<T> {
    transport: T,
}

impl<T> MachineUpdateNodeClient<T>
where
    T: MachineUpdateRpcTransport,
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

    pub async fn prepare_update(
        &self,
        machine_id: &MachineId,
        operation_id: &str,
        version: &str,
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MachineUpdateRpcOperation::PrepareUpdate,
            &NodeRequest::MeshPeerPrepareUpdate {
                operation_id: operation_id.to_string(),
                version: version.to_string(),
            },
        )
        .await
    }

    pub async fn execute_update(
        &self,
        machine_id: &MachineId,
        operation_id: &str,
        version: &str,
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MachineUpdateRpcOperation::ExecuteUpdate,
            &NodeRequest::MeshPeerExecuteUpdate {
                operation_id: operation_id.to_string(),
                version: version.to_string(),
            },
        )
        .await
    }

    async fn request_expect_ok(
        &self,
        machine_id: &MachineId,
        operation: MachineUpdateRpcOperation,
        request: &NodeRequest,
    ) -> Result<(), NodeRpcError> {
        let response = self
            .transport
            .machine_update_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineStorageRpcOperation {
    StoragePromoteSelf,
    StorageRestoreSelf,
}

impl MachineStorageRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::StoragePromoteSelf => "machine_storage_promote_self",
            Self::StorageRestoreSelf => "machine_storage_restore_self",
        }
    }
}

#[async_trait]
pub trait MachineStorageRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn machine_request(
        &self,
        machine_id: &MachineId,
        operation: MachineStorageRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct MachineStorageNodeClient<T> {
    transport: T,
}

impl<T> MachineStorageNodeClient<T>
where
    T: MachineStorageRpcTransport,
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

    pub async fn promote_storage_self(
        &self,
        machine_id: &MachineId,
        replicas: StorageReplicaPolicy,
        authority_peers: &[MachineStorageAuthorityPeer],
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MachineStorageRpcOperation::StoragePromoteSelf,
            &NodeRequest::MachineStoragePromoteSelf {
                replicas,
                authority_peers: authority_peers.to_vec(),
            },
        )
        .await
    }

    pub async fn restore_storage_self(
        &self,
        machine_id: &MachineId,
        participation: &StorageParticipation,
        replicas: StorageReplicaPolicy,
        authority_peers: &[MachineStorageAuthorityPeer],
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MachineStorageRpcOperation::StorageRestoreSelf,
            &NodeRequest::MachineStorageRestoreSelf {
                participation: participation.clone(),
                replicas,
                authority_peers: authority_peers.to_vec(),
            },
        )
        .await
    }

    async fn request_expect_ok(
        &self,
        machine_id: &MachineId,
        operation: MachineStorageRpcOperation,
        request: &NodeRequest,
    ) -> Result<(), NodeRpcError> {
        let response = self
            .transport
            .machine_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)
    }
}

#[cfg(test)]
mod machine_operation_tests {
    use std::collections::VecDeque;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeMachineOperationTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, MachineOperationRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeMachineOperationTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MachineOperationRpcTransport for FakeMachineOperationTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn machine_operation_request(
            &self,
            machine_id: &MachineId,
            operation: MachineOperationRpcOperation,
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
                .unwrap_or_else(|| Ok(NodeResponse::success("ok", None)))
        }
    }

    #[tokio::test]
    async fn machine_operation_client_builds_get_request_and_applies_policy() {
        let transport = FakeMachineOperationTransport::default();
        let client =
            MachineOperationNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
                timeout: Duration::from_secs(11),
            });
        let machine_id = MachineId::new("machine-a");

        client
            .get(&machine_id, "operation-1")
            .await
            .expect("machine operation get");

        let requests = transport.requests.lock().expect("requests");
        let [get] = requests.as_slice() else {
            panic!("expected one request");
        };
        assert_eq!(get.0, machine_id);
        assert_eq!(get.1, MachineOperationRpcOperation::Get);
        assert!(matches!(
            &get.2,
            NodeRequest::MachineOperationGet { id } if id == "operation-1"
        ));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(11)
            }]
        );
    }

    #[tokio::test]
    async fn machine_operation_client_preserves_remote_error_and_transport_error() {
        let machine_id = MachineId::new("machine-a");

        let transport = FakeMachineOperationTransport::with_responses(vec![Ok(
            NodeResponse::error("REMOTE_REFUSED", "peer refused", None),
        )]);
        let error = MachineOperationNodeClient::new(transport)
            .get(&machine_id, "operation-1")
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "machine_operation_get");
        assert_eq!(error.code, "REMOTE_REFUSED");
        assert_eq!(error.message, "peer refused");

        let transport = FakeMachineOperationTransport::with_responses(vec![Err(
            NodeRpcError::transport("machine_operation_get", "NATS_RPC_TIMEOUT", "timed out"),
        )]);
        let error = MachineOperationNodeClient::new(transport)
            .get(&machine_id, "operation-1")
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "machine_operation_get");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");
    }
}

#[cfg(test)]
mod machine_lifecycle_tests {
    use std::collections::VecDeque;

    use ployz_model::MachineSelfTransition;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeMachineLifecycleTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, MachineLifecycleRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeMachineLifecycleTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MachineLifecycleRpcTransport for FakeMachineLifecycleTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn machine_lifecycle_request(
            &self,
            machine_id: &MachineId,
            operation: MachineLifecycleRpcOperation,
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
                .unwrap_or_else(|| Ok(NodeResponse::success("ok", None)))
        }
    }

    #[tokio::test]
    async fn machine_lifecycle_node_client_builds_transition_request_and_applies_policy() {
        let transport = FakeMachineLifecycleTransport::default();
        let client =
            MachineLifecycleNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
                timeout: Duration::from_secs(11),
            });
        let machine_id = MachineId::new("machine-a");

        client
            .transition_self(&machine_id, MachineSelfTransition::Drain)
            .await
            .expect("transition self");

        let requests = transport.requests.lock().expect("requests");
        let [transition] = requests.as_slice() else {
            panic!("expected one request");
        };
        assert_eq!(transition.0, machine_id);
        assert_eq!(transition.1, MachineLifecycleRpcOperation::TransitionSelf);
        assert!(matches!(
            &transition.2,
            NodeRequest::MachineTransitionSelf {
                transition: MachineSelfTransition::Drain,
            }
        ));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(11)
            }]
        );
    }

    #[tokio::test]
    async fn machine_lifecycle_node_client_preserves_remote_error_and_transport_error() {
        let machine_id = MachineId::new("machine-a");

        let transport = FakeMachineLifecycleTransport::with_responses(vec![Ok(
            NodeResponse::error("REMOTE_REFUSED", "peer refused", None),
        )]);
        let error = MachineLifecycleNodeClient::new(transport)
            .transition_self(&machine_id, MachineSelfTransition::Drain)
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "machine_transition_self");
        assert_eq!(error.code, "REMOTE_REFUSED");
        assert_eq!(error.message, "peer refused");

        let transport = FakeMachineLifecycleTransport::with_responses(vec![Err(
            NodeRpcError::transport("machine_transition_self", "NATS_RPC_TIMEOUT", "timed out"),
        )]);
        let error = MachineLifecycleNodeClient::new(transport)
            .transition_self(&machine_id, MachineSelfTransition::Drain)
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "machine_transition_self");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");
    }
}

#[cfg(test)]
mod machine_update_tests {
    use std::collections::VecDeque;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeMachineUpdateTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, MachineUpdateRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeMachineUpdateTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MachineUpdateRpcTransport for FakeMachineUpdateTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn machine_update_request(
            &self,
            machine_id: &MachineId,
            operation: MachineUpdateRpcOperation,
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
                .unwrap_or_else(|| Ok(NodeResponse::success("ok", None)))
        }
    }

    #[tokio::test]
    async fn machine_update_node_client_builds_update_requests_and_applies_policy() {
        let transport = FakeMachineUpdateTransport::default();
        let client = MachineUpdateNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
            timeout: Duration::from_secs(11),
        });
        let machine_id = MachineId::new("machine-a");

        client
            .prepare_update(&machine_id, "operation-1", "v1.2.3")
            .await
            .expect("prepare update");
        client
            .execute_update(&machine_id, "operation-1", "v1.2.3")
            .await
            .expect("execute update");

        let requests = transport.requests.lock().expect("requests");
        let [prepare, execute] = requests.as_slice() else {
            panic!("expected two requests");
        };
        assert_eq!(prepare.0, machine_id);
        assert_eq!(prepare.1, MachineUpdateRpcOperation::PrepareUpdate);
        assert!(matches!(
            &prepare.2,
            NodeRequest::MeshPeerPrepareUpdate {
                operation_id,
                version,
            } if operation_id == "operation-1" && version == "v1.2.3"
        ));
        assert_eq!(execute.0, machine_id);
        assert_eq!(execute.1, MachineUpdateRpcOperation::ExecuteUpdate);
        assert!(matches!(
            &execute.2,
            NodeRequest::MeshPeerExecuteUpdate {
                operation_id,
                version,
            } if operation_id == "operation-1" && version == "v1.2.3"
        ));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(11)
            }]
        );
    }

    #[tokio::test]
    async fn machine_update_node_client_preserves_remote_error_and_transport_error() {
        let machine_id = MachineId::new("machine-a");

        let transport = FakeMachineUpdateTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_REFUSED",
            "peer refused",
            None,
        ))]);
        let error = MachineUpdateNodeClient::new(transport)
            .prepare_update(&machine_id, "operation-1", "v1.2.3")
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "machine_update_prepare");
        assert_eq!(error.code, "REMOTE_REFUSED");
        assert_eq!(error.message, "peer refused");

        let transport = FakeMachineUpdateTransport::with_responses(vec![Err(
            NodeRpcError::transport("machine_update_execute", "NATS_RPC_TIMEOUT", "timed out"),
        )]);
        let error = MachineUpdateNodeClient::new(transport)
            .execute_update(&machine_id, "operation-1", "v1.2.3")
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "machine_update_execute");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");
    }
}

#[cfg(test)]
mod machine_tests {
    use std::collections::VecDeque;

    use ployz_model::{
        MachineLifecycle, MachineMembership, MachineStorageRole, MachineTopology, OverlayIp,
        PublicKey, RegionRole,
    };

    use super::*;

    #[derive(Clone, Default)]
    struct FakeMachineTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, MachineStorageRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeMachineTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MachineStorageRpcTransport for FakeMachineTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn machine_request(
            &self,
            machine_id: &MachineId,
            operation: MachineStorageRpcOperation,
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
                .unwrap_or_else(|| Ok(NodeResponse::success("ok", None)))
        }
    }

    #[tokio::test]
    async fn machine_storage_node_client_builds_storage_requests_and_applies_policy() {
        let transport = FakeMachineTransport::default();
        let client = MachineStorageNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
            timeout: Duration::from_secs(9),
        });
        let machine_id = MachineId::new("machine-a");
        let peers = [machine_record("machine-a"), machine_record("machine-b")];
        let peer_payloads = peers
            .iter()
            .map(MachineStorageAuthorityPeer::from)
            .collect::<Vec<_>>();

        client
            .promote_storage_self(&machine_id, StorageReplicaPolicy::R3, &peer_payloads)
            .await
            .expect("promote storage self");
        client
            .restore_storage_self(
                &machine_id,
                &StorageParticipation::Candidate,
                StorageReplicaPolicy::R3,
                &peer_payloads,
            )
            .await
            .expect("restore storage self");

        let requests = transport.requests.lock().expect("requests");
        let [promote, restore] = requests.as_slice() else {
            panic!("expected two requests");
        };
        assert_eq!(promote.0, machine_id);
        assert_eq!(promote.1, MachineStorageRpcOperation::StoragePromoteSelf);
        assert!(matches!(
            &promote.2,
            NodeRequest::MachineStoragePromoteSelf {
                replicas,
                authority_peers,
            } if replicas == &StorageReplicaPolicy::R3
                && authority_peers == &peer_payloads
        ));
        assert_eq!(restore.0, machine_id);
        assert_eq!(restore.1, MachineStorageRpcOperation::StorageRestoreSelf);
        assert!(matches!(
            &restore.2,
            NodeRequest::MachineStorageRestoreSelf {
                participation,
                replicas,
                authority_peers,
            } if participation == &StorageParticipation::Candidate
                && replicas == &StorageReplicaPolicy::R3
                && authority_peers == &peer_payloads
        ));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(9)
            }]
        );
    }

    #[tokio::test]
    async fn machine_storage_node_client_preserves_remote_error_and_transport_error() {
        let machine_id = MachineId::new("machine-a");
        let peers = [machine_record("machine-a")];
        let peer_payloads = peers
            .iter()
            .map(MachineStorageAuthorityPeer::from)
            .collect::<Vec<_>>();

        let transport = FakeMachineTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_REFUSED",
            "peer refused",
            None,
        ))]);
        let error = MachineStorageNodeClient::new(transport)
            .promote_storage_self(&machine_id, StorageReplicaPolicy::R3, &peer_payloads)
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "machine_storage_promote_self");
        assert_eq!(error.code, "REMOTE_REFUSED");
        assert_eq!(error.message, "peer refused");

        let transport = FakeMachineTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_RESTORE_REFUSED",
            "restore refused",
            None,
        ))]);
        let error = MachineStorageNodeClient::new(transport)
            .restore_storage_self(
                &machine_id,
                &StorageParticipation::Candidate,
                StorageReplicaPolicy::R3,
                &peer_payloads,
            )
            .await
            .expect_err("remote restore response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "machine_storage_restore_self");
        assert_eq!(error.code, "REMOTE_RESTORE_REFUSED");
        assert_eq!(error.message, "restore refused");

        let transport = FakeMachineTransport::with_responses(vec![Err(NodeRpcError::transport(
            "machine_storage_restore_self",
            "NATS_RPC_TIMEOUT",
            "timed out",
        ))]);
        let error = MachineStorageNodeClient::new(transport)
            .restore_storage_self(
                &machine_id,
                &StorageParticipation::Candidate,
                StorageReplicaPolicy::R3,
                &peer_payloads,
            )
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "machine_storage_restore_self");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");
    }

    fn machine_record(id: &str) -> MachineMembership {
        MachineMembership {
            id: MachineId::new(id),
            public_key: PublicKey([id.len() as u8; 32]),
            overlay_ip: format!("fd00::{id_len:x}", id_len = id.len())
                .parse()
                .map(OverlayIp)
                .expect("valid overlay"),
            topology: MachineTopology::local(),
            region_role: RegionRole::HomeData,
            subnet: Some("10.42.0.0/24".parse().expect("valid subnet")),
            bridge_ip: None,
            endpoints: vec!["127.0.0.1:51820".into()],
            lifecycle: MachineLifecycle::Active,
            storage_role: MachineStorageRole::default_authority(),
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        }
    }
}
