use async_trait::async_trait;
use ployz_model::{
    ImageDistributeRequest, ImageReceiveSessionRequest, ImageReceivedImportRequest, MachineId,
};
use ployz_node_api::{
    IMAGE_DISTRIBUTE_PAYLOAD_KIND, IMAGE_DISTRIBUTE_VALIDATION_PAYLOAD_KIND,
    IMAGE_RECEIVE_SESSION_PAYLOAD_KIND, IMAGE_RECEIVED_IMPORT_PAYLOAD_KIND,
    NodeImageDistributePayload, NodeImageDistributeValidationPayload,
    NodeImageReceiveSessionPayload, NodeImageReceivedImportPayload, NodeRequest, NodeResponse,
};

use super::{NodeRpcError, NodeRpcPolicy, NodeServiceResponse, decode_payload_variant};

pub const IMAGE_RECEIVE_SESSION_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(15);
pub const IMAGE_DISTRIBUTE_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(30 * 60);
pub const IMAGE_RECEIVED_IMPORT_RPC_POLICY: NodeRpcPolicy = NodeRpcPolicy::from_secs(30 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageRpcOperation {
    ReceiveSession,
    Distribute,
    ReceivedImport,
}

impl ImageRpcOperation {
    #[must_use]
    pub fn operation_name(self) -> &'static str {
        match self {
            Self::ReceiveSession => "image_receive_session",
            Self::Distribute => "image_distribute",
            Self::ReceivedImport => "image_received_import",
        }
    }

    #[must_use]
    pub fn rpc_policy(self) -> NodeRpcPolicy {
        match self {
            Self::ReceiveSession => IMAGE_RECEIVE_SESSION_RPC_POLICY,
            Self::Distribute => IMAGE_DISTRIBUTE_RPC_POLICY,
            Self::ReceivedImport => IMAGE_RECEIVED_IMPORT_RPC_POLICY,
        }
    }
}

#[async_trait]
pub trait ImageRpcTransport: Clone + Send + Sync {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self;

