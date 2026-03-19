use crate::transport::Transport;
use ployz_api::{
    DaemonPayload, DaemonRequest, DaemonResponse, MachineListPayload, MeshReadyOutput,
    MeshReadyPayload, MeshSelfRecordPayload,
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

    pub async fn machine_list(&self) -> std::io::Result<MachineListPayload> {
        let response = self.request_ok(DaemonRequest::MachineList).await?;
        extract_payload(response, "machine list", |payload| match payload {
            DaemonPayload::MachineList(payload) => Some(payload),
            _ => None,
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
            _ => None,
        })
    }

    pub async fn mesh_self_record(&self) -> std::io::Result<MeshSelfRecordPayload> {
        let response = self.request_ok(DaemonRequest::MeshSelfRecord).await?;
        extract_payload(response, "mesh self record", |payload| match payload {
            DaemonPayload::MeshSelfRecord(payload) => Some(payload),
            _ => None,
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
