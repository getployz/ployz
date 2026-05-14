use async_trait::async_trait;
use ployz_model::{MachineId, NetworkId};
use ployz_node_api::{NodeRequest, NodeResponse};

use super::{NodeRpcError, NodeRpcPolicy, ensure_success};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshReadinessRpcOperation {
    Ready,
    SelfRecord,
}

impl MeshReadinessRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::Ready => "mesh_ready",
            Self::SelfRecord => "mesh_self_record",
        }
    }
}

#[async_trait]
pub trait MeshReadinessRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn mesh_readiness_request(
        &self,
        machine_id: &MachineId,
        operation: MeshReadinessRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct MeshReadinessNodeClient<T> {
    transport: T,
}

impl<T> MeshReadinessNodeClient<T>
where
    T: MeshReadinessRpcTransport,
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

    pub async fn ready(
        &self,
        machine_id: &MachineId,
        json: bool,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.request_expect_response(
            machine_id,
            MeshReadinessRpcOperation::Ready,
            &NodeRequest::MeshReady { json },
        )
        .await
    }

    pub async fn self_record(&self, machine_id: &MachineId) -> Result<NodeResponse, NodeRpcError> {
        self.request_expect_response(
            machine_id,
            MeshReadinessRpcOperation::SelfRecord,
            &NodeRequest::MeshSelfRecord,
        )
        .await
    }

    async fn request_expect_response(
        &self,
        machine_id: &MachineId,
        operation: MeshReadinessRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        let response = self
            .transport
            .mesh_readiness_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)?;
        Ok(response)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshRpcOperation {
    PrepareDestroy,
    CancelDestroy,
    ExecuteDestroy,
    RemoveMachine,
}

impl MeshRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::PrepareDestroy => "mesh_peer_prepare_destroy",
            Self::CancelDestroy => "mesh_peer_cancel_destroy",
            Self::ExecuteDestroy => "mesh_peer_execute_destroy",
            Self::RemoveMachine => "mesh_peer_remove_machine",
        }
    }
}