    async fn image_request(
        &self,
        machine_id: &MachineId,
        operation: ImageRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError>;
}

#[derive(Clone)]
pub struct ImageNodeClient<T> {
    transport: T,
}

impl<T> ImageNodeClient<T>
where
    T: ImageRpcTransport,
{
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    pub async fn receive_session(
        &self,
        machine_id: &MachineId,
        request: ImageReceiveSessionRequest,
    ) -> Result<ImageNodeResponse, NodeRpcError> {
        self.request_image(
            machine_id,
            ImageRpcOperation::ReceiveSession,
            &NodeRequest::ImageReceiveSession { request },
        )
        .await
    }

    pub async fn distribute(
        &self,
        machine_id: &MachineId,
        request: ImageDistributeRequest,
    ) -> Result<ImageNodeResponse, NodeRpcError> {
        self.request_image(
            machine_id,
            ImageRpcOperation::Distribute,
            &NodeRequest::ImageDistribute { request },
        )
        .await
    }

    pub async fn received_import(
        &self,
        machine_id: &MachineId,
        request: ImageReceivedImportRequest,
    ) -> Result<ImageNodeResponse, NodeRpcError> {
        self.request_image(
            machine_id,
            ImageRpcOperation::ReceivedImport,
            &NodeRequest::ImageReceivedImport { request },
        )
        .await
    }

    async fn request_image(
        &self,
        machine_id: &MachineId,
        operation: ImageRpcOperation,
        request: &NodeRequest,
    ) -> Result<ImageNodeResponse, NodeRpcError> {
        let transport = self.transport.with_node_rpc_policy(operation.rpc_policy());
        let response = transport
            .image_request(machine_id, operation, request)
            .await?;
        NodeServiceResponse::from_node_response(response, |payload| {
            decode_image_payload(operation.operation_name(), payload)
        })
    }
}

pub type ImageNodeResponse = NodeServiceResponse<ImageNodePayload>;

#[derive(Debug, Clone)]
pub enum ImageNodePayload {
    Distribute(NodeImageDistributePayload),
    DistributeValidation(NodeImageDistributeValidationPayload),
    ReceiveSession(NodeImageReceiveSessionPayload),
    ReceivedImport(NodeImageReceivedImportPayload),
}

fn decode_image_payload(
    operation_name: &'static str,
    payload: serde_json::Value,
) -> Result<ImageNodePayload, NodeRpcError> {
    let Some(kind) = payload.get("kind").and_then(serde_json::Value::as_str) else {
        return Err(NodeRpcError::missing_payload(
            operation_name,
            "image payload",
        ));
    };
    match kind {
        IMAGE_DISTRIBUTE_PAYLOAD_KIND => {
            decode_payload_variant(operation_name, IMAGE_DISTRIBUTE_PAYLOAD_KIND, payload)
                .map(ImageNodePayload::Distribute)
        }
        IMAGE_DISTRIBUTE_VALIDATION_PAYLOAD_KIND => decode_payload_variant(
            operation_name,
            IMAGE_DISTRIBUTE_VALIDATION_PAYLOAD_KIND,
            payload,
        )
        .map(ImageNodePayload::DistributeValidation),
        IMAGE_RECEIVE_SESSION_PAYLOAD_KIND => {
            decode_payload_variant(operation_name, IMAGE_RECEIVE_SESSION_PAYLOAD_KIND, payload)
                .map(ImageNodePayload::ReceiveSession)
        }
        IMAGE_RECEIVED_IMPORT_PAYLOAD_KIND => {
            decode_payload_variant(operation_name, IMAGE_RECEIVED_IMPORT_PAYLOAD_KIND, payload)
                .map(ImageNodePayload::ReceivedImport)
        }
        _ => Err(NodeRpcError::missing_payload(
            operation_name,
            "image payload",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use ployz_model::{
        ImageArtifact, ImageArtifactProvenance, ImageAvailabilityRecord, ImageDigest,
        ImageDistributeValidationFailure, ImagePlatform, ImagePresence, ImageRef,
    };
    use ployz_node_api::{
        IMAGE_DISTRIBUTE_PAYLOAD_KIND, IMAGE_DISTRIBUTE_VALIDATION_PAYLOAD_KIND,
        IMAGE_RECEIVE_SESSION_PAYLOAD_KIND, IMAGE_RECEIVED_IMPORT_PAYLOAD_KIND, NodeRequest,
        NodeResponse,
    };

    use super::*;
    use crate::{NodeRpcErrorKind, NodeRpcPolicy};

    #[derive(Clone, Default)]
    struct FakeImageTransport {
        responses: Arc<Mutex<VecDeque<Result<NodeResponse, NodeRpcError>>>>,
        requests: Arc<Mutex<Vec<(MachineId, ImageRpcOperation, NodeRequest)>>>,
        policies: Arc<Mutex<Vec<NodeRpcPolicy>>>,
    }

    impl FakeImageTransport {
        fn with_responses(responses: Vec<Result<NodeResponse, NodeRpcError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                requests: Arc::new(Mutex::new(Vec::new())),
                policies: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl ImageRpcTransport for FakeImageTransport {
        fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
            self.policies.lock().expect("policies").push(policy);
            self.clone()
        }

        async fn image_request(
            &self,
            machine_id: &MachineId,
            operation: ImageRpcOperation,
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
    async fn image_node_client_builds_requests_and_applies_operation_policies() {
        let transport = FakeImageTransport::default();
        let client = ImageNodeClient::new(transport.clone());
        let target_machine = MachineId::new("machine-b");
        let source_machine = MachineId::new("machine-a");
        let digest = digest();
        let platform = ImagePlatform {
            os: "linux".into(),
            architecture: "amd64".into(),
            variant: Some("v8".into()),
        };
        let repo_tags = vec!["example/app:latest".into(), "example/app:v1".into()];

        client
            .receive_session(
                &target_machine,
                ImageReceiveSessionRequest {
                    operation_id: "image-push-1".into(),
                    source_machine: source_machine.clone(),
                    repository: Some("ployz/image-push-1".into()),
                },
            )
            .await
            .expect("receive session");
        client
            .distribute(
                &source_machine,
                ImageDistributeRequest {
                    digest: digest.clone(),
                    source_machine: source_machine.clone(),
                    target_machines: vec![target_machine.clone()],
                    platform: Some(platform.clone()),
                },
            )
            .await
            .expect("distribute");
        client
            .received_import(
                &target_machine,
                ImageReceivedImportRequest {
                    operation_id: "image-push-1".into(),
                    source_machine: source_machine.clone(),
                    repository: "ployz/image-push-1".into(),
                    reference: "image-push-1".into(),
                    expected_digest: digest.clone(),
                    platform: Some(platform.clone()),
                    repo_tags: repo_tags.clone(),
                },
            )
            .await
            .expect("received import");

        let requests = transport.requests.lock().expect("requests");
        let [receive_session, distribute, received_import] = requests.as_slice() else {
            panic!("expected three image requests");
        };
        assert_eq!(receive_session.0, target_machine);
        assert_eq!(receive_session.1, ImageRpcOperation::ReceiveSession);
        assert!(matches!(
            &receive_session.2,
            NodeRequest::ImageReceiveSession { request }
                if request.operation_id == "image-push-1"
                    && request.source_machine == source_machine
                    && request.repository.as_deref() == Some("ployz/image-push-1")
        ));
        assert_eq!(distribute.0, source_machine);
        assert_eq!(distribute.1, ImageRpcOperation::Distribute);
        assert!(matches!(
            &distribute.2,
            NodeRequest::ImageDistribute { request }
                if request.digest == digest
                    && request.source_machine == source_machine
                    && request.target_machines.as_slice() == [target_machine.clone()]
                    && request.platform.as_ref() == Some(&platform)
        ));
        assert_eq!(received_import.0, target_machine);
        assert_eq!(received_import.1, ImageRpcOperation::ReceivedImport);
        assert!(matches!(
            &received_import.2,
            NodeRequest::ImageReceivedImport { request }
                if request.operation_id == "image-push-1"
                    && request.source_machine == source_machine
                    && request.repository == "ployz/image-push-1"
                    && request.reference == "image-push-1"
                    && request.expected_digest == digest
                    && request.platform.as_ref() == Some(&platform)
                    && request.repo_tags.as_slice() == repo_tags.as_slice()
        ));
        assert_eq!(
            transport.policies.lock().expect("policies").as_slice(),
            &[
                IMAGE_RECEIVE_SESSION_RPC_POLICY,
                IMAGE_DISTRIBUTE_RPC_POLICY,
                IMAGE_RECEIVED_IMPORT_RPC_POLICY,
            ]
        );
    }

    #[tokio::test]
    async fn image_node_client_preserves_remote_error_payloads_and_maps_decode_errors() {
        let digest = digest();
        let target_machine = MachineId::new("machine-b");
        let source_machine = MachineId::new("machine-a");
        let platform = ImagePlatform {
            os: "linux".into(),
            architecture: "amd64".into(),
            variant: Some("v8".into()),
        };
        let transport = FakeImageTransport::with_responses(vec![Ok(NodeResponse::error(
            "IMAGE_DISTRIBUTE_DUPLICATE_TARGET",
            "duplicate target",
            Some(serde_json::json!({
                "kind": IMAGE_DISTRIBUTE_VALIDATION_PAYLOAD_KIND,
                "digest": digest.as_str(),
                "source_machine": "machine-a",
                "target_machines": ["machine-b", "machine-b"],
                "platform": {
                    "os": "linux",
                    "architecture": "amd64",
                    "variant": "v8"
                },
                "failure": {
                    "type": "duplicate_target",
                    "duplicate_target": "machine-b"
                }
            })),
        ))]);
        let response = ImageNodeClient::new(transport)
            .distribute(
                &source_machine,
                ImageDistributeRequest {
                    digest: digest.clone(),
                    source_machine: source_machine.clone(),
                    target_machines: vec![target_machine.clone()],
                    platform: None,
                },
            )
            .await
            .expect("remote image response");
        assert!(!response.is_ok());
        assert_eq!(response.code(), "IMAGE_DISTRIBUTE_DUPLICATE_TARGET");
        assert_eq!(response.message(), "duplicate target");
        let Some(ImageNodePayload::DistributeValidation(payload)) = response.into_payload() else {
            panic!("expected distribute validation payload");
        };
        assert_eq!(payload.digest, digest.clone());
        assert_eq!(payload.source_machine, source_machine.clone());
        assert_eq!(
            payload.target_machines.as_slice(),
            &[target_machine.clone(), target_machine.clone()]
        );
        assert_eq!(payload.platform.as_ref(), Some(&platform));
        assert!(matches!(
            payload.failure,
            ImageDistributeValidationFailure::DuplicateTarget { duplicate_target }
                if duplicate_target == target_machine
        ));

        let transport = FakeImageTransport::with_responses(vec![Err(NodeRpcError::transport(
            "image_receive_session",
            "NATS_RPC_TIMEOUT",
            "timed out",
        ))]);
        let error = ImageNodeClient::new(transport)
            .receive_session(
                &target_machine,
                ImageReceiveSessionRequest {
                    operation_id: "image-push-1".into(),
                    source_machine: MachineId::new("machine-a"),
                    repository: None,
                },
            )
            .await
            .expect_err("transport error should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Transport);
        assert_eq!(error.operation, "image_receive_session");

        let transport = FakeImageTransport::with_responses(vec![Ok(NodeResponse::success(
            "ok",
            Some(serde_json::json!({
                "kind": IMAGE_RECEIVE_SESSION_PAYLOAD_KIND,
                "target_machine": "machine-b"
            })),
        ))]);
        let error = ImageNodeClient::new(transport)
            .receive_session(
                &target_machine,
                ImageReceiveSessionRequest {
                    operation_id: "image-push-1".into(),
                    source_machine: MachineId::new("machine-a"),
                    repository: None,
                },
            )
            .await
            .expect_err("invalid payload fields should fail");
        assert_eq!(error.kind, NodeRpcErrorKind::Decode);
        assert_eq!(error.operation, "image_receive_session");
    }

    #[tokio::test]
    async fn image_node_client_maps_payload_discriminator_errors_and_decodes_import_payload() {
        let digest = digest();
        let target_machine = MachineId::new("machine-b");
        let source_machine = MachineId::new("machine-a");

        for payload in [
            serde_json::json!({
                "target_machine": "machine-b",
                "endpoint": "http://127.0.0.1:4320/v2/ployz/image-push-1",
                "token": "token-1",
                "expires_at_unix_secs": 1_777_646_000_u64,
                "headers": {}
            }),
            serde_json::json!({
                "kind": "wrong-image-payload",
                "target_machine": "machine-b",
                "endpoint": "http://127.0.0.1:4320/v2/ployz/image-push-1",
                "token": "token-1",
                "expires_at_unix_secs": 1_777_646_000_u64,
                "headers": {}
            }),
        ] {
            let transport = FakeImageTransport::with_responses(vec![Ok(NodeResponse::success(
                "ok",
                Some(payload),
            ))]);
            let error = ImageNodeClient::new(transport)
                .receive_session(
                    &target_machine,
                    ImageReceiveSessionRequest {
                        operation_id: "image-push-1".into(),
                        source_machine: source_machine.clone(),
                        repository: None,
                    },
                )
                .await
                .expect_err("payload discriminator should fail");
            assert_eq!(error.kind, NodeRpcErrorKind::MissingPayload);
            assert_eq!(error.operation, "image_receive_session");
        }

        let record = image_record(target_machine.clone(), digest.clone());
        let transport = FakeImageTransport::with_responses(vec![Ok(NodeResponse::success(
            "imported",
            Some(serde_json::json!({
                "kind": IMAGE_RECEIVED_IMPORT_PAYLOAD_KIND,
                "target_machine": target_machine.as_str(),
                "record": record
            })),
        ))]);
        let response = ImageNodeClient::new(transport)
            .received_import(
                &target_machine,
                ImageReceivedImportRequest {
                    operation_id: "image-push-1".into(),
                    source_machine,
                    repository: "ployz/image-push-1".into(),
                    reference: "image-push-1".into(),
                    expected_digest: digest.clone(),
                    platform: None,
                    repo_tags: vec!["example/app:latest".into()],
                },
            )
            .await
            .expect("received import payload should decode");
        assert!(response.is_ok());
        let Some(ImageNodePayload::ReceivedImport(payload)) = response.into_payload() else {
            panic!("expected received import payload");
        };
        assert_eq!(payload.target_machine, target_machine);
        assert_eq!(payload.record.machine_id, target_machine);
        assert_eq!(payload.record.digest, digest);
    }

    fn default_response(operation: ImageRpcOperation) -> NodeResponse {
        let payload = match operation {
            ImageRpcOperation::ReceiveSession => serde_json::json!({
                "kind": IMAGE_RECEIVE_SESSION_PAYLOAD_KIND,
                "target_machine": "machine-b",
                "endpoint": "http://127.0.0.1:4320/v2/ployz/image-push-1",
                "token": "token-1",
                "expires_at_unix_secs": 1_777_646_000_u64,
                "headers": {}
            }),
            ImageRpcOperation::Distribute => serde_json::json!({
                "kind": IMAGE_DISTRIBUTE_PAYLOAD_KIND,
                "operation_id": "image-distribute-1",
                "digest": digest().as_str(),
                "source_machine": "machine-a",
                "targets": []
            }),
            ImageRpcOperation::ReceivedImport => {
                return NodeResponse::success("imported", None);
            }
        };
        NodeResponse::success("ok", Some(payload))
    }

    fn digest() -> ImageDigest {
        ImageDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("valid digest")
    }

    fn image_record(machine_id: MachineId, digest: ImageDigest) -> ImageAvailabilityRecord {
        ImageAvailabilityRecord {
            machine_id,
            digest: digest.clone(),
            presence: ImagePresence::Present {
                artifact: ImageArtifact {
                    image: ImageRef::digest_only(digest),
                    platform: None,
                    provenance: ImageArtifactProvenance::External {
                        source: Some("test".into()),
                    },
                    created_at: 1,
                },
                recorded_at: 2,
                source_operation_id: Some("image-push-1".into()),
            },
            updated_at: 3,
        }
    }
}
