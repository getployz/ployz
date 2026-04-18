use crate::transport::Transport;
use ployz_api::{
    CoordinationAbortRequest, CoordinationCommitPayload, CoordinationCommitRequest,
    CoordinationPreparePayload, CoordinationPrepareRequest, CoordinationRenewPayload,
    CoordinationRenewRequest, DaemonPayload, DaemonRequest, DaemonResponse, DeployOptions,
    MachineListPayload, MeshReadyPayload, MeshSelfRecordPayload, MeshStatusPayload,
    NodeStatusPayload, StatusPayload,
};
use ployz_types::model::{DeployApplyResult, DeployPreview};
use ployz_types::spec::DeployManifest;

pub struct DaemonClient<T> {
    transport: T,
}

impl<T> DaemonClient<T> {
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    #[must_use]
    pub fn transport(&self) -> &T {
        &self.transport
    }
}

impl<T: Transport> DaemonClient<T> {
    pub async fn request(&self, request: DaemonRequest) -> std::io::Result<DaemonResponse> {
        self.transport.request(request).await
    }

    pub async fn request_ok(&self, request: DaemonRequest) -> std::io::Result<DaemonResponse> {
        let response = self.request(request).await?;
        if response.ok {
            return Ok(response);
        }
        Err(std::io::Error::other(format!(
            "daemon error [{}]: {}",
            response.code, response.message
        )))
    }