#[async_trait]
pub trait MeshRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn mesh_request(
        &self,
        machine_id: &MachineId,
        operation: MeshRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct MeshNodeClient<T> {
    transport: T,
}

impl<T> MeshNodeClient<T>
where
    T: MeshRpcTransport,
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

    pub async fn prepare_destroy(
        &self,
        machine_id: &MachineId,
        operation_id: &str,
        network_id: &NetworkId,
        coordinator_id: &MachineId,
        expected_machine_ids: &[MachineId],
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MeshRpcOperation::PrepareDestroy,
            &NodeRequest::MeshPeerPrepareDestroy {
                operation_id: operation_id.to_string(),
                network_id: network_id.clone(),
                coordinator_id: coordinator_id.clone(),
                expected_machine_ids: expected_machine_ids.to_vec(),
            },
        )
        .await
    }

    pub async fn cancel_destroy(
        &self,
        machine_id: &MachineId,
        operation_id: &str,
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MeshRpcOperation::CancelDestroy,
            &NodeRequest::MeshPeerCancelDestroy {
                operation_id: operation_id.to_string(),
            },
        )
        .await
    }

    pub async fn execute_destroy(
        &self,
        machine_id: &MachineId,
        operation_id: &str,
        network_id: &NetworkId,
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MeshRpcOperation::ExecuteDestroy,
            &NodeRequest::MeshPeerExecuteDestroy {
                operation_id: operation_id.to_string(),
                network_id: network_id.clone(),
            },
        )
        .await
    }

    pub async fn remove_machine(
        &self,
        machine_id: &MachineId,
        operation_id: &str,
        network_id: &NetworkId,
        removed_machine_id: &MachineId,
    ) -> Result<(), NodeRpcError> {
        self.request_expect_ok(
            machine_id,
            MeshRpcOperation::RemoveMachine,
            &NodeRequest::MeshPeerRemoveMachine {
                operation_id: operation_id.to_string(),
                network_id: network_id.clone(),
                machine_id: removed_machine_id.clone(),
            },
        )
        .await
    }

    async fn request_expect_ok(
        &self,
        machine_id: &MachineId,
        operation: MeshRpcOperation,
        request: &NodeRequest,
    ) -> Result<(), NodeRpcError> {
        let response = self
            .transport
            .mesh_request(machine_id, operation, request)
            .await?;
        ensure_success(operation.operation_name(), &response)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;

    use super::super::NodeRpcErrorKind;
    use super::*;

    #[derive(Clone, Default)]
    struct FakeMeshReadinessTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, MeshReadinessRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeMeshReadinessTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MeshReadinessRpcTransport for FakeMeshReadinessTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn mesh_readiness_request(
            &self,
            machine_id: &MachineId,
            operation: MeshReadinessRpcOperation,
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
    async fn mesh_readiness_client_builds_ready_and_self_record_requests_and_applies_policy() {
        let transport = FakeMeshReadinessTransport::default();
        let client = MeshReadinessNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
            timeout: Duration::from_secs(11),
        });
        let machine_id = MachineId::new("machine-a");

        client.ready(&machine_id, false).await.expect("mesh ready");
        client
            .self_record(&machine_id)
            .await
            .expect("mesh self record");

        let requests = transport.requests.lock().expect("requests");
        let [ready, self_record] = requests.as_slice() else {
            panic!("expected two requests");
        };
        assert_eq!(ready.0, machine_id);
        assert_eq!(ready.1, MeshReadinessRpcOperation::Ready);
        assert!(matches!(&ready.2, NodeRequest::MeshReady { json: false }));
        assert_eq!(self_record.0, machine_id);
        assert_eq!(self_record.1, MeshReadinessRpcOperation::SelfRecord);
        assert!(matches!(&self_record.2, NodeRequest::MeshSelfRecord));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(11)
            }]
        );
    }

    #[tokio::test]
    async fn mesh_readiness_client_preserves_remote_error_and_transport_error() {
        let machine_id = MachineId::new("machine-a");

        let transport = FakeMeshReadinessTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_REFUSED",
            "peer refused",
            None,
        ))]);
        let error = MeshReadinessNodeClient::new(transport)
            .ready(&machine_id, false)
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "mesh_ready");
        assert_eq!(error.code, "REMOTE_REFUSED");
        assert_eq!(error.message, "peer refused");

        let transport = FakeMeshReadinessTransport::with_responses(vec![Err(
            NodeRpcError::transport("mesh_self_record", "NATS_RPC_TIMEOUT", "timed out"),
        )]);
        let error = MeshReadinessNodeClient::new(transport)
            .self_record(&machine_id)
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "mesh_self_record");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");
    }

    #[derive(Clone, Default)]
    struct FakeMeshTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, MeshRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeMeshTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MeshRpcTransport for FakeMeshTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn mesh_request(
            &self,
            machine_id: &MachineId,
            operation: MeshRpcOperation,
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
    async fn mesh_node_client_builds_mesh_membership_requests_and_applies_policy() {
        let transport = FakeMeshTransport::default();
        let client = MeshNodeClient::new(transport.clone()).with_policy(NodeRpcPolicy {
            timeout: Duration::from_secs(7),
        });
        let machine_id = MachineId::new("machine-a");
        let network_id = NetworkId::new("network-a");
        let coordinator_id = MachineId::new("coordinator-a");
        let expected_machine_ids = vec![MachineId::new("machine-a"), MachineId::new("machine-b")];
        let removed_machine_id = MachineId::new("machine-b");

        client
            .prepare_destroy(
                &machine_id,
                "destroy-1",
                &network_id,
                &coordinator_id,
                &expected_machine_ids,
            )
            .await
            .expect("prepare destroy");
        client
            .cancel_destroy(&machine_id, "destroy-1")
            .await
            .expect("cancel destroy");
        client
            .execute_destroy(&machine_id, "destroy-1", &network_id)
            .await
            .expect("execute destroy");
        client
            .remove_machine(&machine_id, "remove-1", &network_id, &removed_machine_id)
            .await
            .expect("remove machine");

        let requests = transport.requests.lock().expect("requests");
        let [prepare, cancel, execute, remove] = requests.as_slice() else {
            panic!("expected four requests");
        };
        assert_eq!(prepare.0, machine_id);
        assert_eq!(prepare.1, MeshRpcOperation::PrepareDestroy);
        assert!(matches!(
            &prepare.2,
            NodeRequest::MeshPeerPrepareDestroy {
                operation_id,
                network_id: request_network_id,
                coordinator_id: request_coordinator_id,
                expected_machine_ids: request_expected_machine_ids,
            } if operation_id == "destroy-1"
                && request_network_id == &network_id
                && request_coordinator_id == &coordinator_id
                && request_expected_machine_ids == &expected_machine_ids
        ));
        assert_eq!(cancel.0, machine_id);
        assert_eq!(cancel.1, MeshRpcOperation::CancelDestroy);
        assert!(matches!(
            &cancel.2,
            NodeRequest::MeshPeerCancelDestroy { operation_id } if operation_id == "destroy-1"
        ));
        assert_eq!(execute.0, machine_id);
        assert_eq!(execute.1, MeshRpcOperation::ExecuteDestroy);
        assert!(matches!(
            &execute.2,
            NodeRequest::MeshPeerExecuteDestroy {
                operation_id,
                network_id: request_network_id,
            } if operation_id == "destroy-1" && request_network_id == &network_id
        ));
        assert_eq!(remove.0, machine_id);
        assert_eq!(remove.1, MeshRpcOperation::RemoveMachine);
        assert!(matches!(
            &remove.2,
            NodeRequest::MeshPeerRemoveMachine {
                operation_id,
                network_id: request_network_id,
                machine_id: request_machine_id,
            } if operation_id == "remove-1"
                && request_network_id == &network_id
                && request_machine_id == &removed_machine_id
        ));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[NodeRpcPolicy {
                timeout: Duration::from_secs(7)
            }]
        );
    }

    #[tokio::test]
    async fn mesh_node_client_preserves_remote_error_and_transport_error() {
        let machine_id = MachineId::new("machine-a");
        let network_id = NetworkId::new("network-a");

        let transport = FakeMeshTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_REFUSED",
            "peer refused",
            None,
        ))]);
        let error = MeshNodeClient::new(transport)
            .execute_destroy(&machine_id, "destroy-1", &network_id)
            .await
            .expect_err("remote response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "mesh_peer_execute_destroy");
        assert_eq!(error.code, "REMOTE_REFUSED");
        assert_eq!(error.message, "peer refused");

        let transport = FakeMeshTransport::with_responses(vec![Err(NodeRpcError::transport(
            "mesh_peer_prepare_destroy",
            "NATS_RPC_TIMEOUT",
            "timed out",
        ))]);
        let error = MeshNodeClient::new(transport)
            .prepare_destroy(
                &machine_id,
                "destroy-1",
                &network_id,
                &MachineId::new("coordinator-a"),
                std::slice::from_ref(&machine_id),
            )
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "mesh_peer_prepare_destroy");
        assert_eq!(error.code, "NATS_RPC_TIMEOUT");

        let transport = FakeMeshTransport::with_responses(vec![Ok(NodeResponse::error(
            "REMOTE_REMOVE_REFUSED",
            "remove refused",
            None,
        ))]);
        let error = MeshNodeClient::new(transport)
            .remove_machine(&machine_id, "remove-1", &network_id, &machine_id)
            .await
            .expect_err("remote remove response should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Remote);
        assert_eq!(error.operation, "mesh_peer_remove_machine");
        assert_eq!(error.code, "REMOTE_REMOVE_REFUSED");
        assert_eq!(error.message, "remove refused");
    }
}
