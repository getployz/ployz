use crate::daemon::DaemonState;
use crate::model::WorkloadId;
use crate::transport::DaemonResponse;

impl DaemonState {
    pub async fn handle_workload_create(&self, name: &str) -> DaemonResponse {
        let active = match &self.active {
            Some(a) => a,
            None => return self.err("NO_MESH", "no mesh is running"),
        };

        let wm = match &active.workload_manager {
            Some(wm) => wm,
            None => return self.err("NOT_SUPPORTED", "workloads require Docker mode"),
        };

        match wm.create(name).await {
            Ok(record) => {
                let info = serde_json::json!({
                    "id": record.id.0,
                    "machine_id": record.machine_id.0,
                    "overlay_ip": record.overlay_ip.to_string(),
                    "sidecar_container": record.sidecar_container,
                });
                self.ok(format!("workload created: {info}"))
            }
            Err(e) => self.err("WORKLOAD_CREATE_FAILED", format!("{e}")),
        }
    }

    pub async fn handle_workload_destroy(&self, name: &str) -> DaemonResponse {
        let active = match &self.active {
            Some(a) => a,
            None => return self.err("NO_MESH", "no mesh is running"),
        };

        let wm = match &active.workload_manager {
            Some(wm) => wm,
            None => return self.err("NOT_SUPPORTED", "workloads require Docker mode"),
        };

        let id = WorkloadId(name.to_string());
        match wm.destroy(&id).await {
            Ok(()) => self.ok(format!("workload '{name}' destroyed")),
            Err(e) => self.err("WORKLOAD_DESTROY_FAILED", format!("{e}")),
        }
    }

    pub async fn handle_workload_list(&self) -> DaemonResponse {
        let active = match &self.active {
            Some(a) => a,
            None => return self.err("NO_MESH", "no mesh is running"),
        };

        let wm = match &active.workload_manager {
            Some(wm) => wm,
            None => return self.err("NOT_SUPPORTED", "workloads require Docker mode"),
        };

        let workloads = wm.list().await;
        let items: Vec<serde_json::Value> = workloads
            .iter()
            .map(|w| {
                serde_json::json!({
                    "id": w.id.0,
                    "machine_id": w.machine_id.0,
                    "overlay_ip": w.overlay_ip.to_string(),
                    "sidecar_container": w.sidecar_container,
                })
            })
            .collect();

        let json = serde_json::to_string_pretty(&items).unwrap_or_default();
        self.ok(json)
    }
}
