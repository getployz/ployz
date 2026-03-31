use crate::transport::Transport;
use ployz_api::{
    DaemonPayload, DaemonRequest, DaemonResponse, MachineListPayload, MeshReadyOutput,
    MeshReadyPayload, MeshSelfRecordPayload, MeshStatusPayload, StatusPayload,
};
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
            | DaemonPayload::MachineList(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshReady(_)
            | DaemonPayload::MeshSelfRecord(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_) => None,
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
            | DaemonPayload::MachineList(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshReady(_)
            | DaemonPayload::MeshSelfRecord(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_) => None,
        })
    }

    pub async fn machine_list(&self) -> std::io::Result<MachineListPayload> {
        let response = self.request_ok(DaemonRequest::MachineList).await?;
        extract_payload(response, "machine list", |payload| match payload {
            DaemonPayload::MachineList(payload) => Some(payload),
            DaemonPayload::Status(_)
            | DaemonPayload::MeshStatus(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshReady(_)
            | DaemonPayload::MeshSelfRecord(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_) => None,
        })
    }

    pub async fn mesh_ready(&self) -> std::io::Result<MeshReadyPayload> {
        let response = self
            .request_ok(DaemonRequest::MeshReady {
                output: MeshReadyOutput::Json,
            })
            .await?;
        extract_payload(response, "mesh ready", |payload| match payload {
            DaemonPayload::MeshReady(payload) => Some(payload),
            DaemonPayload::Status(_)
            | DaemonPayload::MeshStatus(_)
            | DaemonPayload::MachineList(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshSelfRecord(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_) => None,
        })
    }

    pub async fn mesh_self_record(&self) -> std::io::Result<MeshSelfRecordPayload> {
        let response = self.request_ok(DaemonRequest::MeshSelfRecord).await?;
        extract_payload(response, "mesh self record", |payload| match payload {
            DaemonPayload::MeshSelfRecord(payload) => Some(payload),
            DaemonPayload::Status(_)
            | DaemonPayload::MeshStatus(_)
            | DaemonPayload::MachineList(_)
            | DaemonPayload::MachineAdd(_)
            | DaemonPayload::MachineRemove(_)
            | DaemonPayload::MeshReady(_)
            | DaemonPayload::MachineOperationList(_)
            | DaemonPayload::MachineOperation(_) => None,
        })
    }

    pub async fn deploy_export_manifest(&self, namespace: &str) -> std::io::Result<DeployManifest> {
        let response = self
            .request_ok(DaemonRequest::DeployExport {
                namespace: namespace.to_string(),
            })
            .await?;
        serde_json::from_str(&response.message).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to decode exported deploy manifest: {error}"),
            )
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::{Future, ready};
    use std::sync::Mutex;

    struct StaticTransport {
        response: DaemonResponse,
        requests: Mutex<Vec<DaemonRequest>>,
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
    async fn deploy_export_manifest_errors_on_invalid_json() {
        let transport = StaticTransport::new(DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "{not-json}".into(),
            payload: None,
        });
        let client = DaemonClient::new(&transport);

        let error = client
            .deploy_export_manifest("prod")
            .await
            .expect_err("invalid manifest json should fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains("failed to decode exported deploy manifest")
        );
    }
}
