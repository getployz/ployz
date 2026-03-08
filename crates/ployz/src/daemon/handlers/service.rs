use crate::daemon::DaemonState;
use crate::spec::ServiceSpec;
use crate::transport::DaemonResponse;
use crate::workload::runner::ServiceRunner;

impl DaemonState {
    fn overlay_network_name(&self) -> Option<String> {
        self.active
            .as_ref()
            .map(|a| format!("ployz-{}", a.config.name.0))
    }

    fn service_runner(&self) -> Result<ServiceRunner, DaemonResponse> {
        ServiceRunner::new(self.overlay_network_name())
            .map_err(|e| self.err("DOCKER_ERROR", format!("{e}")))
    }

    pub async fn handle_service_run(&self, spec_json: &str) -> DaemonResponse {
        let spec: ServiceSpec = match serde_json::from_str(spec_json) {
            Ok(s) => s,
            Err(e) => return self.err("INVALID_SPEC", format!("invalid service spec: {e}")),
        };

        let runner = match self.service_runner() {
            Ok(r) => r,
            Err(resp) => return resp,
        };

        match runner.run(&spec).await {
            Ok(container_name) => {
                let info = serde_json::json!({
                    "container": container_name,
                    "service": spec.name,
                    "namespace": spec.namespace.0,
                    "image": spec.container.image,
                });
                self.ok(format!("{info}"))
            }
            Err(e) => self.err("SERVICE_RUN_FAILED", format!("{e}")),
        }
    }

    pub async fn handle_service_list(&self) -> DaemonResponse {
        let runner = match self.service_runner() {
            Ok(r) => r,
            Err(resp) => return resp,
        };

        match runner.list().await {
            Ok(services) => {
                let json = serde_json::to_string_pretty(&services).unwrap_or_default();
                self.ok(json)
            }
            Err(e) => self.err("SERVICE_LIST_FAILED", format!("{e}")),
        }
    }

    pub async fn handle_service_remove(&self, name: &str, namespace: &str) -> DaemonResponse {
        let runner = match self.service_runner() {
            Ok(r) => r,
            Err(resp) => return resp,
        };

        match runner.remove(name, namespace).await {
            Ok(()) => self.ok(format!("service '{name}' removed")),
            Err(e) => self.err("SERVICE_REMOVE_FAILED", format!("{e}")),
        }
    }
}