    pub async fn status(&self) -> std::io::Result<StatusPayload> {
        let response = self.request_ok(DaemonRequest::Status).await?;
        extract_payload(response, "status", |payload| match payload {
            DaemonPayload::Status(payload) => Some(payload),
            DaemonPayload::MeshStatus(_)
            | DaemonPayload::NodeStatus(_)
            | DaemonPayload::MachineList(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshReady(_)
            | DaemonPayload::MeshSelfRecord(_)
            | DaemonPayload::DeployPreview(_)
            | DaemonPayload::DeployApply(_)
            | DaemonPayload::DeployExport(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_)
            | DaemonPayload::CoordinationPrepare(_)
            | DaemonPayload::CoordinationRenew(_)
            | DaemonPayload::CoordinationCommit(_) => None,
        })
    }

    pub async fn mesh_status(&self, network: &str) -> std::io::Result<MeshStatusPayload> {
        let response = self
            .request_ok(DaemonRequest::MeshStatus {
                network: network.to_string(),
            })
            .await?;
        extract_payload(response, "mesh status", |payload| match payload {
            DaemonPayload::MeshStatus(payload) => Some(payload),
            DaemonPayload::Status(_)
            | DaemonPayload::NodeStatus(_)
            | DaemonPayload::MachineList(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshReady(_)
            | DaemonPayload::MeshSelfRecord(_)
            | DaemonPayload::DeployPreview(_)
            | DaemonPayload::DeployApply(_)
            | DaemonPayload::DeployExport(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_)
            | DaemonPayload::CoordinationPrepare(_)
            | DaemonPayload::CoordinationRenew(_)
            | DaemonPayload::CoordinationCommit(_) => None,
        })
    }

    pub async fn node_status(&self) -> std::io::Result<NodeStatusPayload> {
        let response = self.request_ok(DaemonRequest::NodeStatus).await?;
        extract_payload(response, "node status", |payload| match payload {
            DaemonPayload::NodeStatus(payload) => Some(payload),
            DaemonPayload::Status(_)
            | DaemonPayload::MeshStatus(_)
            | DaemonPayload::MachineList(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshReady(_)
            | DaemonPayload::MeshSelfRecord(_)
            | DaemonPayload::DeployPreview(_)
            | DaemonPayload::DeployApply(_)
            | DaemonPayload::DeployExport(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_)
            | DaemonPayload::CoordinationPrepare(_)
            | DaemonPayload::CoordinationRenew(_)
            | DaemonPayload::CoordinationCommit(_) => None,
        })
    }

    pub async fn machine_list(&self) -> std::io::Result<MachineListPayload> {
        let response = self.request_ok(DaemonRequest::MachineList).await?;
        extract_payload(response, "machine list", |payload| match payload {
            DaemonPayload::MachineList(payload) => Some(payload),
            DaemonPayload::Status(_)
            | DaemonPayload::NodeStatus(_)
            | DaemonPayload::MeshStatus(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshReady(_)
            | DaemonPayload::MeshSelfRecord(_)
            | DaemonPayload::DeployPreview(_)
            | DaemonPayload::DeployApply(_)
            | DaemonPayload::DeployExport(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_)
            | DaemonPayload::CoordinationPrepare(_)
            | DaemonPayload::CoordinationRenew(_)
            | DaemonPayload::CoordinationCommit(_) => None,
        })
    }

    pub async fn mesh_ready(&self) -> std::io::Result<MeshReadyPayload> {
        let response = self.request_ok(DaemonRequest::MeshReady).await?;
        extract_payload(response, "mesh ready", |payload| match payload {
            DaemonPayload::MeshReady(payload) => Some(payload),
            DaemonPayload::Status(_)
            | DaemonPayload::NodeStatus(_)
            | DaemonPayload::MeshStatus(_)
            | DaemonPayload::MachineList(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshSelfRecord(_)
            | DaemonPayload::DeployPreview(_)
            | DaemonPayload::DeployApply(_)
            | DaemonPayload::DeployExport(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_)
            | DaemonPayload::CoordinationPrepare(_)
            | DaemonPayload::CoordinationRenew(_)
            | DaemonPayload::CoordinationCommit(_) => None,
        })
    }

    pub async fn mesh_self_record(&self) -> std::io::Result<MeshSelfRecordPayload> {
        let response = self.request_ok(DaemonRequest::MeshSelfRecord).await?;
        extract_payload(response, "mesh self record", |payload| match payload {
            DaemonPayload::MeshSelfRecord(payload) => Some(payload),
            DaemonPayload::Status(_)
            | DaemonPayload::NodeStatus(_)
            | DaemonPayload::MeshStatus(_)
            | DaemonPayload::MachineList(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshReady(_)
            | DaemonPayload::DeployPreview(_)
            | DaemonPayload::DeployApply(_)
            | DaemonPayload::DeployExport(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_)
            | DaemonPayload::CoordinationPrepare(_)
            | DaemonPayload::CoordinationRenew(_)
            | DaemonPayload::CoordinationCommit(_) => None,
        })
    }

    pub async fn deploy_preview(
        &self,
        manifest: &DeployManifest,
        options: DeployOptions,
    ) -> std::io::Result<DeployPreview> {
        let manifest_json = encode_manifest(manifest, "deploy preview manifest")?;
        let response = self
            .request_ok(DaemonRequest::DeployPreview {
                manifest_json,
                options,
            })
            .await?;
        extract_payload(response, "deploy preview", |payload| match payload {
            DaemonPayload::DeployPreview(payload) => Some(payload.preview),
            DaemonPayload::Status(_)
            | DaemonPayload::NodeStatus(_)
            | DaemonPayload::MeshStatus(_)
            | DaemonPayload::MachineList(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshReady(_)
            | DaemonPayload::MeshSelfRecord(_)
            | DaemonPayload::DeployApply(_)
            | DaemonPayload::DeployExport(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_)
            | DaemonPayload::CoordinationPrepare(_)
            | DaemonPayload::CoordinationRenew(_)
            | DaemonPayload::CoordinationCommit(_) => None,
        })
    }

    pub async fn deploy_apply(
        &self,
        manifest: &DeployManifest,
        options: DeployOptions,
    ) -> std::io::Result<DeployApplyResult> {
        let manifest_json = encode_manifest(manifest, "deploy apply manifest")?;
        let response = self
            .request_ok(DaemonRequest::DeployApply {
                manifest_json,
                options,
            })
            .await?;
        extract_payload(response, "deploy apply", |payload| match payload {
            DaemonPayload::DeployApply(payload) => Some(payload.result),
            DaemonPayload::Status(_)
            | DaemonPayload::NodeStatus(_)
            | DaemonPayload::MeshStatus(_)
            | DaemonPayload::MachineList(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshReady(_)
            | DaemonPayload::MeshSelfRecord(_)
            | DaemonPayload::DeployPreview(_)
            | DaemonPayload::DeployExport(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_)
            | DaemonPayload::CoordinationPrepare(_)
            | DaemonPayload::CoordinationRenew(_)
            | DaemonPayload::CoordinationCommit(_) => None,
        })
    }

    pub async fn deploy_export_manifest(&self, namespace: &str) -> std::io::Result<DeployManifest> {
        let response = self
            .request_ok(DaemonRequest::DeployExport {
                namespace: namespace.to_string(),
            })
            .await?;
        extract_payload(response, "deploy export", |payload| match payload {
            DaemonPayload::DeployExport(payload) => Some(payload.manifest),
            DaemonPayload::Status(_)
            | DaemonPayload::NodeStatus(_)
            | DaemonPayload::MeshStatus(_)
            | DaemonPayload::MachineList(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshReady(_)
            | DaemonPayload::MeshSelfRecord(_)
            | DaemonPayload::DeployPreview(_)
            | DaemonPayload::DeployApply(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_)
            | DaemonPayload::CoordinationPrepare(_)
            | DaemonPayload::CoordinationRenew(_)
            | DaemonPayload::CoordinationCommit(_) => None,
        })
    }

    pub async fn coordination_prepare(
        &self,
        request: CoordinationPrepareRequest,
    ) -> std::io::Result<CoordinationPreparePayload> {
        let response = self
            .request(DaemonRequest::CoordinationPrepare {
                request: request.clone(),
            })
            .await?;
        extract_payload_or_error(response, "coordination prepare", |payload| match payload {
            DaemonPayload::CoordinationPrepare(payload) => Some(payload),
            DaemonPayload::Status(_)
            | DaemonPayload::NodeStatus(_)
            | DaemonPayload::MeshStatus(_)
            | DaemonPayload::MachineList(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshReady(_)
            | DaemonPayload::MeshSelfRecord(_)
            | DaemonPayload::DeployPreview(_)
            | DaemonPayload::DeployApply(_)
            | DaemonPayload::DeployExport(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_)
            | DaemonPayload::CoordinationRenew(_)
            | DaemonPayload::CoordinationCommit(_) => None,
        })
    }

    pub async fn coordination_commit(
        &self,
        request: CoordinationCommitRequest,
    ) -> std::io::Result<CoordinationCommitPayload> {
        let response = self
            .request(DaemonRequest::CoordinationCommit {
                request: request.clone(),
            })
            .await?;
        extract_payload_or_error(response, "coordination commit", |payload| match payload {
            DaemonPayload::CoordinationCommit(payload) => Some(payload),
            DaemonPayload::Status(_)
            | DaemonPayload::NodeStatus(_)
            | DaemonPayload::MeshStatus(_)
            | DaemonPayload::MachineList(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshReady(_)
            | DaemonPayload::MeshSelfRecord(_)
            | DaemonPayload::DeployPreview(_)
            | DaemonPayload::DeployApply(_)
            | DaemonPayload::DeployExport(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_)
            | DaemonPayload::CoordinationPrepare(_)
            | DaemonPayload::CoordinationRenew(_) => None,
        })
    }

    pub async fn coordination_renew(
        &self,
        request: CoordinationRenewRequest,
    ) -> std::io::Result<CoordinationRenewPayload> {
        let response = self
            .request(DaemonRequest::CoordinationRenew {
                request: request.clone(),
            })
            .await?;
        extract_payload_or_error(response, "coordination renew", |payload| match payload {
            DaemonPayload::CoordinationRenew(payload) => Some(payload),
            DaemonPayload::Status(_)
            | DaemonPayload::NodeStatus(_)
            | DaemonPayload::MeshStatus(_)
            | DaemonPayload::MachineList(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshReady(_)
            | DaemonPayload::MeshSelfRecord(_)
            | DaemonPayload::DeployPreview(_)
            | DaemonPayload::DeployApply(_)
            | DaemonPayload::DeployExport(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_)
            | DaemonPayload::CoordinationPrepare(_)
            | DaemonPayload::CoordinationCommit(_) => None,
        })
    }

    pub async fn coordination_abort(
        &self,
        request: CoordinationAbortRequest,
    ) -> std::io::Result<()> {
        self.request_ok(DaemonRequest::CoordinationAbort { request })
            .await
            .map(|_| ())
    }
}

fn extract_payload<T>(
    response: DaemonResponse,
    expected: &str,
    extract: impl FnOnce(DaemonPayload) -> Option<T>,
) -> std::io::Result<T> {
    let Some(payload) = response.payload else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("missing {expected} payload"),
        ));
    };

    extract(payload).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unexpected payload for {expected}"),
        )
    })
}

fn extract_payload_or_error<T>(
    response: DaemonResponse,
    expected: &str,
    extract: impl FnOnce(DaemonPayload) -> Option<T>,
) -> std::io::Result<T> {
    if response.ok {
        return extract_payload(response, expected, extract);
    }

    let Some(payload) = response.payload else {
        return Err(std::io::Error::other(format!(
            "daemon error [{}]: {}",
            response.code, response.message
        )));
    };

    if let Some(value) = extract(payload) {
        return Ok(value);
    }

    Err(std::io::Error::other(format!(
        "daemon error [{}]: {}",
        response.code, response.message
    )))
}

fn encode_manifest(manifest: &DeployManifest, label: &str) -> std::io::Result<String> {
    serde_json::to_string(manifest).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("failed to encode {label}: {error}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_api::{DeployApplyPayload, DeployPreviewPayload};
    use ployz_types::model::{DeployId, DeployState};
    use ployz_types::spec::{
        ContainerSpec, DeployManifest, Namespace, NetworkMode, Placement, PullPolicy, Resources,
        RestartPolicy, RolloutStrategy, ServiceSpec,
    };
    use std::future::{Future, ready};
    use std::sync::Mutex;

    struct StaticTransport {
        response: DaemonResponse,
        requests: Mutex<Vec<DaemonRequest>>,
    }

    fn deploy_manifest() -> DeployManifest {
        DeployManifest {
            namespace: Namespace("prod".into()),
            services: vec![ServiceSpec {
                name: "api".into(),
                placement: Placement::Global,
                template: ContainerSpec {
                    image: "nginx:latest".into(),
                    command: None,
                    entrypoint: None,
                    env: std::collections::BTreeMap::new(),
                    volumes: Vec::new(),
                    cap_add: Vec::new(),
                    cap_drop: Vec::new(),
                    privileged: false,
                    user: None,
                    pull_policy: PullPolicy::IfNotPresent,
                    resources: Resources::empty(),
                    sysctls: std::collections::BTreeMap::new(),
                },
                network: NetworkMode::Overlay,
                service_ports: Vec::new(),
                publish: Vec::new(),
                routes: Vec::new(),
                readiness: None,
                rollout: RolloutStrategy::Recreate,
                labels: std::collections::BTreeMap::new(),
                stop_grace_period: None,
                restart: RestartPolicy::UnlessStopped,
                pre_deploy: None,
            }],
        }
    }

    impl StaticTransport {
        fn new(response: DaemonResponse) -> Self {
            Self {
                response,
                requests: Mutex::new(Vec::new()),
            }
        }

        fn pop_request(&self) -> DaemonRequest {
            let mut requests = self.requests.lock().expect("lock requests");
            requests.pop().expect("captured request")
        }
    }

    impl Transport for StaticTransport {
        fn request(
            &self,
            request: DaemonRequest,
        ) -> impl Future<Output = std::io::Result<DaemonResponse>> + Send + '_ {
            self.requests.lock().expect("lock requests").push(request);
            ready(Ok(self.response.clone()))
        }
    }

    #[tokio::test]
    async fn status_extracts_payload() {
        let transport = StaticTransport::new(DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "status".into(),
            payload: Some(DaemonPayload::Status(StatusPayload {
                protocol_version: 1,
                daemon_version: "0.2.2".into(),
                machine_id: "founder".into(),
                active_network_name: Some("alpha".into()),
                phase: "running".into(),
                capabilities: vec!["status-payload-v1".into()],
            })),
        });
        let client = DaemonClient::new(&transport);

        let payload = client.status().await.expect("status payload");
        assert_eq!(payload.machine_id, "founder");
        assert_eq!(payload.active_network_name.as_deref(), Some("alpha"));

        let request = transport.pop_request();
        let DaemonRequest::Status = request else {
            panic!("unexpected request: {request:?}");
        };
    }

    #[tokio::test]
    async fn mesh_status_extracts_payload() {
        let transport = StaticTransport::new(DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "mesh status".into(),
            payload: Some(DaemonPayload::MeshStatus(MeshStatusPayload {
                network_name: "alpha".into(),
                network_id: String::new(),
                overlay: String::new(),
                state: "missing".into(),
                exists: false,
            })),
        });
        let client = DaemonClient::new(&transport);

        let payload = client
            .mesh_status("alpha")
            .await
            .expect("mesh status payload");
        assert_eq!(payload.network_name, "alpha");
        assert!(!payload.exists);

        let request = transport.pop_request();
        let DaemonRequest::MeshStatus { network } = request else {
            panic!("unexpected request: {request:?}");
        };
        assert_eq!(network, "alpha");
    }

    #[tokio::test]
    async fn status_errors_on_unexpected_payload() {
        let transport = StaticTransport::new(DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "wrong payload".into(),
            payload: Some(DaemonPayload::MeshStatus(MeshStatusPayload {
                network_name: "alpha".into(),
                network_id: "net-1".into(),
                overlay: "fd00::1".into(),
                state: "running".into(),
                exists: true,
            })),
        });
        let client = DaemonClient::new(&transport);

        let error = client.status().await.expect_err("unexpected payload error");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("unexpected payload for status"));
    }

    #[tokio::test]
    async fn request_ok_surfaces_daemon_error_code_and_message() {
        let transport = StaticTransport::new(DaemonResponse {
            ok: false,
            code: "MESH_DOWN".into(),
            message: "mesh is not running".into(),
            payload: None,
        });
        let client = DaemonClient::new(&transport);

        let error = client
            .request_ok(DaemonRequest::Status)
            .await
            .expect_err("daemon error should surface");
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert!(error.to_string().contains("MESH_DOWN"));
        assert!(error.to_string().contains("mesh is not running"));
    }

    #[tokio::test]
    async fn deploy_export_manifest_errors_on_missing_payload() {
        let transport = StaticTransport::new(DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "deploy export".into(),
            payload: None,
        });
        let client = DaemonClient::new(&transport);

        let error = client
            .deploy_export_manifest("prod")
            .await
            .expect_err("missing deploy export payload should fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("missing deploy export payload"));
    }

    #[tokio::test]
    async fn deploy_preview_extracts_payload() {
        let manifest = deploy_manifest();
        let preview = DeployPreview {
            namespace: manifest.namespace.clone(),
            manifest_hash: "hash-1".into(),
            participants: Vec::new(),
            services: Vec::new(),
            warnings: Vec::new(),
        };
        let transport = StaticTransport::new(DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "deploy preview".into(),
            payload: Some(DaemonPayload::DeployPreview(DeployPreviewPayload {
                preview: preview.clone(),
            })),
        });
        let client = DaemonClient::new(&transport);

        let payload = client
            .deploy_preview(&manifest, DeployOptions::default())
            .await
            .expect("deploy preview payload");
        assert_eq!(payload, preview);

        let request = transport.pop_request();
        let DaemonRequest::DeployPreview { manifest_json, .. } = request else {
            panic!("unexpected request: {request:?}");
        };
        let encoded: DeployManifest =
            serde_json::from_str(&manifest_json).expect("preview request manifest json");
        assert_eq!(encoded.namespace, manifest.namespace);
    }

    #[tokio::test]
    async fn deploy_apply_extracts_payload() {
        let manifest = deploy_manifest();
        let result = DeployApplyResult {
            deploy_id: DeployId("deploy-1".into()),
            preview: DeployPreview {
                namespace: manifest.namespace.clone(),
                manifest_hash: "hash-1".into(),
                participants: Vec::new(),
                services: Vec::new(),
                warnings: Vec::new(),
            },
            state: DeployState::Committed,
            events: Vec::new(),
        };
        let transport = StaticTransport::new(DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "deploy apply".into(),
            payload: Some(DaemonPayload::DeployApply(DeployApplyPayload {
                result: result.clone(),
            })),
        });
        let client = DaemonClient::new(&transport);

        let payload = client
            .deploy_apply(&manifest, DeployOptions::default())
            .await
            .expect("deploy apply payload");
        assert_eq!(payload, result);

        let request = transport.pop_request();
        let DaemonRequest::DeployApply { manifest_json, .. } = request else {
            panic!("unexpected request: {request:?}");
        };
        let encoded: DeployManifest =
            serde_json::from_str(&manifest_json).expect("apply request manifest json");
        assert_eq!(encoded.namespace, manifest.namespace);
    }

    #[tokio::test]
    async fn deploy_preview_errors_on_missing_payload() {
        let transport = StaticTransport::new(DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "deploy preview".into(),
            payload: None,
        });
        let client = DaemonClient::new(&transport);
        let manifest = deploy_manifest();

        let error = client
            .deploy_preview(&manifest, DeployOptions::default())
            .await
            .expect_err("missing deploy preview payload should fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("missing deploy preview payload"));
    }
}
