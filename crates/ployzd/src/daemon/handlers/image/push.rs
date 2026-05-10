use std::collections::BTreeMap;
use std::net::SocketAddr;

use ployz_api::{
    DaemonPayload, ImageDistributeRequest, ImagePushRequest, ImageReceiveSessionPayload,
    ImageReceiveSessionRequest,
};
use ployz_store_api::MachineMembershipStore;

use crate::daemon::DaemonState;
use crate::daemon::handlers::image::registry::{
    REGISTRY_OPERATION_HEADER, REGISTRY_SESSION_HEADER, REGISTRY_SOURCE_MACHINE_HEADER,
    validate_repository,
};

impl DaemonState {
    pub(crate) async fn handle_image_push(
        &self,
        request: &ImagePushRequest,
    ) -> ployz_api::DaemonResponse {
        let _ = request;
        self.err(
            "IMAGE_PUSH_UNIMPLEMENTED",
            "image push transport is not implemented yet",
        )
    }

    pub(crate) async fn handle_image_distribute(
        &self,
        request: &ImageDistributeRequest,
    ) -> ployz_api::DaemonResponse {
        let _ = request;
        self.err(
            "IMAGE_DISTRIBUTE_UNIMPLEMENTED",
            "image distribute transport is not implemented yet",
        )
    }

