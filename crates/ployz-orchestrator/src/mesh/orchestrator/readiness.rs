use super::*;

#[derive(Debug, Clone, Copy)]
pub struct MeshReadyStatus {
    pub ready: bool,
    pub phase: Phase,
}

impl Mesh {
    pub async fn ready_status(&self) -> MeshReadyStatus {
        let phase = self.phase;
        let ready = phase == Phase::Running;

        MeshReadyStatus { ready, phase }
    }

    pub async fn detect_endpoints(&self) -> Result<Vec<String>> {
        self.endpoint_discovery
            .detect_endpoints(self.listen_port)
            .await
            .map_err(MeshError::Runtime)
    }
}