    pub(crate) async fn handle_image_receive_session(
        &self,
        request: &ImageReceiveSessionRequest,
    ) -> ployz_api::DaemonResponse {
        let active = match self.require_active(
            "IMAGE_RECEIVER_INACTIVE",
            "image receive session requires a running mesh",
        ) {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let Some(bind_addr) = active.image_receiver_bind_addr else {
            return self.err(
                "IMAGE_RECEIVER_INACTIVE",
                "image receiver listener is not running",
            );
        };
        let repository = request
            .repository
            .clone()
            .unwrap_or_else(|| default_receive_repository(&request.operation_id));
        if let Err(error) = validate_repository(&repository) {
            return self.err("IMAGE_RECEIVER_INVALID_REPOSITORY", error.to_string());
        }
        let machines = match active.mesh.store.list_machines().await {
            Ok(machines) => machines,
            Err(error) => {
                return self.err(
                    "IMAGE_RECEIVER_SOURCE_LOOKUP_FAILED",
                    format!("list machines for image receive source validation: {error}"),
                );
            }
        };
        if !machines
            .iter()
            .any(|machine| machine.id == request.source_machine)
        {
            return self.err(
                "IMAGE_RECEIVER_SOURCE_UNKNOWN",
                format!(
                    "source machine '{}' is not a cluster member",
                    request.source_machine
                ),
            );
        }

        let session = self
            .image_registry
            .register_session(
                &request.operation_id,
                request.source_machine.clone(),
                repository.clone(),
            )
            .await;
        let mut headers = BTreeMap::new();
        headers.insert(
            REGISTRY_OPERATION_HEADER.to_string(),
            session.operation_id.clone(),
        );
        headers.insert(
            REGISTRY_SOURCE_MACHINE_HEADER.to_string(),
            session.source_machine.0.clone(),
        );
        headers.insert(REGISTRY_SESSION_HEADER.to_string(), session.token.clone());
        let payload = ImageReceiveSessionPayload {
            target_machine: self.identity.machine_id.clone(),
            endpoint: receiver_endpoint(bind_addr, &session.repository),
            token: session.token,
            expires_at_unix_secs: session.expires_at_unix_secs,
            headers,
        };

        self.ok_with_payload(
            "image receive session created",
            Some(DaemonPayload::ImageReceiveSession(payload)),
        )
    }
}

fn receiver_endpoint(bind_addr: SocketAddr, repository: &str) -> String {
    format!("http://{bind_addr}/v2/{repository}")
}

fn default_receive_repository(operation_id: &str) -> String {
    let mut segment = String::with_capacity(operation_id.len().max(1));
    for ch in operation_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            segment.push(ch);
        } else {
            segment.push('-');
        }
    }
    if segment.is_empty() {
        segment.push_str("session");
    }
    format!("ployz/{segment}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use ployz_api::DaemonPayload;
    use ployz_orchestrator::{Mesh, WireguardDriver};
    use ployz_runtime_api::Identity;
    use ployz_store_api::StoreDriver;
    use ployz_types::model::{
        ImageDigest, MachineId, MachineMembership, NetworkLifecycle, NetworkName, OverlayIp,
        PublicKey,
    };
    use sha2::{Digest as _, Sha256};
    use tower::ServiceExt as _;

    use crate::daemon::{ActiveMesh, RetainedSubnet};
    use crate::mesh_state::network::NetworkConfig;

    #[tokio::test]
    async fn image_push_handler_returns_explicit_unimplemented_code() {
        let state = make_state();
        let response = state
            .handle_image_push(&ImagePushRequest {
                source_image: "example/app:latest".into(),
                target_machines: vec![MachineId("machine-a".into())],
                platform: None,
                expected_digest: None,
            })
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_PUSH_UNIMPLEMENTED");
    }

    #[tokio::test]
    async fn image_distribute_handler_returns_explicit_unimplemented_code() {
        let state = make_state();
        let response = state
            .handle_image_distribute(&ImageDistributeRequest {
                digest: ImageDigest::try_new(format!("sha256:{}", "a".repeat(64)))
                    .expect("valid digest"),
                source_machine: MachineId("machine-a".into()),
                target_machines: vec![MachineId("machine-b".into())],
                platform: None,
            })
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_DISTRIBUTE_UNIMPLEMENTED");
    }

    #[tokio::test]
    async fn image_receive_session_requires_active_mesh() {
        let state = make_state();
        let response = state
            .handle_image_receive_session(&ImageReceiveSessionRequest {
                operation_id: "image-push-1".into(),
                source_machine: MachineId("machine-a".into()),
                repository: Some("ployz/image-push-1".into()),
            })
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_RECEIVER_INACTIVE");
    }

    #[tokio::test]
    async fn image_receive_session_returns_endpoint_token_and_headers() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;

        let response = state
            .handle_image_receive_session(&ImageReceiveSessionRequest {
                operation_id: "image-push-1".into(),
                source_machine: MachineId("machine-a".into()),
                repository: Some("ployz/image-push-1".into()),
            })
            .await;

        assert!(response.ok, "{response:?}");
        let Some(DaemonPayload::ImageReceiveSession(payload)) = response.payload else {
            panic!("expected image receive session payload");
        };
        assert_eq!(payload.target_machine, MachineId("founder".into()));
        assert_eq!(
            payload.endpoint,
            "http://127.0.0.1:4320/v2/ployz/image-push-1"
        );
        assert_eq!(
            payload
                .headers
                .get(REGISTRY_OPERATION_HEADER)
                .map(String::as_str),
            Some("image-push-1")
        );
        assert_eq!(
            payload
                .headers
                .get(REGISTRY_SOURCE_MACHINE_HEADER)
                .map(String::as_str),
            Some("machine-a")
        );
        assert_eq!(
            payload
                .headers
                .get(REGISTRY_SESSION_HEADER)
                .map(String::as_str),
            Some(payload.token.as_str())
        );
        assert!(payload.expires_at_unix_secs > 0);
    }

    #[tokio::test]
    async fn image_receive_session_rejects_unknown_source_machine() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;

        let response = state
            .handle_image_receive_session(&ImageReceiveSessionRequest {
                operation_id: "image-push-1".into(),
                source_machine: MachineId("unknown".into()),
                repository: Some("ployz/image-push-1".into()),
            })
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_RECEIVER_SOURCE_UNKNOWN");
    }

    #[tokio::test]
    async fn image_receive_session_token_authorizes_registry_upload() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let response = state
            .handle_image_receive_session(&ImageReceiveSessionRequest {
                operation_id: "image-push-1".into(),
                source_machine: MachineId("machine-a".into()),
                repository: Some("ployz/image-push-1".into()),
            })
            .await;
        let Some(DaemonPayload::ImageReceiveSession(payload)) = response.payload else {
            panic!("expected image receive session payload");
        };
        let digest = test_sha256_digest(b"hello");
        let router = state.image_registry.clone().router();
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!(
                "/v2/ployz/image-push-1/blobs/uploads/?digest={digest}"
            ))
            .header(
                REGISTRY_OPERATION_HEADER,
                payload.headers[REGISTRY_OPERATION_HEADER].as_str(),
            )
            .header(
                REGISTRY_SOURCE_MACHINE_HEADER,
                payload.headers[REGISTRY_SOURCE_MACHINE_HEADER].as_str(),
            )
            .header(
                REGISTRY_SESSION_HEADER,
                payload.headers[REGISTRY_SESSION_HEADER].as_str(),
            )
            .body(Body::from("hello"))
            .expect("request");

        let response = router.oneshot(request).await.expect("registry response");

        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn image_receive_session_token_is_scoped_to_repository() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let response = state
            .handle_image_receive_session(&ImageReceiveSessionRequest {
                operation_id: "image-push-1".into(),
                source_machine: MachineId("machine-a".into()),
                repository: Some("ployz/image-push-1".into()),
            })
            .await;
        let Some(DaemonPayload::ImageReceiveSession(payload)) = response.payload else {
            panic!("expected image receive session payload");
        };
        let digest = test_sha256_digest(b"hello");
        let router = state.image_registry.clone().router();
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("/v2/other/repo/blobs/uploads/?digest={digest}"))
            .header(
                REGISTRY_OPERATION_HEADER,
                payload.headers[REGISTRY_OPERATION_HEADER].as_str(),
            )
            .header(
                REGISTRY_SOURCE_MACHINE_HEADER,
                payload.headers[REGISTRY_SOURCE_MACHINE_HEADER].as_str(),
            )
            .header(
                REGISTRY_SESSION_HEADER,
                payload.headers[REGISTRY_SESSION_HEADER].as_str(),
            )
            .body(Body::from("hello"))
            .expect("request");

        let response = router.oneshot(request).await.expect("registry response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    fn make_state() -> DaemonState {
        let data_dir =
            std::env::temp_dir().join(format!("ployz-image-push-handler-{}", uuid::Uuid::new_v4()));
        let identity = Identity::generate(MachineId("founder".into()), [31; 32]);
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

    async fn install_active_mesh(state: &mut DaemonState) {
        let identity = Identity::generate(MachineId("founder".into()), [31; 32]);
        let mut config = NetworkConfig::new(
            NetworkName("alpha".into()),
            &identity.public_key,
            "10.210.0.0/16",
            "10.210.0.0/24".parse().expect("valid subnet"),
        );
        config.lifecycle = NetworkLifecycle::Running;
        let store = StoreDriver::memory();
        store
            .upsert_self_machine(&MachineMembership::seed(
                MachineId("machine-a".into()),
                PublicKey([12; 32]),
                OverlayIp("fd00::12".parse().expect("valid overlay")),
                None,
                Vec::new(),
            ))
            .await
            .expect("insert source machine");
        let mesh = Mesh::new(
            WireguardDriver::memory(),
            store,
            None,
            state.identity.machine_id.clone(),
            51820,
        );
        state.active = Some(ActiveMesh {
            retained_subnet: RetainedSubnet::from_running_config(config.subnet),
            config,
            mesh,
            nats_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            zfs_transfer: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            image_receiver: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            image_receiver_bind_addr: Some("127.0.0.1:4320".parse().expect("valid address")),
            gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            certificate_renewal: None,
            bootstrap_peer_seed: None,
        });
    }

    fn test_sha256_digest(body: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(body);
        format!("sha256:{:x}", hasher.finalize())
    }
}
